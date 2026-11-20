//! Draft independent candidate overlay. This only establishes internal
//! consistency of caller-provided inputs; it does not authorize publication.
use super::candidate_revision_v2::{BaseKind, Item, Snapshot, snapshot_digest};
use super::strict::parse_change;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

fn closed(v: &Value, allowed: &[&str]) -> Result<(), &'static str> {
    let fields = v.as_object().ok_or("expected object")?;
    if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unsupported change field");
    }
    Ok(())
}
fn route(s: &str) -> bool {
    s.starts_with('/')
        && !s.starts_with("//")
        && !s.contains(['?', '#', '\\'])
        && !s.chars().any(char::is_control)
        && !s.split('/').any(|p| p == "." || p == "..")
}
fn inventory(items: &[Item]) -> Result<(), &'static str> {
    if items.len() > 100_000 {
        return Err("inventory exceeds item limit");
    }
    let (mut ids, mut paths) = (BTreeSet::new(), BTreeSet::new());
    let (mut bytes, mut links) = (0usize, 0usize);
    for item in items {
        bytes = bytes
            .checked_add(item.source.len())
            .ok_or("inventory too large")?;
        links = links
            .checked_add(item.links.as_ref().map_or(0, Vec::len))
            .ok_or("inventory too large")?;
        if bytes > 64 * 1024 * 1024 || links > 1_000_000 {
            return Err("inventory too large");
        }
        if item.entity_id.is_empty()
            || item.media_type.is_empty()
            || !route(&item.public_path)
            || item.revision.as_ref().is_some_and(String::is_empty)
            || format!("{:x}", Sha256::digest(item.source.as_bytes())) != item.sha256
            || item
                .links
                .as_ref()
                .is_some_and(|ls| ls.iter().any(|l| !route(l)))
            || !ids.insert(&item.entity_id)
            || !paths.insert(&item.public_path)
        {
            return Err("invalid or duplicate inventory item");
        }
    }
    Ok(())
}
fn state_matches(state: &Value, item: &Item) -> Result<(), &'static str> {
    closed(state, &["revision", "sha256", "inline", "file", "uri"])?;
    let source = state
        .get("inline")
        .and_then(Value::as_str)
        .ok_or("inline state required")?;
    if state.get("file").is_some()
        || state.get("uri").is_some()
        || source != item.source
        || state["sha256"] != item.sha256
        || state
            .get("revision")
            .is_some_and(|r| r.as_str() != item.revision.as_deref())
    {
        return Err("state and inventory differ");
    }
    Ok(())
}

/// Reconstruct the candidate from an independently acquired, host-supplied
/// base snapshot, v1 change, and after projections. Only inline content states
/// are supported. The complete flag is a host assertion, not evidence. The
/// caller must obtain snapshot inputs with duplicate-safe parsing and protect
/// their authenticity, freshness, and publication boundary independently.
pub fn overlay(
    base: &Snapshot,
    change_json: &[u8],
    after: &[Item],
) -> Result<Snapshot, &'static str> {
    let change = parse_change(change_json)?;
    closed(
        &change,
        &[
            "schema",
            "change_id",
            "created_at",
            "origin",
            "base",
            "content_root",
            "site",
            "author",
            "entries",
            "extensions",
        ],
    )?;
    if change.get("extensions").is_some() {
        return Err("unsupported change extensions");
    }
    closed(&change["origin"], &["kind", "system", "ref"])?;
    closed(&change["base"], &["kind", "revision"])?;
    if let Some(site) = change.get("site") {
        closed(site, &["id", "base_url"])?;
    }
    if let Some(author) = change.get("author") {
        closed(author, &["id", "display_name", "kind"])?;
    }
    let kind = match base.base.kind {
        BaseKind::Git => "git",
        BaseKind::Opaque => "opaque",
    };
    if base.scope.is_empty()
        || base.site_id.is_empty()
        || base.base.revision.is_empty()
        || !base.complete
        || change["base"]["kind"] != kind
        || change["base"]["revision"] != base.base.revision
        || (change["origin"]["kind"] == "git" && kind != "git")
    {
        return Err("incomplete or mismatched base");
    }
    if let Some(site) = change.get("site")
        && (site.get("id").is_some_and(|id| id != &base.site_id)
            || site
                .get("base_url")
                .is_some_and(|url| url.as_str() != base.site_base_url.as_deref()))
    {
        return Err("site mismatch");
    }
    inventory(&base.items)?;
    inventory(after)?;
    // Enforce profile bounds and IDs before building maps.
    snapshot_digest(base)?;
    let mut current: BTreeMap<&str, &Item> = base
        .items
        .iter()
        .map(|i| (i.entity_id.as_str(), i))
        .collect();
    let mut projected: BTreeMap<&str, &Item> =
        after.iter().map(|i| (i.entity_id.as_str(), i)).collect();
    let entries = change["entries"].as_array().ok_or("invalid entries")?;
    if entries.len() > 100_000 {
        return Err("too many changes");
    }
    let (mut targets, mut consumed) = (BTreeSet::new(), BTreeSet::new());
    for entry in entries {
        closed(
            entry,
            &[
                "entity_id",
                "operation",
                "previous_entity_id",
                "media_type",
                "public_path",
                "metadata",
                "before",
                "after",
            ],
        )?;
        let id = entry["entity_id"].as_str().ok_or("invalid entity")?;
        if !targets.insert(id) {
            return Err("duplicate change entity");
        }
        let op = entry["operation"].as_str().ok_or("invalid operation")?;
        if op == "create" {
            continue;
        }
        let old_id = if op == "rename" {
            entry["previous_entity_id"]
                .as_str()
                .ok_or("missing previous identity")?
        } else {
            id
        };
        if !consumed.insert(old_id) {
            return Err("old identity consumed twice");
        }
        let old = current.remove(old_id).ok_or("old identity unavailable")?;
        state_matches(entry.get("before").ok_or("before state unavailable")?, old)?;
        if (op == "delete") != entry.get("after").is_none() {
            return Err("invalid after state");
        }
    }
    for entry in entries {
        if entry["operation"] == "delete" {
            continue;
        }
        let id = entry["entity_id"].as_str().ok_or("invalid entity")?;
        let item = projected.remove(id).ok_or("after projection unavailable")?;
        state_matches(entry.get("after").ok_or("after state unavailable")?, item)?;
        if entry["media_type"] != item.media_type
            || entry
                .get("public_path")
                .is_some_and(|path| path != &item.public_path)
            || current.insert(id, item).is_some()
        {
            return Err("after projection mismatch");
        }
    }
    if !projected.is_empty() {
        return Err("unexpected after projection");
    }
    let mut candidate = base.clone();
    candidate.items = current.into_values().cloned().collect();
    inventory(&candidate.items)?;
    snapshot_digest(&candidate)?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn fixture() -> (Snapshot, Vec<Item>, Value) {
        let v: Value = serde_json::from_str(include_str!("portable-verdict-vector.json")).unwrap();
        (
            serde_json::from_value(v["base"].clone()).unwrap(),
            serde_json::from_value(v["after"].clone()).unwrap(),
            serde_json::from_str(v["artifacts"]["change"].as_str().unwrap()).unwrap(),
        )
    }
    fn apply(base: &Snapshot, after: &[Item], change: &Value) -> Result<Snapshot, &'static str> {
        overlay(base, change.to_string().as_bytes(), after)
    }
    fn state(item: &Item) -> Value {
        json!({"sha256":item.sha256, "inline":item.source})
    }
    #[test]
    fn creates_candidate_matching_shared_aexeo_vector() {
        let (base, after, change) = fixture();
        let candidate = apply(&base, &after, &change).unwrap();
        let digest = snapshot_digest(&candidate).unwrap();
        assert_eq!(
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            "5b3bbd7e90693296330978fdd708f309cc3c52bac6a8f17f450567d472b1c8a2"
        );
    }
    #[test]
    fn update_rename_delete_and_route_swaps() {
        let (mut base, after, mut change) = fixture();
        let old = after[0].clone();
        base.items.push(old.clone());
        let mut new = old.clone();
        new.source = "new".into();
        new.sha256 = format!("{:x}", Sha256::digest(b"new"));
        change["entries"][0] = json!({"entity_id":"article","operation":"update","media_type":"text/plain","before":state(&old),"after":state(&new)});
        assert_eq!(
            apply(&base, &[new.clone()], &change).unwrap().items[0].source,
            "new"
        );
        new.entity_id = "renamed".into();
        change["entries"][0] = json!({"entity_id":"renamed","previous_entity_id":"article","operation":"rename","media_type":"text/plain","before":state(&old),"after":state(&new)});
        assert_eq!(
            apply(&base, &[new], &change).unwrap().items[0].entity_id,
            "renamed"
        );
        change["entries"][0] = json!({"entity_id":"article","operation":"delete","media_type":"text/plain","before":state(&old)});
        assert!(apply(&base, &[], &change).unwrap().items.is_empty());
        let mut second = old.clone();
        second.entity_id = "second".into();
        second.public_path = "/second".into();
        base.items.push(second.clone());
        let mut swapped_first = old.clone();
        swapped_first.public_path = second.public_path.clone();
        let mut swapped_second = second.clone();
        swapped_second.public_path = old.public_path.clone();
        change["entries"] = json!([
            {"entity_id":"article","operation":"update","media_type":"text/plain","before":state(&old),"after":state(&swapped_first)},
            {"entity_id":"second","operation":"update","media_type":"text/plain","before":state(&second),"after":state(&swapped_second)}
        ]);
        assert!(apply(&base, &[swapped_first, swapped_second], &change).is_ok());
    }
    #[test]
    fn rejects_ambiguous_stale_or_incomplete_inputs() {
        let (base, after, change) = fixture();
        let mut b = base.clone();
        b.complete = false;
        assert!(apply(&b, &after, &change).is_err());
        b = base.clone();
        b.base.revision = "stale".into();
        assert!(apply(&b, &after, &change).is_err());
        let mut bad_after = after.clone();
        bad_after[0].source.push('x');
        assert!(apply(&base, &bad_after, &change).is_err());
        assert!(apply(&base, &[], &change).is_err());
        assert!(apply(&base, &[after[0].clone(), after[0].clone()], &change).is_err());
        let mut c = change.clone();
        c["entries"][0]["after"]["inline"] = json!("tampered");
        assert!(apply(&base, &after, &c).is_err());
        c = change.clone();
        c["entries"][0]["unknown"] = json!(true);
        assert!(apply(&base, &after, &c).is_err());
        c = change.clone();
        c["extensions"] = json!({});
        assert!(apply(&base, &after, &c).is_err());
        c = change.clone();
        c["entries"][0]["after"] = json!({"file":"article.txt","sha256":after[0].sha256});
        assert!(apply(&base, &after, &c).is_err());
        c = change.clone();
        let duplicate = c["entries"][0].clone();
        c["entries"].as_array_mut().unwrap().push(duplicate);
        assert!(apply(&base, &after, &c).is_err());
        let text = change.to_string().replacen(
            "\"schema\":",
            "\"schema\":\"content-gate.change/v1\",\"schema\":",
            1,
        );
        assert!(overlay(&base, text.as_bytes(), &after).is_err());
    }
}
