//! Stateless revisions for bounded status snapshots. Revisions describe the
//! observable sections, not the underlying database or external state.

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

const SECTIONS: &[&str] = &[
    "summary",
    "data_freshness",
    "advice",
    "waiters",
    "session_contexts",
    "collaboration_fence",
    "lease_release_requests",
    "coordinated_operations",
    "focus",
    "omitted_details",
    "drilldown",
];

fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

/// Returns a current snapshot, or section replacements against a valid prior
/// revision. A missing, malformed, or differently scoped token falls back to a
/// full snapshot; it must never be interpreted as an unchanged observation.
pub(super) fn render(
    mut snapshot: Value,
    root: &Path,
    session: Option<i64>,
    since: Option<&str>,
) -> Result<Value, serde_json::Error> {
    let scope = digest(format!("{}\0{:?}", root.display(), session).as_bytes());
    let mut hashes = Vec::with_capacity(SECTIONS.len());
    for key in SECTIONS {
        hashes.push(digest(&serde_json::to_vec(
            &snapshot.get(*key).unwrap_or(&Value::Null),
        )?));
    }
    let revision = format!("v1:{scope}:{}", hashes.join(":"));
    let prior = since.and_then(|token| {
        if token.len() > 1024 {
            return None;
        }
        let fields: Vec<&str> = token.split(':').collect();
        if fields.len() != SECTIONS.len() + 2
            || fields[0] != "v1"
            || fields[1] != scope
            || fields[2..].iter().any(|hash| {
                hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
        {
            return None;
        }
        Some(fields)
    });
    if let Some(prior) = prior {
        let mut changed = Map::new();
        for (index, key) in SECTIONS.iter().enumerate() {
            if prior[index + 2] != hashes[index] {
                changed.insert(
                    (*key).into(),
                    snapshot.get(*key).cloned().unwrap_or(Value::Null),
                );
            }
        }
        return Ok(json!({
            "schema": "aethyme.broker-status-delta/v1",
            "revision": revision,
            "changed_sections": changed,
            "data_freshness": snapshot.get("data_freshness").cloned().unwrap_or(Value::Null),
            "scope": { "session_id": session },
            "semantics": "Section replacements relative to the supplied revision; deferred checks remain unknown.",
        }));
    }
    if let Some(object) = snapshot.as_object_mut() {
        object.insert("revision".into(), Value::String(revision));
        if since.is_some() {
            object.insert("revision_reset".into(), Value::Bool(true));
        }
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::render;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn revision_reports_section_replacements_and_falls_back_across_scope() {
        let first = render(
            json!({"summary": {"count": 1}, "data_freshness": {"unknown": true}}),
            Path::new("/one"),
            None,
            None,
        )
        .unwrap();
        let revision = first["revision"].as_str().unwrap();
        let same = render(
            json!({"summary": {"count": 1}, "data_freshness": {"unknown": true}}),
            Path::new("/one"),
            None,
            Some(revision),
        )
        .unwrap();
        assert!(same["changed_sections"].as_object().unwrap().is_empty());
        assert_eq!(same["data_freshness"]["unknown"], true);
        let next = render(
            json!({"summary": {"count": 2}, "data_freshness": {"unknown": true}}),
            Path::new("/one"),
            None,
            Some(revision),
        )
        .unwrap();
        assert_eq!(next["changed_sections"]["summary"]["count"], 2);
        let other = render(
            json!({"summary": {"count": 2}}),
            Path::new("/two"),
            None,
            Some(revision),
        )
        .unwrap();
        assert_eq!(other["revision_reset"], true);
        let focused = render(
            json!({"summary": {"count": 2}}),
            Path::new("/one"),
            Some(7),
            Some(revision),
        )
        .unwrap();
        assert_eq!(focused["revision_reset"], true);
        let invalid = render(
            json!({"summary": {"count": 2}}),
            Path::new("/one"),
            None,
            Some("bad"),
        )
        .unwrap();
        assert_eq!(invalid["revision_reset"], true);
    }
}
