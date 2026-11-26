//! Experimental offline v2 comparison, not a publication authorization.
//! The caller must independently acquire and protect every input, authenticate
//! the fresh evaluator, establish inventory completeness and freshness, and
//! synchronize the final check with publication.
use super::candidate_overlay_v2::overlay;
use super::candidate_revision_v2::{Item, Snapshot, snapshot_digest};
use super::portable_v2::{CandidateArtifacts, candidate_binding};
use super::strict::{DecisionInputs, parse_change, parse_unique, strict_precheck_v1};
use serde_json::{Value, json};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

#[path = "portable_host_v2.rs"]
mod portable_host_v2;

const MAX_ENVELOPE_BYTES: usize = 8 * 1024 * 1024;

/// Inputs independently acquired for a fresh offline evaluation. This draft
/// supports a host-provided collection and source-bound link observations.
/// The host must independently establish evidence provenance and freshness.
pub struct Inputs<'a> {
    pub base: &'a Snapshot,
    pub after: &'a [Item],
    pub change: &'a [u8],
    pub resolved_policy: &'a [u8],
    pub host_options: &'a [u8],
    pub collection: Option<&'a [u8]>,
    pub external_evidence: Option<&'a [u8]>,
    pub evaluator_identity: &'a [u8],
    pub expected_validator_name: &'a str,
    pub expected_validator_version: &'a str,
    pub required_checks: &'a [&'a str],
    pub fresh_exit_code: Option<i32>,
    pub fresh_timed_out: bool,
}

/// Matching experimental evidence only; it confers no publication authority.
#[derive(Debug, PartialEq, Eq)]
pub struct ExperimentalMatch {
    /// Number of declared gates whose scope was checked.
    pub policy_gate_count: usize,
    /// Number of expected item and collection checks.
    pub policy_check_count: usize,
}

fn closed(v: &Value, allowed: &[&str]) -> Result<(), &'static str> {
    if v.as_object()
        .ok_or("expected object")?
        .keys()
        .any(|k| !allowed.contains(&k.as_str()))
    {
        return Err("unsupported verdict field");
    }
    Ok(())
}
fn valid_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn closed_verdict(v: &Value) -> Result<(), &'static str> {
    closed(
        v,
        &[
            "schema",
            "status",
            "subject",
            "validator",
            "evaluated_at",
            "reason",
            "summary",
            "checks",
            "findings",
            "findings_omitted",
            "extensions",
        ],
    )?;
    closed(&v["subject"], &["change_id", "base", "entries"])?;
    closed(&v["subject"]["base"], &["kind", "revision"])?;
    for entry in v["subject"]["entries"]
        .as_array()
        .ok_or("invalid subject")?
    {
        closed(entry, &["entity_id", "sha256"])?;
    }
    closed(
        &v["validator"],
        &["name", "version", "profile", "profile_sha256"],
    )?;
    closed(
        &v["summary"],
        &[
            "checks",
            "passed",
            "failed",
            "not_checked",
            "blocking_failed",
            "blocking_not_checked",
        ],
    )?;
    for check in v["checks"].as_array().ok_or("invalid checks")? {
        closed(check, &["id", "status", "blocking", "reason"])?;
    }
    for finding in v["findings"].as_array().ok_or("invalid findings")? {
        closed(
            finding,
            &[
                "rule_id",
                "severity",
                "message",
                "entity_id",
                "check_id",
                "location",
                "subject",
                "suggestion",
            ],
        )?;
        if let Some(location) = finding.get("location") {
            closed(location, &["line", "column"])?;
        }
    }
    if let Some(ext) = v.get("extensions") {
        closed(
            ext,
            &[
                "aexeo.offline-input-binding.v1",
                "aexeo.observed-link-binding.v1",
                "aexeo.change-evidence-input-binding.v1",
                "aexeo.html-content-scope.v1",
            ],
        )?;
        let offline = ext.get("aexeo.offline-input-binding.v1");
        let observed = ext.get("aexeo.observed-link-binding.v1");
        let evidence = ext.get("aexeo.change-evidence-input-binding.v1");
        let valid_evidence = evidence.and_then(Value::as_str).is_some_and(valid_sha);
        let valid_offline = offline.and_then(Value::as_str).is_some_and(valid_sha);
        let valid_observed = observed.is_some_and(|value| {
            closed(value, &["inputs_sha256", "observations_sha256"]).is_ok()
                && value.as_object().is_some_and(|m| m.len() == 2)
                && ["inputs_sha256", "observations_sha256"]
                    .iter()
                    .all(|key| value[*key].as_str().is_some_and(valid_sha))
        });
        if (valid_offline as u8 + (valid_observed || valid_evidence) as u8) != 1
            || evidence.is_some() != valid_evidence
            || offline.is_some() != valid_offline
            || observed.is_some() != valid_observed
            || ext
                .get("aexeo.html-content-scope.v1")
                .is_some_and(|scope| scope.as_str() != Some("main"))
        {
            return Err("unsupported verdict extension");
        }
    }
    Ok(())
}
fn envelope(bytes: &[u8]) -> Result<Value, &'static str> {
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return Err("envelope exceeds size limit");
    }
    let value = parse_unique(bytes)?;
    closed(
        &value,
        &["schema", "binding_profile", "binding_sha256", "verdict"],
    )?;
    if value["schema"] != "content-gate.bound-verdict/v2"
        || value["binding_profile"] != "content-gate.candidate-binding/v1"
        || !value["binding_sha256"].as_str().is_some_and(valid_sha)
    {
        return Err("unsupported envelope or binding");
    }
    closed_verdict(&value["verdict"])?;
    Ok(value)
}

// Match the host projection to the reconstructed inventory and changed
// metadata. Text extraction remains a host assertion, not an observed fact.
fn validate_collection(
    bytes: &[u8],
    candidate: &Snapshot,
    change: &Value,
) -> Result<Value, &'static str> {
    let collection = parse_unique(bytes)?;
    closed(&collection, &["change_id", "items", "complete"])?;
    if collection["change_id"] != change["change_id"] {
        return Err("collection change differs");
    }
    let complete = collection["complete"]
        .as_bool()
        .ok_or("invalid collection completeness")?;
    let items = collection["items"]
        .as_array()
        .ok_or("invalid collection items")?;
    if items.len() > 100_000 {
        return Err("collection exceeds item limit");
    }
    let candidate_ids: BTreeSet<_> = candidate
        .items
        .iter()
        .map(|i| i.entity_id.as_str())
        .collect();
    let mut by_id = BTreeMap::new();
    for item in items {
        closed(
            item,
            &[
                "id",
                "text",
                "source_text",
                "headings",
                "metadata",
                "evidence_sources",
            ],
        )?;
        let id = item["id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or("invalid collection identity")?;
        if !candidate_ids.contains(id) || by_id.insert(id, item).is_some() {
            return Err("collection identities differ");
        }
        for key in ["text", "source_text"] {
            if item
                .get(key)
                .is_some_and(|v| !v.is_null() && !v.is_string())
            {
                return Err("invalid collection text");
            }
        }
        if item.get("headings").is_some_and(|v| {
            !v.is_null()
                && v.as_array()
                    .is_none_or(|a| a.iter().any(|x| !x.is_string()))
        }) {
            return Err("invalid collection headings");
        }
        for key in ["metadata", "evidence_sources"] {
            if item.get(key).is_some_and(|v| {
                !v.is_null()
                    && v.as_object()
                        .is_none_or(|m| m.values().any(|x| !x.is_string()))
            }) {
                return Err("invalid collection metadata");
            }
        }
    }
    if complete && by_id.len() != candidate_ids.len() {
        return Err("complete collection identities differ");
    }
    for entry in change["entries"].as_array().ok_or("invalid entries")? {
        if entry["operation"] != "delete" {
            let item = by_id
                .get(entry["entity_id"].as_str().ok_or("invalid entity")?)
                .ok_or("changed collection item unavailable")?;
            let metadata = |v: &Value| v.get("metadata").filter(|v| !v.is_null()).cloned();
            if metadata(item) != metadata(entry) {
                return Err("collection changed metadata differs");
            }
        }
    }
    Ok(collection)
}

// Limits mirror the versioned Aexeo evaluator. The comparator adds a
// separate aggregate bound on work over distinct field/group pairs.
const COLLECTION_MAX_KEYS: usize = 10_000;
const COLLECTION_MAX_KEY_BYTES: usize = 4 * 1024 * 1024;
const COLLECTION_MAX_VALUE_BYTES: usize = 16 * 1024;
const COLLECTION_MAX_WORK: usize = 1_000_000;

fn collection_uniqueness_status(
    collection: &Value,
    field: &str,
    group: Option<&str>,
) -> Result<&'static str, &'static str> {
    if collection["complete"].as_bool() != Some(true) {
        return Ok("not_checked");
    }
    let items = collection["items"]
        .as_array()
        .ok_or("invalid collection items")?;
    let mut seen = BTreeSet::new();
    let mut key_bytes = 0usize;
    let mut duplicate = false;
    for item in items {
        let metadata = &item["metadata"];
        let Some(value) = metadata
            .get(field)
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
        else {
            return Ok("not_checked");
        };
        let category = match group {
            Some(name) => match metadata
                .get(name)
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty())
            {
                Some(value) => Some(value),
                None => return Ok("not_checked"),
            },
            None => None,
        };
        let category_bytes = category.map_or(0, str::len);
        if value.len() > COLLECTION_MAX_VALUE_BYTES || category_bytes > COLLECTION_MAX_VALUE_BYTES {
            return Ok("not_checked");
        }
        let key = (category, value);
        if seen.contains(&key) {
            duplicate = true;
        } else {
            key_bytes += value.len() + category_bytes;
            if seen.len() >= COLLECTION_MAX_KEYS || key_bytes > COLLECTION_MAX_KEY_BYTES {
                return Ok("not_checked");
            }
            seen.insert(key);
        }
    }
    Ok(if duplicate { "failed" } else { "passed" })
}

fn verify_plain_count_statuses(
    policy: &Value,
    checks: &BTreeMap<&str, &Value>,
    after: &[Item],
    kind: &str,
    mismatch: &'static str,
    mut count: impl FnMut(&Value, &Item) -> Result<u64, &'static str>,
) -> Result<(), &'static str> {
    for gate in policy["gates"].as_array().ok_or("invalid gates")? {
        if gate["kind"] != kind {
            continue;
        }
        let gate_id = gate["id"].as_str().ok_or("invalid gate id")?;
        for item in after.iter().filter(|item| item.media_type == "text/plain") {
            let id = format!(
                "gate:{}:{}:{}",
                item.entity_id.len(),
                item.entity_id,
                gate_id
            );
            let check = checks
                .get(id.as_str())
                .ok_or("resolved policy check is missing")?;
            let status = check["status"].as_str().ok_or("invalid check status")?;
            if !matches!(status, "passed" | "failed") {
                continue;
            }
            let count = count(gate, item)?;
            let failed = gate["min"].as_u64().is_some_and(|min| count < min)
                || gate["max"].as_u64().is_some_and(|max| count > max);
            if status != if failed { "failed" } else { "passed" } {
                return Err(mismatch);
            }
        }
    }
    Ok(())
}

fn verify_plain_word_count_statuses(
    policy: &Value,
    checks: &BTreeMap<&str, &Value>,
    after: &[Item],
    counts: &mut BTreeMap<String, u64>,
) -> Result<(), &'static str> {
    verify_plain_count_statuses(
        policy,
        checks,
        after,
        "word_count",
        "plain-text word count status differs from source",
        |_, item| {
            Ok(*counts.entry(item.entity_id.clone()).or_insert_with(|| {
                u64::try_from(item.source.split_whitespace().count()).unwrap_or(u64::MAX)
            }))
        },
    )
}

fn verify_plain_character_count_statuses(
    policy: &Value,
    checks: &BTreeMap<&str, &Value>,
    after: &[Item],
    counts: &mut BTreeMap<(String, bool), u64>,
) -> Result<(), &'static str> {
    for gate in policy["gates"].as_array().ok_or("invalid gates")? {
        if gate["kind"] != "character_count" {
            continue;
        }
        let gate_id = gate["id"].as_str().ok_or("invalid gate id")?;
        let ignore = gate["ignore_whitespace"]
            .as_bool()
            .ok_or("invalid whitespace option")?;
        for item in after.iter().filter(|item| item.media_type == "text/plain") {
            let id = format!(
                "gate:{}:{}:{}",
                item.entity_id.len(),
                item.entity_id,
                gate_id
            );
            let check = checks
                .get(id.as_str())
                .ok_or("resolved policy check is missing")?;
            let status = check["status"].as_str().ok_or("invalid check status")?;
            if !matches!(status, "passed" | "failed") {
                continue;
            }
            let count = *counts
                .entry((item.entity_id.clone(), ignore))
                .or_insert_with(|| {
                    u64::try_from(
                        item.source
                            .chars()
                            .filter(|c| !ignore || !c.is_whitespace())
                            .count(),
                    )
                    .unwrap_or(u64::MAX)
                });
            let failed = gate["min"].as_u64().is_some_and(|min| count < min)
                || gate["max"].as_u64().is_some_and(|max| count > max);
            if status != if failed { "failed" } else { "passed" } {
                return Err("plain-text character count status differs from source");
            }
        }
    }
    Ok(())
}

// Final-newline checks inspect the exact source bytes, so no rich-text
// projection or additional host capability is required.
fn verify_final_newline_statuses(
    policy: &Value,
    checks: &BTreeMap<&str, &Value>,
    after: &[Item],
) -> Result<(), &'static str> {
    let _span = crate::phase_telemetry::span("content_gate_v2_final_newline_check");
    for gate in policy["gates"].as_array().ok_or("invalid gates")? {
        if gate["kind"] != "final_newline" {
            continue;
        }
        let gate_id = gate["id"].as_str().ok_or("invalid gate id")?;
        for item in after.iter().filter(|item| {
            matches!(
                item.media_type.as_str(),
                "text/plain" | "text/markdown" | "text/html"
            )
        }) {
            let id = format!(
                "gate:{}:{}:{}",
                item.entity_id.len(),
                item.entity_id,
                gate_id
            );
            let check = checks
                .get(id.as_str())
                .ok_or("resolved policy check is missing")?;
            let status = check["status"].as_str().ok_or("invalid check status")?;
            if !matches!(status, "passed" | "failed") {
                continue;
            }
            let expected = if item.source.is_empty() || item.source.ends_with('\n') {
                "passed"
            } else {
                "failed"
            };
            if status != expected {
                return Err("final newline status differs from source");
            }
        }
    }
    Ok(())
}

fn plain_text_paragraph_count(source: &str) -> Result<u64, &'static str> {
    let mut count = 0;
    let mut inside = false;
    for line in source.split_inclusive('\n') {
        let blank = line.trim_end_matches(['\r', '\n']).trim().is_empty();
        if !blank && !inside {
            count += 1;
            if count > 100_000 {
                return Err("plain-text paragraph count exceeds evaluator limit");
            }
        }
        inside = !blank;
    }
    Ok(count)
}

fn verify_plain_paragraph_count_statuses(
    policy: &Value,
    checks: &BTreeMap<&str, &Value>,
    after: &[Item],
    counts: &mut BTreeMap<String, u64>,
) -> Result<(), &'static str> {
    verify_plain_count_statuses(
        policy,
        checks,
        after,
        "paragraph_count",
        "plain-text paragraph count status differs from source",
        |_, item| {
            if let Some(count) = counts.get(&item.entity_id) {
                return Ok(*count);
            }
            let count = plain_text_paragraph_count(&item.source)?;
            counts.insert(item.entity_id.clone(), count);
            Ok(count)
        },
    )
}

fn verify_collection_statuses(
    policy: &Value,
    collection: &Value,
    checks: &BTreeMap<&str, &Value>,
) -> Result<(), &'static str> {
    let gates = policy["gates"].as_array().ok_or("invalid resolved gates")?;
    let items = collection["items"]
        .as_array()
        .ok_or("invalid collection items")?;
    let mut cache = BTreeMap::new();
    let mut work = 0usize;
    for gate in gates
        .iter()
        .filter(|gate| gate["kind"] == "unique_collection_metadata")
    {
        let field = gate["field"]
            .as_str()
            .filter(|v| !v.trim().is_empty())
            .ok_or("invalid collection metadata fields")?;
        let group = match gate.get("group_by") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_str()
                    .filter(|v| !v.trim().is_empty())
                    .ok_or("invalid collection metadata fields")?,
            ),
        };
        let id = gate["id"].as_str().ok_or("invalid gate id")?;
        let check_id = format!("collection:0::{id}");
        let check = checks
            .get(check_id.as_str())
            .ok_or("resolved policy check is missing")?;
        let status = check["status"].as_str().ok_or("invalid check status")?;
        if status == "not_checked" {
            continue;
        }
        let actual = match cache.entry((field, group)) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                work = work
                    .checked_add(items.len())
                    .ok_or("collection comparison work limit exceeded")?;
                if work > COLLECTION_MAX_WORK {
                    return Err("collection comparison work limit exceeded");
                }
                *entry.insert(collection_uniqueness_status(collection, field, group)?)
            }
        };
        if actual == "not_checked" {
            return Err("collection gate cannot assess incomplete observations");
        }
        if matches!(status, "passed" | "failed") && status != actual {
            return Err("collection uniqueness status differs from observations");
        }
    }
    Ok(())
}

// Keep this scope list aligned with the versioned Aexeo resolved-policy format.
// Understanding scope does not validate gate parameters or authenticate results.
fn item_gate_kind(kind: &str) -> bool {
    matches!(
        kind,
        "editorial_pacing"
            | "literal_markdown_links"
            | "list_item_count"
            | "word_count"
            | "character_count"
            | "metadata_length"
            | "metadata_age"
            | "paragraph_count"
            | "paragraph_length"
            | "repeated_paragraph_openings"
            | "repeated_sentence_openings"
            | "sentence_opening_window"
            | "sentence_opening_terms"
            | "repeated_punctuation"
            | "trailing_ing_clause"
            | "similar_sentence_lengths"
            | "sentence_length_variance"
            | "prose_sentence_length_variance"
            | "inline_triple_candidates"
            | "duplicate_paragraphs"
            | "paragraph_character_length"
            | "link_count"
            | "link_destinations"
            | "link_reachability"
            | "sentence_length"
            | "sentence_character_length"
            | "repeated_terms"
            | "required_terms"
            | "forbidden_terms"
            | "required_metadata"
            | "required_headings"
            | "heading_length"
            | "heading_hierarchy"
            | "duplicate_headings"
            | "final_newline"
            | "evidence_anchors"
            | "structured_fact"
            | "evidence_review"
            | "evidence_review_age"
            | "custom"
            | "site_group"
            | "site_audit"
    )
}

fn policy_coverage(
    policy: &Value,
    change: &Value,
    has_collection: bool,
    checks: &BTreeMap<&str, &Value>,
) -> Result<(usize, usize), &'static str> {
    closed(policy, &["version", "name", "gates"])?;
    if policy["version"].as_u64() != Some(1) || policy["name"].as_str().is_none_or(str::is_empty) {
        return Err("unsupported resolved policy");
    }
    let gates = policy["gates"].as_array().ok_or("invalid resolved gates")?;
    if gates.is_empty() {
        return Err("empty resolved policy");
    }
    let entries = change["entries"].as_array().ok_or("invalid entries")?;
    let entities: Vec<&str> = entries
        .iter()
        .filter(|entry| entry.get("after").is_some_and(|v| !v.is_null()))
        .map(|entry| entry["entity_id"].as_str().ok_or("invalid entity"))
        .collect::<Result<_, _>>()?;
    let unsupported_item_media = entries.iter().any(|entry| {
        entry.get("after").is_some_and(|v| !v.is_null())
            && !matches!(
                entry["media_type"].as_str(),
                Some("text/plain" | "text/markdown" | "text/html")
            )
    });
    let mut ids = BTreeSet::new();
    let mut expected = 0usize;
    for gate in gates {
        let id = gate["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("invalid gate id")?;
        let kind = gate["kind"].as_str().ok_or("invalid gate kind")?;
        let blocking = gate["blocking"].as_bool().ok_or("invalid gate blocking")?;
        if !ids.insert(id) || (!item_gate_kind(kind) && kind != "unique_collection_metadata") {
            return Err("unsupported resolved gate");
        }
        if kind == "final_newline" {
            closed(gate, &["kind", "id", "severity", "blocking"])?;
            if !matches!(gate["severity"].as_str(), Some("warning" | "error")) {
                return Err("invalid resolved final newline gate");
            }
        }
        let collection = has_collection && kind == "unique_collection_metadata";
        if !collection && unsupported_item_media {
            return Err("item media type is not supported by this evaluator");
        }
        let candidates = if collection { 1 } else { entities.len() };
        expected = expected
            .checked_add(candidates)
            .ok_or("policy coverage exceeds limit")?;
        if expected > checks.len() {
            return Err("resolved policy check is missing");
        }
        if collection {
            let check_id = format!("collection:0::{id}");
            if checks
                .get(check_id.as_str())
                .is_none_or(|c| c["blocking"].as_bool() != Some(blocking))
            {
                return Err("resolved policy check is missing or has inconsistent blocking");
            }
        } else {
            for entity in &entities {
                let check_id = format!("gate:{}:{entity}:{id}", entity.len());
                let check = checks
                    .get(check_id.as_str())
                    .filter(|c| c["blocking"].as_bool() == Some(blocking))
                    .ok_or("resolved policy check is missing or has inconsistent blocking")?;
                if kind == "unique_collection_metadata"
                    && !has_collection
                    && check["status"] != "not_checked"
                {
                    return Err(
                        "candidate evaluator cannot execute collection gate without a collection",
                    );
                }
                if matches!(kind, "custom" | "site_group" | "site_audit")
                    && check["status"] != "not_checked"
                {
                    return Err("candidate evaluator cannot execute site or custom gates");
                }
            }
        }
    }
    if expected != checks.len() {
        return Err("unexpected check outside resolved policy");
    }
    Ok((gates.len(), expected))
}

// Validate the supported snapshot shape, but do not claim to authenticate the
// host's observations or independently re-extract destinations from prose.
fn validate_link_evidence(bytes: &[u8], change: &Value) -> Result<(), &'static str> {
    let snapshot = parse_unique(bytes)?;
    closed(&snapshot, &["version", "change_id", "entries"])?;
    if snapshot.as_object().is_none_or(|m| m.len() != 3)
        || snapshot["version"].as_u64() != Some(1)
        || snapshot["change_id"] != change["change_id"]
    {
        return Err("invalid link observation snapshot");
    }
    let entries = snapshot["entries"]
        .as_object()
        .ok_or("invalid link observation snapshot")?;
    let change_entries: BTreeMap<_, _> = change["entries"]
        .as_array()
        .ok_or("invalid change entries")?
        .iter()
        .map(|entry| {
            entry["entity_id"]
                .as_str()
                .map(|id| (id, entry))
                .ok_or("invalid entity")
        })
        .collect::<Result<_, _>>()?;
    for (id, item) in entries {
        closed(
            item,
            &["version", "source_sha256", "destinations", "observations"],
        )?;
        let entry = change_entries
            .get(id.as_str())
            .ok_or("invalid link observation snapshot entry")?;
        let destinations = item["destinations"]
            .as_array()
            .ok_or("invalid link observation snapshot entry")?;
        let observations = item["observations"]
            .as_array()
            .ok_or("invalid link observation snapshot entry")?;
        if item.as_object().is_none_or(|m| m.len() != 4)
            || item["version"].as_u64() != Some(1)
            || entry.get("after").is_none_or(Value::is_null)
            || !matches!(
                entry["media_type"].as_str(),
                Some("text/markdown" | "text/html")
            )
            || item["source_sha256"] != entry["after"]["sha256"]
            || destinations.len() != observations.len()
            || destinations.iter().any(|v| !v.is_string())
            || observations.iter().any(|v| {
                !matches!(
                    v.as_str(),
                    Some("reachable" | "not_found" | "not_checked" | "not_applicable")
                )
            })
        {
            return Err("invalid link observation snapshot entry");
        }
    }
    Ok(())
}

// Sources and links are host assertions, not authenticated facts.
fn candidate_evidence(
    bytes: &[u8],
    change: &Value,
    options: &[u8],
) -> Result<(Option<Value>, Option<Value>), &'static str> {
    let artifact = parse_unique(bytes)?;
    if artifact.get("schema").and_then(Value::as_str)
        != Some("aexeo.candidate-external-evidence/v1")
    {
        validate_link_evidence(bytes, change)?;
        return Ok((None, Some(artifact)));
    }
    closed(&artifact, &["schema", "sources", "links"])?;
    if artifact.as_object().is_none_or(|map| map.len() != 3) {
        return Err("invalid candidate evidence artifact");
    }
    let sources = artifact["sources"]
        .as_object()
        .ok_or("invalid evidence source map")?;
    let entities: BTreeSet<_> = change["entries"]
        .as_array()
        .ok_or("invalid entries")?
        .iter()
        .filter(|entry| entry.get("after").is_some_and(|value| !value.is_null()))
        .map(|entry| entry["entity_id"].as_str().ok_or("invalid entity"))
        .collect::<Result<_, _>>()?;
    if sources.len() != entities.len() || sources.keys().any(|key| !entities.contains(key.as_str()))
    {
        return Err("evidence inventory must match proposed entity identities");
    }
    let options = parse_unique(options)?;
    let max_entry = options["max_entry_bytes"]
        .as_u64()
        .ok_or("invalid evidence limits")?;
    let max_total = options["max_total_bytes"]
        .as_u64()
        .ok_or("invalid evidence limits")?;
    let mut total = 0u64;
    for (entity, values) in sources {
        total = total
            .checked_add(entity.len() as u64)
            .ok_or("evidence inventory exceeds byte limit")?;
        for (name, content) in values.as_object().ok_or("invalid evidence source map")? {
            let content = content.as_str().ok_or("invalid evidence source")?;
            if name.is_empty() || content.len() as u64 > max_entry {
                return Err("invalid evidence source or byte limit");
            }
            total = total
                .checked_add(name.len() as u64)
                .and_then(|value| value.checked_add(content.len() as u64))
                .ok_or("evidence inventory exceeds byte limit")?;
        }
        if total > max_total {
            return Err("evidence inventory exceeds byte limit");
        }
    }
    let links = (!artifact["links"].is_null()).then(|| artifact["links"].clone());
    if let Some(links) = &links {
        let encoded = serde_json::to_vec(links).map_err(|_| "invalid link observations")?;
        validate_link_evidence(&encoded, change)?;
    }
    Ok((Some(artifact["sources"].clone()), links))
}

#[cfg(test)]
fn evidence_capabilities(
    policy: &Value,
    change: &Value,
    checks: &BTreeMap<&str, &Value>,
    sources: Option<&Value>,
    links: Option<&Value>,
) -> Result<(), &'static str> {
    evidence_capabilities_with_mode(policy, change, checks, sources, links, false)
}

fn evidence_capabilities_with_mode(
    policy: &Value,
    change: &Value,
    checks: &BTreeMap<&str, &Value>,
    sources: Option<&Value>,
    links: Option<&Value>,
    plain_text_paragraphs: bool,
) -> Result<(), &'static str> {
    let entries = change["entries"].as_array().ok_or("invalid entries")?;
    let gates = policy["gates"].as_array().ok_or("invalid gates")?;
    for gate in gates {
        let kind = gate["kind"].as_str().ok_or("invalid gate kind")?;
        for entry in entries
            .iter()
            .filter(|entry| entry.get("after").is_some_and(|v| !v.is_null()))
        {
            let entity = entry["entity_id"].as_str().ok_or("invalid entity")?;
            let id = gate["id"].as_str().ok_or("invalid gate id")?;
            let check_id = format!("gate:{}:{entity}:{id}", entity.len());
            let Some(check) = checks.get(check_id.as_str()) else {
                continue;
            };
            let available = sources
                .and_then(|items| items.get(entity))
                .and_then(Value::as_object);
            let has = |field: &str| {
                gate[field]
                    .as_str()
                    .is_some_and(|key| available.is_some_and(|m| m.contains_key(key)))
            };
            let missing = match kind {
                "editorial_pacing" | "evidence_anchors" | "structured_fact" => !has("source"),
                "evidence_review" | "evidence_review_age" => !has("source") || !has("review"),
                "link_reachability" => links.and_then(|v| v["entries"].get(entity)).is_none(),
                "literal_markdown_links" => entry["media_type"] != "text/html",
                "paragraph_count"
                | "paragraph_length"
                | "paragraph_character_length"
                | "repeated_paragraph_openings"
                | "repeated_sentence_openings"
                | "sentence_opening_window"
                | "sentence_opening_terms"
                | "repeated_punctuation"
                | "trailing_ing_clause"
                | "similar_sentence_lengths"
                | "sentence_length_variance"
                | "inline_triple_candidates"
                | "duplicate_paragraphs" => {
                    entry["media_type"] == "text/plain" && !plain_text_paragraphs
                }
                "sentence_length" | "sentence_character_length" => {
                    gate["paragraphs_only"] == true
                        && entry["media_type"] == "text/plain"
                        && !plain_text_paragraphs
                }
                "required_metadata" | "metadata_length" | "metadata_age" => {
                    entry.get("metadata").is_none_or(Value::is_null)
                }
                "list_item_count"
                | "link_count"
                | "link_destinations"
                | "required_headings"
                | "heading_length"
                | "heading_hierarchy"
                | "duplicate_headings"
                | "prose_sentence_length_variance" => entry["media_type"] == "text/plain",
                _ => false,
            };
            if missing && check["status"] != "not_checked" {
                return Err("candidate gate cannot execute without required evidence");
            }
            if kind == "link_reachability"
                && let Some(item) = links.and_then(|v| v["entries"].get(entity))
            {
                let observations = item["observations"]
                    .as_array()
                    .ok_or("invalid link observations")?;
                if (observations.iter().any(|v| v == "not_checked")
                    && check["status"] != "not_checked")
                    || (observations.iter().any(|v| v == "not_found")
                        && check["status"] == "passed")
                {
                    return Err("link reachability status contradicts its observations");
                }
            }
        }
    }
    Ok(())
}

/// Compare an earlier and a freshly produced envelope against independently
/// reconstructed inputs. Passing is never permission to publish. In addition
/// to caller obligations above, snapshots must be duplicate-safe parsed. This
/// host must independently establish that any collection faithfully projects
/// the candidate and establish link observation provenance and freshness.
pub fn compare(
    previous: &[u8],
    fresh: &[u8],
    input: &Inputs<'_>,
) -> Result<ExperimentalMatch, &'static str> {
    if input.evaluator_identity.is_empty()
        || input.evaluator_identity.len() > 256
        || std::str::from_utf8(input.evaluator_identity).is_err()
        || input.expected_validator_name.is_empty()
        || input.expected_validator_version.is_empty()
    {
        return Err("invalid trusted evaluator identity");
    }
    let required: BTreeSet<_> = input.required_checks.iter().copied().collect();
    if required.is_empty() || required.len() != input.required_checks.len() || required.contains("")
    {
        return Err("nonempty distinct required checks needed");
    }
    let previous = envelope(previous)?;
    let fresh = envelope(fresh)?;
    let change = parse_change(input.change)?;
    let candidate = overlay(input.base, input.change, input.after)?;
    let policy = parse_unique(input.resolved_policy)?;
    let options = parse_unique(input.host_options)?;
    let plain_text_paragraphs = options["plain_text_paragraphs"]
        .as_bool()
        .ok_or("invalid host paragraph option")?;
    let collection = input
        .collection
        .map(|bytes| validate_collection(bytes, &candidate, &change))
        .transpose()?;
    let (sources, links) = match input.external_evidence {
        Some(bytes) => candidate_evidence(bytes, &change, input.host_options)?,
        None => (None, None),
    };
    let binding = candidate_binding(&CandidateArtifacts {
        change: input.change,
        resolved_policy: input.resolved_policy,
        host_options: input.host_options,
        evaluator: input.evaluator_identity,
        collection: input.collection,
        external_evidence: input.external_evidence,
        base_revision_sha256: snapshot_digest(input.base)?,
        candidate_revision_sha256: snapshot_digest(&candidate)?,
    })?;
    let binding_hex: String = binding.iter().map(|b| format!("{b:02x}")).collect();
    let entries = change["entries"]
        .as_array()
        .ok_or("invalid change entries")?;
    let by_id: BTreeMap<_, _> = candidate
        .items
        .iter()
        .map(|item| (item.entity_id.as_str(), item))
        .collect();
    let mut current = BTreeMap::new();
    let mut subject_entries = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = entry["entity_id"].as_str().ok_or("invalid entity")?;
        let item = by_id.get(id).copied();
        let after = entry.get("after");
        let content = match (after, item) {
            (None, None) => None,
            (Some(_), Some(item)) => Some(item.source.as_bytes().to_vec()),
            _ => return Err("candidate subject differs"),
        };
        current.insert(id.to_owned(), content);
        subject_entries.push(json!({"entity_id": id, "sha256": after.map(|s| &s["sha256"])}));
    }
    let subject =
        json!({"change_id":change["change_id"], "base":change["base"], "entries":subject_entries});
    let decision = DecisionInputs {
        change: input.change.to_vec(),
        policy: input.resolved_policy.to_vec(),
        options: input.host_options.to_vec(),
        collection: input.collection.map(<[u8]>::to_vec),
        external: input.external_evidence.map(<[u8]>::to_vec),
        additional: BTreeMap::new(),
    };
    let mut coverage = None;
    let mut word_counts = BTreeMap::new();
    let mut character_counts = BTreeMap::new();
    let mut paragraph_counts = BTreeMap::new();
    for env in [&previous, &fresh] {
        if env["binding_sha256"] != binding_hex {
            return Err("candidate input binding differs");
        }
        let verdict = &env["verdict"];
        let observed = verdict
            .get("extensions")
            .and_then(|ext| ext.get("aexeo.observed-link-binding.v1"))
            .is_some();
        let source_bound = verdict
            .get("extensions")
            .and_then(|ext| ext.get("aexeo.change-evidence-input-binding.v1"))
            .is_some();
        if observed != links.is_some() || source_bound != sources.is_some() {
            return Err("external evidence does not match the verdict profile");
        }
        if verdict["subject"] != subject
            || verdict["validator"]["name"] != input.expected_validator_name
            || verdict["validator"]["version"] != input.expected_validator_version
        {
            return Err("verdict subject or validator differs");
        }
        let encoded = serde_json::to_vec(verdict).map_err(|_| "invalid verdict")?;
        strict_precheck_v1(
            &decision,
            &decision,
            &encoded,
            input.fresh_exit_code,
            input.fresh_timed_out,
            &current,
        )?;
        let checks: BTreeMap<_, _> = verdict["checks"]
            .as_array()
            .ok_or("invalid checks")?
            .iter()
            .filter_map(|c| c["id"].as_str().map(|id| (id, c)))
            .collect();
        let host_verified = portable_host_v2::verify(&policy, verdict, input.after, &checks)?;
        let mut coverage_change = change.clone();
        if host_verified {
            for entry in coverage_change["entries"]
                .as_array_mut()
                .ok_or("invalid entries")?
            {
                if entry["media_type"] == portable_host_v2::MEDIA {
                    entry["media_type"] = json!("text/plain");
                }
            }
        }
        coverage = Some(policy_coverage(
            &policy,
            &coverage_change,
            input.collection.is_some(),
            &checks,
        )?);
        if let Some(collection) = collection.as_ref() {
            verify_collection_statuses(&policy, collection, &checks)?;
        }
        verify_final_newline_statuses(&policy, &checks, input.after)?;
        verify_plain_word_count_statuses(&policy, &checks, input.after, &mut word_counts)?;
        verify_plain_paragraph_count_statuses(
            &policy,
            &checks,
            input.after,
            &mut paragraph_counts,
        )?;
        verify_plain_character_count_statuses(
            &policy,
            &checks,
            input.after,
            &mut character_counts,
        )?;
        evidence_capabilities_with_mode(
            &policy,
            &change,
            &checks,
            sources.as_ref(),
            links.as_ref(),
            plain_text_paragraphs,
        )?;
        for id in &required {
            if !checks
                .get(id)
                .is_some_and(|c| c["status"] == "passed" && c["blocking"] == true)
            {
                return Err("required check absent or not passing");
            }
        }
    }
    let mut old = previous["verdict"].clone();
    let mut new = fresh["verdict"].clone();
    old.as_object_mut()
        .ok_or("invalid verdict")?
        .remove("evaluated_at");
    new.as_object_mut()
        .ok_or("invalid verdict")?
        .remove("evaluated_at");
    if old != new {
        return Err("fresh verdict differs");
    }
    let (policy_gate_count, policy_check_count) = coverage.ok_or("missing policy coverage")?;
    Ok(ExperimentalMatch {
        policy_gate_count,
        policy_check_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Value, Snapshot, Vec<Item>, Value) {
        let v: Value = serde_json::from_str(include_str!("portable-verdict-vector.json")).unwrap();
        let base = serde_json::from_value(v["base"].clone()).unwrap();
        let after = serde_json::from_value(v["after"].clone()).unwrap();
        let envelope = v["portable"].clone();
        (v, base, after, envelope)
    }
    fn inputs<'a>(v: &'a Value, base: &'a Snapshot, after: &'a [Item]) -> Inputs<'a> {
        Inputs {
            base,
            after,
            change: v["artifacts"]["change"].as_str().unwrap().as_bytes(),
            resolved_policy: v["artifacts"]["resolved_policy"]
                .as_str()
                .unwrap()
                .as_bytes(),
            host_options: v["artifacts"]["host_options"].as_str().unwrap().as_bytes(),
            collection: None,
            external_evidence: None,
            evaluator_identity: v["artifacts"]["evaluator"].as_str().unwrap().as_bytes(),
            expected_validator_name: "aexeo",
            expected_validator_version: "0.0.24",
            required_checks: &["gate:7:article:words"],
            fresh_exit_code: Some(0),
            fresh_timed_out: false,
        }
    }
    fn run(old: &Value, new: &Value, input: &Inputs<'_>) -> bool {
        compare(
            old.to_string().as_bytes(),
            new.to_string().as_bytes(),
            input,
        )
        .is_ok()
    }
    #[test]
    fn matches_aexeo_generated_host_vector_and_rejects_changed_candidate() {
        let vector: Value =
            serde_json::from_str(include_str!("portable-host-vector.json")).unwrap();
        let base: Snapshot = serde_json::from_value(vector["base"].clone()).unwrap();
        let mut after: Vec<Item> = serde_json::from_value(vector["after"].clone()).unwrap();
        let check_ids = [
            "gate:7:article:paragraphs",
            "gate:7:article:pwords",
            "gate:7:article:pchars",
        ];
        let run = |items: &[Item]| {
            let input = Inputs {
                base: &base,
                after: items,
                change: vector["artifacts"]["change"].as_str().unwrap().as_bytes(),
                resolved_policy: vector["artifacts"]["resolved_policy"]
                    .as_str()
                    .unwrap()
                    .as_bytes(),
                host_options: vector["artifacts"]["host_options"]
                    .as_str()
                    .unwrap()
                    .as_bytes(),
                collection: None,
                external_evidence: None,
                evaluator_identity: vector["artifacts"]["evaluator"]
                    .as_str()
                    .unwrap()
                    .as_bytes(),
                expected_validator_name: "aexeo",
                expected_validator_version: "0.0.26",
                required_checks: &check_ids,
                fresh_exit_code: Some(0),
                fresh_timed_out: false,
            };
            compare(
                vector["previous"].to_string().as_bytes(),
                vector["fresh"].to_string().as_bytes(),
                &input,
            )
        };
        assert!(run(&after).is_ok());
        after[0].source.push(' ');
        assert!(run(&after).is_err());
    }

    #[test]
    fn matches_shared_vector_but_does_not_authorize_publication() {
        let (v, base, after, old) = fixture();
        let input = inputs(&v, &base, &after);
        let mut fresh = old.clone();
        fresh["verdict"]["evaluated_at"] = json!("2026-11-19T10:37:59Z");
        assert!(run(&old, &fresh, &input));
        fresh["verdict"]["validator"]["name"] = json!("other");
        assert!(!run(&old, &fresh, &input));
    }
    #[test]
    fn final_newline_status_matches_source_for_all_supported_formats() {
        let (_, _, mut after, _) = fixture();
        let policy = json!({"gates":[{"id":"line","kind":"final_newline"}]});
        for media in ["text/plain", "text/markdown", "text/html"] {
            after[0].media_type = media.into();
            for (source, expected) in [
                ("", "passed"),
                ("é\n", "passed"),
                ("é\r\n", "passed"),
                ("é", "failed"),
                ("é\r", "failed"),
                ("é\u{2028}", "failed"),
            ] {
                after[0].source = source.into();
                let mut check = json!({"status":expected});
                assert!(
                    verify_final_newline_statuses(
                        &policy,
                        &BTreeMap::from([("gate:7:article:line", &check)]),
                        &after
                    )
                    .is_ok()
                );
                check["status"] = json!(if expected == "passed" {
                    "failed"
                } else {
                    "passed"
                });
                assert_eq!(
                    verify_final_newline_statuses(
                        &policy,
                        &BTreeMap::from([("gate:7:article:line", &check)]),
                        &after
                    ),
                    Err("final newline status differs from source")
                );
                check["status"] = json!("not_checked");
                assert!(
                    verify_final_newline_statuses(
                        &policy,
                        &BTreeMap::from([("gate:7:article:line", &check)]),
                        &after
                    )
                    .is_ok()
                );
            }
        }
    }

    #[test]
    fn final_newline_policy_rejects_missing_or_unknown_fields() {
        let (v, _, _, verdict) = fixture();
        let mut policy: Value =
            serde_json::from_str(v["artifacts"]["resolved_policy"].as_str().unwrap()).unwrap();
        let change: Value =
            serde_json::from_str(v["artifacts"]["change"].as_str().unwrap()).unwrap();
        policy["gates"][0] =
            json!({"kind":"final_newline","id":"words","severity":"warning","blocking":true});
        let check = &verdict["verdict"]["checks"][0];
        let checks = BTreeMap::from([("gate:7:article:words", check)]);
        assert!(policy_coverage(&policy, &change, false, &checks).is_ok());
        policy["gates"][0]["extra"] = json!(true);
        assert!(policy_coverage(&policy, &change, false, &checks).is_err());
        policy["gates"][0].as_object_mut().unwrap().remove("extra");
        policy["gates"][0]
            .as_object_mut()
            .unwrap()
            .remove("severity");
        assert!(policy_coverage(&policy, &change, false, &checks).is_err());
    }

    #[test]
    fn portable_comparator_rejects_rebound_false_final_newline_pass() {
        let (v, base, after, mut verdict) = fixture();
        let mut input = inputs(&v, &base, &after);
        let mut policy: Value = serde_json::from_slice(input.resolved_policy).unwrap();
        let gate = policy["gates"][0].as_object_mut().unwrap();
        gate.remove("min");
        gate.remove("max");
        gate.insert("kind".into(), json!("final_newline"));
        let policy_bytes = serde_json::to_vec(&policy).unwrap();
        input.resolved_policy = &policy_bytes;
        let candidate = overlay(&base, input.change, &after).unwrap();
        let hash = candidate_binding(&CandidateArtifacts {
            change: input.change,
            resolved_policy: input.resolved_policy,
            host_options: input.host_options,
            evaluator: input.evaluator_identity,
            collection: None,
            external_evidence: None,
            base_revision_sha256: snapshot_digest(&base).unwrap(),
            candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
        })
        .unwrap();
        verdict["binding_sha256"] =
            json!(hash.iter().map(|b| format!("{b:02x}")).collect::<String>());
        assert_eq!(
            compare(
                verdict.to_string().as_bytes(),
                verdict.to_string().as_bytes(),
                &input
            ),
            Err("final newline status differs from source")
        );
    }

    #[test]
    fn plain_paragraph_counts_respect_boundaries_and_limits() {
        for (source, expected) in [
            ("", 0),
            (" \n\t\r\n", 0),
            ("One\r\ntwo\r\n\u{a0}\r\nThree", 2),
            ("a\r\rb", 1),
            ("a\n\u{1c}\nb", 1),
            ("a\n\nb\n", 2),
        ] {
            assert_eq!(plain_text_paragraph_count(source), Ok(expected));
        }
        assert_eq!(
            plain_text_paragraph_count(&"x\n\n".repeat(100_000)),
            Ok(100_000)
        );
        assert_eq!(
            plain_text_paragraph_count(&"x\n\n".repeat(100_001)),
            Err("plain-text paragraph count exceeds evaluator limit")
        );
    }

    #[test]
    fn plain_paragraph_statuses_respect_bounds_and_unavailable() {
        let (_, _, mut after, _) = fixture();
        after[0].source = "One\n\nTwo".into();
        let policy =
            json!({"gates":[{"id":"paragraphs","kind":"paragraph_count","min":2,"max":2}]});
        let mut check = json!({"status":"passed"});
        let mut counts = BTreeMap::new();
        let verify = |c: &Value, items: &[Item], cache: &mut BTreeMap<String, u64>| {
            let checks = BTreeMap::from([("gate:7:article:paragraphs", c)]);
            verify_plain_paragraph_count_statuses(&policy, &checks, items, cache)
        };
        assert!(verify(&check, &after, &mut counts).is_ok());
        check["status"] = json!("failed");
        assert_eq!(
            verify(&check, &after, &mut counts),
            Err("plain-text paragraph count status differs from source")
        );
        check["status"] = json!("not_checked");
        assert!(verify(&check, &after, &mut counts).is_ok());
        after[0].media_type = "text/markdown".into();
        check["status"] = json!("failed");
        assert!(verify(&check, &after, &mut counts).is_ok());
    }

    #[test]
    fn plain_word_counts_respect_bounds_whitespace_and_unavailable() {
        let (_, _, mut after, _) = fixture();
        after[0].source = "é\u{a0}word\u{85}next\u{1c}tail".into();
        let mut policy = json!({"gates":[{"id":"words","kind":"word_count","min":3,"max":3}]});
        let mut check = json!({"status":"passed"});
        let mut counts = BTreeMap::new();
        let verify = |p: &Value, c: &Value, items: &[Item], cache: &mut BTreeMap<String, u64>| {
            let checks = BTreeMap::from([("gate:7:article:words", c)]);
            verify_plain_word_count_statuses(p, &checks, items, cache)
        };
        assert!(verify(&policy, &check, &after, &mut counts).is_ok());
        assert_eq!(counts.get("article"), Some(&3));
        policy["gates"][0]["min"] = json!(4);
        policy["gates"][0]["max"] = Value::Null;
        assert!(verify(&policy, &check, &after, &mut counts).is_err());
        check["status"] = json!("failed");
        assert!(verify(&policy, &check, &after, &mut counts).is_ok());
        policy["gates"][0]["min"] = Value::Null;
        policy["gates"][0]["max"] = json!(2);
        assert!(verify(&policy, &check, &after, &mut counts).is_ok());
        check["status"] = json!("not_checked");
        assert!(verify(&policy, &check, &after, &mut counts).is_ok());
        after[0].media_type = "text/markdown".into();
        check["status"] = json!("passed");
        assert!(verify(&policy, &check, &after, &mut counts).is_ok());
    }
    #[test]
    fn rejects_rebound_plain_text_word_count_false_pass() {
        let (v, base, after, mut verdict) = fixture();
        let mut input = inputs(&v, &base, &after);
        let mut policy: Value = serde_json::from_slice(input.resolved_policy).unwrap();
        policy["gates"][0]["min"] = json!(2);
        let policy = serde_json::to_vec(&policy).unwrap();
        input.resolved_policy = &policy;
        let candidate = overlay(&base, input.change, &after).unwrap();
        let binding = candidate_binding(&CandidateArtifacts {
            change: input.change,
            resolved_policy: input.resolved_policy,
            host_options: input.host_options,
            evaluator: input.evaluator_identity,
            collection: None,
            external_evidence: None,
            base_revision_sha256: snapshot_digest(&base).unwrap(),
            candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
        })
        .unwrap();
        verdict["binding_sha256"] = json!(
            binding
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        assert_eq!(
            compare(
                verdict.to_string().as_bytes(),
                verdict.to_string().as_bytes(),
                &input
            ),
            Err("plain-text word count status differs from source")
        );
    }
    #[test]
    fn plain_character_counts_respect_whitespace_and_unavailable() {
        let (_, _, mut after, _) = fixture();
        after[0].source = "é\u{a0}a\u{1c}b".into();
        let mut policy = json!({"gates":[{"id":"chars","kind":"character_count","min":4,"max":4,"ignore_whitespace":true}]});
        let mut check = json!({"status":"passed"});
        let mut counts = BTreeMap::new();
        let verify =
            |p: &Value, c: &Value, items: &[Item], cache: &mut BTreeMap<(String, bool), u64>| {
                let checks = BTreeMap::from([("gate:7:article:chars", c)]);
                verify_plain_character_count_statuses(p, &checks, items, cache)
            };
        assert!(verify(&policy, &check, &after, &mut counts).is_ok());
        policy["gates"][0]["ignore_whitespace"] = json!(false);
        assert!(verify(&policy, &check, &after, &mut counts).is_err());
        check["status"] = json!("failed");
        assert!(verify(&policy, &check, &after, &mut counts).is_ok());
        check["status"] = json!("not_checked");
        assert!(verify(&policy, &check, &after, &mut counts).is_ok());
        after[0].media_type = "text/markdown".into();
        check["status"] = json!("passed");
        assert!(verify(&policy, &check, &after, &mut counts).is_ok());
    }
    #[test]
    fn rejects_rebound_plain_text_character_count_false_pass() {
        let (v, base, after, mut verdict) = fixture();
        let mut input = inputs(&v, &base, &after);
        let mut policy: Value = serde_json::from_slice(input.resolved_policy).unwrap();
        policy["gates"][0]["kind"] = json!("character_count");
        policy["gates"][0]["ignore_whitespace"] = json!(false);
        policy["gates"][0]["min"] = json!(4);
        let policy = serde_json::to_vec(&policy).unwrap();
        input.resolved_policy = &policy;
        let candidate = overlay(&base, input.change, &after).unwrap();
        let binding = candidate_binding(&CandidateArtifacts {
            change: input.change,
            resolved_policy: input.resolved_policy,
            host_options: input.host_options,
            evaluator: input.evaluator_identity,
            collection: None,
            external_evidence: None,
            base_revision_sha256: snapshot_digest(&base).unwrap(),
            candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
        })
        .unwrap();
        verdict["binding_sha256"] = json!(
            binding
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        assert_eq!(
            compare(
                verdict.to_string().as_bytes(),
                verdict.to_string().as_bytes(),
                &input
            ),
            Err("plain-text character count status differs from source")
        );
    }
    #[test]
    fn rejects_rebound_plain_text_paragraph_count_false_pass() {
        let (v, base, after, mut verdict) = fixture();
        let mut input = inputs(&v, &base, &after);
        let mut policy: Value = serde_json::from_slice(input.resolved_policy).unwrap();
        policy["gates"][0]["kind"] = json!("paragraph_count");
        policy["gates"][0]["min"] = json!(2);
        let policy = serde_json::to_vec(&policy).unwrap();
        input.resolved_policy = &policy;
        let mut options: Value = serde_json::from_slice(input.host_options).unwrap();
        options["plain_text_paragraphs"] = json!(true);
        let options = serde_json::to_vec(&options).unwrap();
        input.host_options = &options;
        let candidate = overlay(&base, input.change, &after).unwrap();
        let binding = candidate_binding(&CandidateArtifacts {
            change: input.change,
            resolved_policy: input.resolved_policy,
            host_options: input.host_options,
            evaluator: input.evaluator_identity,
            collection: None,
            external_evidence: None,
            base_revision_sha256: snapshot_digest(&base).unwrap(),
            candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
        })
        .unwrap();
        verdict["binding_sha256"] = json!(
            binding
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        assert_eq!(
            compare(
                verdict.to_string().as_bytes(),
                verdict.to_string().as_bytes(),
                &input
            ),
            Err("plain-text paragraph count status differs from source")
        );
    }
    #[test]
    fn rejects_rebound_collection_pass_when_metadata_is_unavailable() {
        let (v, base, after, mut verdict) = fixture();
        let mut input = inputs(&v, &base, &after);
        let policy = json!({"version":1,"name":"test","gates":[{
            "id":"unique","kind":"unique_collection_metadata","field":"slug",
            "severity":"warning","blocking":true
        }]})
        .to_string()
        .into_bytes();
        let collection = json!({"change_id":"example","complete":true,
            "items":[{"id":"article"}]})
        .to_string()
        .into_bytes();
        input.resolved_policy = &policy;
        input.collection = Some(&collection);
        input.required_checks = &["collection:0::unique"];
        let candidate = overlay(&base, input.change, &after).unwrap();
        let binding = candidate_binding(&CandidateArtifacts {
            change: input.change,
            resolved_policy: input.resolved_policy,
            host_options: input.host_options,
            evaluator: input.evaluator_identity,
            collection: input.collection,
            external_evidence: None,
            base_revision_sha256: snapshot_digest(&base).unwrap(),
            candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
        })
        .unwrap();
        verdict["binding_sha256"] = json!(
            binding
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        verdict["verdict"]["checks"][0]["id"] = json!("collection:0::unique");
        assert_eq!(
            compare(
                verdict.to_string().as_bytes(),
                verdict.to_string().as_bytes(),
                &input
            ),
            Err("collection gate cannot assess incomplete observations")
        );
    }

    #[test]
    fn collection_uniqueness_observes_group_values_and_limits() {
        let mut collection = json!({"complete": true, "items": [
            {"metadata":{"slug":"same", "lang":"en"}},
            {"metadata":{"slug":"same", "lang":"en"}}
        ]});
        assert_eq!(
            collection_uniqueness_status(&collection, "slug", Some("lang")),
            Ok("failed")
        );
        collection["items"][1]["metadata"]["lang"] = json!("fr");
        assert_eq!(
            collection_uniqueness_status(&collection, "slug", Some("lang")),
            Ok("passed")
        );
        assert_eq!(
            collection_uniqueness_status(&collection, "slug", None),
            Ok("failed")
        );
        collection["items"][1]["metadata"]["slug"] = json!(" same ");
        assert_eq!(
            collection_uniqueness_status(&collection, "slug", None),
            Ok("passed")
        );
        collection["items"][1]["metadata"]["slug"] = json!(" ");
        assert_eq!(
            collection_uniqueness_status(&collection, "slug", None),
            Ok("not_checked")
        );
        collection["items"][1]["metadata"]["slug"] = json!("é".repeat(8193));
        assert_eq!(
            collection_uniqueness_status(&collection, "slug", None),
            Ok("not_checked")
        );
        collection["complete"] = json!(false);
        assert_eq!(
            collection_uniqueness_status(&collection, "slug", None),
            Ok("not_checked")
        );
        let items: Vec<_> = (0..10001)
            .map(|i| json!({"metadata":{"slug": i.to_string()}}))
            .collect();
        collection = json!({"complete":true,"items":items});
        assert_eq!(
            collection_uniqueness_status(&collection, "slug", None),
            Ok("not_checked")
        );
    }

    #[test]
    fn collection_status_must_match_available_observations() {
        let policy =
            json!({"gates":[{"id":"unique", "kind":"unique_collection_metadata", "field":"slug"}]});
        let collection = json!({"complete":true,"items":[
            {"metadata":{"slug":"same"}},{"metadata":{"slug":"same"}}
        ]});
        for (status, accepted) in [("passed", false), ("failed", true), ("not_checked", true)] {
            let check = json!({"status":status});
            let checks = BTreeMap::from([("collection:0::unique", &check)]);
            assert_eq!(
                verify_collection_statuses(&policy, &collection, &checks).is_ok(),
                accepted
            );
        }
    }

    #[test]
    fn rejects_rebound_policy_with_omitted_gate_even_when_required_check_passes() {
        let (v, base, after, mut verdict) = fixture();
        let mut input = inputs(&v, &base, &after);
        let mut policy: Value = serde_json::from_slice(input.resolved_policy).unwrap();
        let mut extra = policy["gates"][0].clone();
        extra["id"] = json!("omitted");
        extra["blocking"] = json!(false);
        policy["gates"].as_array_mut().unwrap().push(extra);
        let policy_bytes = serde_json::to_vec(&policy).unwrap();
        input.resolved_policy = &policy_bytes;
        let candidate = overlay(&base, input.change, &after).unwrap();
        let binding = candidate_binding(&CandidateArtifacts {
            change: input.change,
            resolved_policy: input.resolved_policy,
            host_options: input.host_options,
            evaluator: input.evaluator_identity,
            collection: None,
            external_evidence: None,
            base_revision_sha256: snapshot_digest(&base).unwrap(),
            candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
        })
        .unwrap();
        verdict["binding_sha256"] = json!(
            binding
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        assert_eq!(
            compare(
                verdict.to_string().as_bytes(),
                verdict.to_string().as_bytes(),
                &input
            ),
            Err("resolved policy check is missing")
        );
    }

    #[test]
    fn rejects_extra_passed_check_outside_resolved_policy() {
        let (v, base, after, mut verdict) = fixture();
        let mut input = inputs(&v, &base, &after);
        assert!(run(&verdict, &verdict, &input));
        verdict["verdict"]["checks"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id": "gate:7:article:unconfigured", "status": "passed", "blocking": true
            }));
        verdict["verdict"]["summary"]["checks"] = json!(2);
        verdict["verdict"]["summary"]["passed"] = json!(2);
        input.required_checks = &["gate:7:article:unconfigured"];
        assert_eq!(
            compare(
                verdict.to_string().as_bytes(),
                verdict.to_string().as_bytes(),
                &input
            ),
            Err("unexpected check outside resolved policy")
        );
    }

    #[test]
    fn rejects_empty_resolved_policy_even_with_existing_checks() {
        let (v, _, _, verdict) = fixture();
        let mut policy: Value =
            serde_json::from_str(v["artifacts"]["resolved_policy"].as_str().unwrap()).unwrap();
        let change: Value =
            serde_json::from_str(v["artifacts"]["change"].as_str().unwrap()).unwrap();
        let check = &verdict["verdict"]["checks"][0];
        let checks = BTreeMap::from([("gate:7:article:words", check)]);
        assert_eq!(
            policy_coverage(&policy, &change, false, &checks),
            Ok((1, 1))
        );
        policy["gates"] = json!([]);
        assert_eq!(
            policy_coverage(&policy, &change, false, &checks),
            Err("empty resolved policy")
        );
    }

    #[test]
    fn unsupported_candidate_capabilities_are_not_reported_as_executed() {
        let (v, _, _, verdict) = fixture();
        let mut policy: Value =
            serde_json::from_str(v["artifacts"]["resolved_policy"].as_str().unwrap()).unwrap();
        let mut change: Value =
            serde_json::from_str(v["artifacts"]["change"].as_str().unwrap()).unwrap();
        let mut check = verdict["verdict"]["checks"][0].clone();
        for kind in ["custom", "site_group", "site_audit"] {
            policy["gates"][0]["kind"] = json!(kind);
            let checks = BTreeMap::from([("gate:7:article:words", &check)]);
            assert_eq!(
                policy_coverage(&policy, &change, false, &checks),
                Err("candidate evaluator cannot execute site or custom gates")
            );
            check["status"] = json!("not_checked");
            let checks = BTreeMap::from([("gate:7:article:words", &check)]);
            assert_eq!(
                policy_coverage(&policy, &change, false, &checks),
                Ok((1, 1))
            );
            check["status"] = json!("passed");
        }
        policy["gates"][0]["kind"] = json!("word_count");
        change["entries"][0]["media_type"] = json!("application/pdf");
        let checks = BTreeMap::from([("gate:7:article:words", &check)]);
        assert_eq!(
            policy_coverage(&policy, &change, false, &checks),
            Err("item media type is not supported by this evaluator")
        );
        policy["gates"][0]["kind"] = json!("unique_collection_metadata");
        let collection = json!({"id":"collection:0::words", "blocking":true});
        let checks = BTreeMap::from([("collection:0::words", &collection)]);
        assert_eq!(policy_coverage(&policy, &change, true, &checks), Ok((1, 1)));
    }

    #[test]
    fn collection_gate_cannot_run_without_collection() {
        let policy = json!({"version":1,"name":"test","gates":[{"id":"unique","kind":"unique_collection_metadata","blocking":false}]});
        let change =
            json!({"entries":[{"entity_id":"article","media_type":"text/plain","after":{}}]});
        for status in ["passed", "failed", "not_checked"] {
            let check = json!({"status":status,"blocking":false});
            let checks = BTreeMap::from([("gate:7:article:unique", &check)]);
            assert_eq!(
                policy_coverage(&policy, &change, false, &checks).is_ok(),
                status == "not_checked"
            );
        }
    }

    #[test]
    fn policy_coverage_checks_unicode_collection_and_unknown_kinds() {
        let (v, _, _, verdict) = fixture();
        let mut policy: Value =
            serde_json::from_str(v["artifacts"]["resolved_policy"].as_str().unwrap()).unwrap();
        let mut change: Value =
            serde_json::from_str(v["artifacts"]["change"].as_str().unwrap()).unwrap();
        change["entries"][0]["entity_id"] = json!("été");
        let mut check = verdict["verdict"]["checks"][0].clone();
        check["id"] = json!("gate:5:été:words");
        let mut checks = BTreeMap::from([("gate:5:été:words", &check)]);
        assert_eq!(
            policy_coverage(&policy, &change, false, &checks),
            Ok((1, 1))
        );
        let mut extra = policy["gates"][0].clone();
        extra["kind"] = json!("unique_collection_metadata");
        extra["id"] = json!("unique");
        policy["gates"].as_array_mut().unwrap().push(extra);
        let collection = json!({"id":"collection:0::unique","blocking":true});
        checks.insert("collection:0::unique", &collection);
        assert_eq!(policy_coverage(&policy, &change, true, &checks), Ok((2, 2)));
        assert!(policy_coverage(&policy, &change, false, &checks).is_err());
        policy["gates"][1]["kind"] = json!("unknown_future_gate");
        assert!(policy_coverage(&policy, &change, true, &checks).is_err());
        policy["gates"][1]["kind"] = json!("unique_collection_metadata");
        policy["gates"][1]["id"] = json!("words");
        assert!(policy_coverage(&policy, &change, true, &checks).is_err());
        policy["gates"].as_array_mut().unwrap().pop();
        policy["gates"][0]["blocking"] = json!(false);
        assert!(policy_coverage(&policy, &change, false, &checks).is_err());
    }

    #[test]
    fn rejects_stale_evidence_missing_checks_and_process_failure() {
        let (v, base, after, old) = fixture();
        let mut input = inputs(&v, &base, &after);
        assert!(run(&old, &old, &input));
        input.host_options = b"changed";
        assert!(!run(&old, &old, &input));
        input.host_options = v["artifacts"]["host_options"].as_str().unwrap().as_bytes();
        input.evaluator_identity = b"";
        assert!(!run(&old, &old, &input));
        input.evaluator_identity = b"build";
        input.required_checks = &[];
        assert!(!run(&old, &old, &input));
        input.required_checks = &["missing"];
        assert!(!run(&old, &old, &input));
        input.required_checks = &["gate:7:article:words", "gate:7:article:words"];
        assert!(!run(&old, &old, &input));
        input.required_checks = &["gate:7:article:words"];
        input.fresh_exit_code = Some(1);
        assert!(!run(&old, &old, &input));
        input.fresh_exit_code = Some(0);
        input.fresh_timed_out = true;
        assert!(!run(&old, &old, &input));
        let mut changed = base.clone();
        changed.complete = false;
        assert!(!run(&old, &old, &inputs(&v, &changed, &after)));
    }
    #[test]
    fn binds_collection_and_rejects_inconsistent_projection() {
        let (v, base, after, mut verdict) = fixture();
        let mut input = inputs(&v, &base, &after);
        let collection =
            br#"{"change_id":"example","complete":true,"items":[{"id":"article","text":"abc"}]}"#;
        input.collection = Some(collection);
        verdict["binding_sha256"] =
            json!("13853ce80d789902d38c941f81f8b59b6be34b7ead0137c7b35b95009bef97ac");
        assert!(run(&verdict, &verdict, &input));
        for bad in [
            br#"{"change_id":"other","complete":true,"items":[{"id":"article"}]}"#.as_slice(),
            br#"{"change_id":"example","complete":true,"items":[]}"#.as_slice(),
            br#"{"change_id":"example","complete":false,"items":[]}"#.as_slice(),
            br#"{"change_id":"example","complete":true,"items":[{"id":"other"}]}"#.as_slice(),
            br#"{"change_id":"example","complete":true,"items":[{"id":"article","metadata":{"x":"y"}}]}"#.as_slice(),
            br#"{"change_id":"example","complete":true,"items":[{"id":"article","unknown":true}]}"#.as_slice(),
            br#"{"change_id":"example","complete":true,"complete":true,"items":[{"id":"article"}]}"#.as_slice(),
        ] {
            input.collection = Some(bad); assert!(!run(&verdict, &verdict, &input));
        }
        // Equivalent projection with different exact bytes is still a different input.
        input.collection = Some(
            br#"{ "change_id":"example","complete":true,"items":[{"id":"article","text":"abc"}]}"#,
        );
        assert!(!run(&verdict, &verdict, &input));
        input.collection = None;
        assert!(!run(&verdict, &verdict, &input));
    }
    #[test]
    fn permits_only_the_known_main_html_scope_extension() {
        let (_, _, _, mut verdict) = fixture();
        verdict["verdict"]["extensions"]["aexeo.html-content-scope.v1"] = json!("main");
        assert!(closed_verdict(&verdict["verdict"]).is_ok());
        for invalid in [json!("full"), json!(null), json!(true), json!(1)] {
            verdict["verdict"]["extensions"]["aexeo.html-content-scope.v1"] = invalid;
            assert!(closed_verdict(&verdict["verdict"]).is_err());
        }
        verdict["verdict"]["extensions"]["aexeo.html-content-scope.v1"] = json!("main");
        verdict["verdict"]["extensions"]
            .as_object_mut()
            .unwrap()
            .remove("aexeo.offline-input-binding.v1");
        assert!(closed_verdict(&verdict["verdict"]).is_err());
    }
    #[test]
    fn paragraph_checks_respect_host_mode_and_sentence_scope() {
        for kind in [
            "paragraph_count",
            "sentence_length",
            "sentence_character_length",
        ] {
            for paragraphs_only in [false, true] {
                let policy =
                    json!({"gates":[{"id":"p","kind":kind,"paragraphs_only":paragraphs_only}]});
                for media in ["text/plain", "text/markdown", "text/html"] {
                    let change =
                        json!({"entries":[{"entity_id":"article","media_type":media,"after":{}}]});
                    let check = json!({"status":"passed"});
                    let checks = BTreeMap::from([("gate:7:article:p", &check)]);
                    for enabled in [false, true] {
                        assert_eq!(
                            evidence_capabilities_with_mode(
                                &policy, &change, &checks, None, None, enabled
                            )
                            .is_ok(),
                            media != "text/plain"
                                || enabled
                                || (kind != "paragraph_count" && !paragraphs_only),
                            "{kind} {media} {paragraphs_only} {enabled}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn absent_plain_paragraphs_cannot_be_reported_as_checked() {
        for kind in [
            "paragraph_count",
            "paragraph_length",
            "paragraph_character_length",
            "repeated_paragraph_openings",
            "repeated_sentence_openings",
            "sentence_opening_window",
            "sentence_opening_terms",
            "repeated_punctuation",
            "trailing_ing_clause",
            "similar_sentence_lengths",
            "sentence_length_variance",
            "inline_triple_candidates",
            "duplicate_paragraphs",
        ] {
            let policy = json!({"gates":[{"id":"p","kind":kind}]});
            let change =
                json!({"entries":[{"entity_id":"article","media_type":"text/plain","after":{}}]});
            for status in ["passed", "failed", "not_checked"] {
                let check = json!({"status":status});
                let checks = BTreeMap::from([("gate:7:article:p", &check)]);
                assert_eq!(
                    evidence_capabilities(&policy, &change, &checks, None, None).is_ok(),
                    status == "not_checked",
                    "{kind} {status}"
                );
            }
        }
    }

    #[test]
    fn missing_metadata_cannot_be_reported_as_checked() {
        for kind in ["required_metadata", "metadata_length", "metadata_age"] {
            let policy = json!({"gates":[{"id":"meta","kind":kind}]});
            for metadata in [None, Some(json!({}))] {
                let mut change = json!({"entries":[{"entity_id":"article","after":{}}]});
                if let Some(value) = &metadata {
                    change["entries"][0]["metadata"] = value.clone();
                }
                for status in ["passed", "failed", "not_checked"] {
                    let check = json!({"status":status});
                    let checks = BTreeMap::from([("gate:7:article:meta", &check)]);
                    assert_eq!(
                        evidence_capabilities(&policy, &change, &checks, None, None).is_ok(),
                        metadata.is_some() || status == "not_checked",
                        "{kind} {metadata:?} {status}"
                    );
                }
            }
        }
    }

    #[test]
    fn literal_markdown_links_require_html_observation() {
        let policy = json!({"gates":[{"id":"literal","kind":"literal_markdown_links"}]});
        for media in ["text/plain", "text/markdown", "text/html"] {
            let change = json!({"entries":[{"entity_id":"article","media_type":media,"after":{}}]});
            for status in ["passed", "failed", "not_checked"] {
                let check = json!({"status":status});
                let checks = BTreeMap::from([("gate:7:article:literal", &check)]);
                assert_eq!(
                    evidence_capabilities(&policy, &change, &checks, None, None).is_ok(),
                    media == "text/html" || status == "not_checked",
                    "{media} {status}"
                );
            }
        }
    }

    #[test]
    fn plain_text_cannot_claim_structural_observations() {
        for kind in [
            "list_item_count",
            "link_count",
            "link_destinations",
            "required_headings",
            "heading_length",
            "heading_hierarchy",
            "duplicate_headings",
            "prose_sentence_length_variance",
        ] {
            let policy = json!({"gates":[{"id":"structure","kind":kind}]});
            for media in ["text/plain", "text/markdown", "text/html"] {
                let change =
                    json!({"entries":[{"entity_id":"article","media_type":media,"after":{}}]});
                for status in ["passed", "failed", "not_checked"] {
                    let check = json!({"status":status});
                    let checks = BTreeMap::from([("gate:7:article:structure", &check)]);
                    assert_eq!(
                        evidence_capabilities(&policy, &change, &checks, None, None).is_ok(),
                        media != "text/plain" || status == "not_checked",
                        "{kind} {media} {status}"
                    );
                }
            }
        }
    }

    #[test]
    fn link_evidence_is_required_for_each_completed_entity_check() {
        let policy = json!({"gates":[{"id":"links","kind":"link_reachability"}]});
        let change = json!({"entries":[{"entity_id":"article","after":{}}]});
        let mut check = json!({"status":"passed"});
        let missing = json!({"entries":{}});
        let present = json!({"entries":{"article":{"observations":[]}}});
        for status in ["passed", "failed", "not_checked"] {
            check["status"] = json!(status);
            let checks = BTreeMap::from([("gate:7:article:links", &check)]);
            assert_eq!(
                evidence_capabilities(&policy, &change, &checks, None, Some(&missing)).is_ok(),
                status == "not_checked"
            );
            assert!(evidence_capabilities(&policy, &change, &checks, None, Some(&present)).is_ok());
        }
    }

    #[test]
    fn matching_forged_verdict_cannot_hide_a_missing_link_snapshot() {
        let (v, base, after, mut verdict) = fixture();
        let mut policy: Value =
            serde_json::from_str(v["artifacts"]["resolved_policy"].as_str().unwrap()).unwrap();
        policy["gates"].as_array_mut().unwrap().push(json!({
            "id":"links", "kind":"link_reachability", "severity":"warning", "blocking":false
        }));
        let policy_bytes = policy.to_string().into_bytes();
        let change = parse_change(v["artifacts"]["change"].as_str().unwrap().as_bytes()).unwrap();
        let evidence = json!({"version":1,"change_id":change["change_id"],"entries":{}})
            .to_string()
            .into_bytes();
        let mut input = inputs(&v, &base, &after);
        input.resolved_policy = &policy_bytes;
        input.external_evidence = Some(&evidence);
        let candidate = overlay(&base, input.change, &after).unwrap();
        let hash = candidate_binding(&CandidateArtifacts {
            change: input.change,
            resolved_policy: input.resolved_policy,
            host_options: input.host_options,
            evaluator: input.evaluator_identity,
            collection: input.collection,
            external_evidence: input.external_evidence,
            base_revision_sha256: snapshot_digest(&base).unwrap(),
            candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
        })
        .unwrap();
        verdict["binding_sha256"] =
            json!(hash.iter().map(|b| format!("{b:02x}")).collect::<String>());
        verdict["verdict"]["extensions"] = json!({"aexeo.observed-link-binding.v1": {
            "inputs_sha256": "a".repeat(64), "observations_sha256": "b".repeat(64)
        }});
        verdict["verdict"]["checks"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id":"gate:7:article:links", "status":"passed", "blocking":false
            }));
        verdict["verdict"]["summary"]["checks"] = json!(2);
        verdict["verdict"]["summary"]["passed"] = json!(2);
        assert!(!run(&verdict, &verdict, &input));
        verdict["verdict"]["checks"][1]["status"] = json!("not_checked");
        verdict["verdict"]["checks"][1]["reason"] = json!("observation unavailable");
        verdict["verdict"]["summary"]["passed"] = json!(1);
        verdict["verdict"]["summary"]["not_checked"] = json!(1);
        assert!(run(&verdict, &verdict, &input));
    }

    #[test]
    fn accepts_only_exact_observed_link_extension() {
        let (_, _, _, mut verdict) = fixture();
        let v = &mut verdict["verdict"];
        assert!(closed_verdict(v).is_ok());
        v["extensions"] = json!({"aexeo.observed-link-binding.v1": {
            "inputs_sha256": "a".repeat(64), "observations_sha256": "b".repeat(64)
        }});
        assert!(closed_verdict(v).is_ok());
        v["extensions"]["aexeo.offline-input-binding.v1"] = json!("c".repeat(64));
        assert!(closed_verdict(v).is_err());
        v["extensions"]
            .as_object_mut()
            .unwrap()
            .remove("aexeo.offline-input-binding.v1");
        v["extensions"]["aexeo.observed-link-binding.v1"]["unexpected"] = json!(true);
        assert!(closed_verdict(v).is_err());
    }
    #[test]
    fn link_observations_require_supported_media() {
        let mut change = json!({"change_id":"c","entries":[{"entity_id":"article","media_type":"text/plain","after":{"sha256":"abc"}}]});
        let mut snapshot = json!({"version":1,"change_id":"c","entries":{"article":{"version":1,"source_sha256":"abc","destinations":[],"observations":[]}}});
        for media in ["text/plain", "text/markdown", "text/html"] {
            change["entries"][0]["media_type"] = json!(media);
            assert_eq!(
                validate_link_evidence(snapshot.to_string().as_bytes(), &change).is_ok(),
                media != "text/plain"
            );
        }
        change["entries"][0]["media_type"] = json!("text/plain");
        snapshot["entries"] = json!({});
        assert!(validate_link_evidence(snapshot.to_string().as_bytes(), &change).is_ok());
    }

    #[test]
    fn rejects_malformed_link_evidence_and_profile_mismatches() {
        let (mut v, base, mut after, mut verdict) = fixture();
        let mut change =
            parse_change(v["artifacts"]["change"].as_str().unwrap().as_bytes()).unwrap();
        change["entries"][0]["media_type"] = json!("text/markdown");
        v["artifacts"]["change"] = json!(change.to_string());
        after[0].media_type = "text/markdown".into();
        let mut snapshot = json!({"version":1,"change_id":change["change_id"],"entries":{"article":{
            "version":1,"source_sha256":change["entries"][0]["after"]["sha256"],
            "destinations":["https://example.org/"],"observations":["reachable"]}}});
        assert!(validate_link_evidence(snapshot.to_string().as_bytes(), &change).is_ok());
        let original = snapshot.clone();
        for (pointer, invalid) in [
            ("/version", json!(true)),
            ("/change_id", json!("wrong")),
            ("/entries/article/source_sha256", json!("0".repeat(64))),
            ("/entries/article/observations", json!([])),
            ("/entries/article/observations", json!(["unknown"])),
            ("/entries/article/destinations", json!([42])),
        ] {
            *snapshot.pointer_mut(pointer).unwrap() = invalid;
            assert!(
                validate_link_evidence(snapshot.to_string().as_bytes(), &change).is_err(),
                "{pointer}"
            );
            snapshot = original.clone();
        }
        let bytes = snapshot.to_string().into_bytes();
        let mut input = inputs(&v, &base, &after);
        input.external_evidence = Some(&bytes);
        let candidate = overlay(&base, input.change, &after).unwrap();
        let hash = candidate_binding(&CandidateArtifacts {
            change: input.change,
            resolved_policy: input.resolved_policy,
            host_options: input.host_options,
            evaluator: input.evaluator_identity,
            collection: input.collection,
            external_evidence: input.external_evidence,
            base_revision_sha256: snapshot_digest(&base).unwrap(),
            candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
        })
        .unwrap();
        verdict["binding_sha256"] =
            json!(hash.iter().map(|b| format!("{b:02x}")).collect::<String>());
        // Rebinding cannot make offline metadata accept external evidence.
        assert!(!run(&verdict, &verdict, &input));
        verdict["verdict"]["extensions"] = json!({"aexeo.observed-link-binding.v1": {
            "inputs_sha256": "a".repeat(64), "observations_sha256": "b".repeat(64)
        }});
        assert!(run(&verdict, &verdict, &input));
        input.external_evidence = None;
        assert!(!run(&verdict, &verdict, &input));
    }
    #[test]
    fn portable_comparator_rejects_forged_pacing_without_source() {
        let (v, base, after, mut verdict) = fixture();
        let mut input = inputs(&v, &base, &after);
        assert!(run(&verdict, &verdict, &input));
        let mut policy: Value = serde_json::from_slice(input.resolved_policy).unwrap();
        policy["gates"][0]["kind"] = json!("editorial_pacing");
        policy["gates"][0]["source"] = json!("composition");
        policy["gates"][0]["profile"] =
            json!({"version":1,"name":"short","fields":[{"name":"hook","max_characters":3}]});
        let policy_bytes = serde_json::to_vec(&policy).unwrap();
        input.resolved_policy = &policy_bytes;
        let candidate = overlay(&base, input.change, &after).unwrap();
        let hash = candidate_binding(&CandidateArtifacts {
            change: input.change,
            resolved_policy: input.resolved_policy,
            host_options: input.host_options,
            evaluator: input.evaluator_identity,
            collection: None,
            external_evidence: None,
            base_revision_sha256: snapshot_digest(&base).unwrap(),
            candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
        })
        .unwrap();
        verdict["binding_sha256"] =
            json!(hash.iter().map(|b| format!("{b:02x}")).collect::<String>());
        assert_eq!(
            compare(
                verdict.to_string().as_bytes(),
                verdict.to_string().as_bytes(),
                &input
            ),
            Err("candidate gate cannot execute without required evidence")
        );
    }

    #[test]
    fn portable_comparator_binds_plain_paragraph_option() {
        for enabled in [false, true] {
            let (v, base, after, mut verdict) = fixture();
            let mut input = inputs(&v, &base, &after);
            let mut policy: Value = serde_json::from_slice(input.resolved_policy).unwrap();
            policy["gates"][0]["kind"] = json!("paragraph_count");
            let policy_bytes = serde_json::to_vec(&policy).unwrap();
            input.resolved_policy = &policy_bytes;
            let mut options: Value = serde_json::from_slice(input.host_options).unwrap();
            options["plain_text_paragraphs"] = json!(enabled);
            let option_bytes = serde_json::to_vec(&options).unwrap();
            input.host_options = &option_bytes;
            let candidate = overlay(&base, input.change, &after).unwrap();
            let hash = candidate_binding(&CandidateArtifacts {
                change: input.change,
                resolved_policy: input.resolved_policy,
                host_options: input.host_options,
                evaluator: input.evaluator_identity,
                collection: None,
                external_evidence: None,
                base_revision_sha256: snapshot_digest(&base).unwrap(),
                candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
            })
            .unwrap();
            verdict["binding_sha256"] =
                json!(hash.iter().map(|b| format!("{b:02x}")).collect::<String>());
            assert_eq!(run(&verdict, &verdict, &input), enabled);
        }
    }

    #[test]
    fn portable_comparator_rejects_forged_metadata_without_projection() {
        for (kind, extra) in [
            ("required_metadata", json!({"fields":["title"]})),
            ("metadata_length", json!({"field":"title","min":1})),
            (
                "metadata_age",
                json!({"field":"date","as_of":"2026-01-01","max_days":30}),
            ),
        ] {
            let (v, base, after, mut verdict) = fixture();
            let mut input = inputs(&v, &base, &after);
            assert!(run(&verdict, &verdict, &input));
            let mut policy: Value = serde_json::from_slice(input.resolved_policy).unwrap();
            let gate = policy["gates"][0].as_object_mut().unwrap();
            gate.remove("min");
            gate.remove("max");
            gate.insert("kind".into(), json!(kind));
            gate.extend(extra.as_object().unwrap().clone());
            let policy_bytes = serde_json::to_vec(&policy).unwrap();
            input.resolved_policy = &policy_bytes;
            let candidate = overlay(&base, input.change, &after).unwrap();
            let hash = candidate_binding(&CandidateArtifacts {
                change: input.change,
                resolved_policy: input.resolved_policy,
                host_options: input.host_options,
                evaluator: input.evaluator_identity,
                collection: None,
                external_evidence: None,
                base_revision_sha256: snapshot_digest(&base).unwrap(),
                candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
            })
            .unwrap();
            verdict["binding_sha256"] =
                json!(hash.iter().map(|b| format!("{b:02x}")).collect::<String>());
            assert_eq!(
                compare(
                    verdict.to_string().as_bytes(),
                    verdict.to_string().as_bytes(),
                    &input
                ),
                Err("candidate gate cannot execute without required evidence")
            );
        }
    }

    #[test]
    fn pacing_requires_its_own_composition_source() {
        let (v, _, _, verdict) = fixture();
        let mut policy: Value =
            serde_json::from_str(v["artifacts"]["resolved_policy"].as_str().unwrap()).unwrap();
        let change: Value =
            serde_json::from_str(v["artifacts"]["change"].as_str().unwrap()).unwrap();
        policy["gates"][0]["kind"] = json!("editorial_pacing");
        policy["gates"][0]["source"] = json!("composition");
        let mut check = verdict["verdict"]["checks"][0].clone();
        for blocking in [false, true] {
            check["blocking"] = json!(blocking);
            for status in ["passed", "failed"] {
                check["status"] = json!(status);
                let checks = BTreeMap::from([("gate:7:article:words", &check)]);
                for sources in [
                    None,
                    Some(json!({"article":{}})),
                    Some(json!({"other":{"composition":"{}"}})),
                ] {
                    assert_eq!(
                        evidence_capabilities(&policy, &change, &checks, sources.as_ref(), None),
                        Err("candidate gate cannot execute without required evidence")
                    );
                }
                let sources = json!({"article":{"composition":"{}"}});
                assert!(
                    evidence_capabilities(&policy, &change, &checks, Some(&sources), None).is_ok()
                );
            }
            check["status"] = json!("not_checked");
            let checks = BTreeMap::from([("gate:7:article:words", &check)]);
            assert!(evidence_capabilities(&policy, &change, &checks, None, None).is_ok());
        }
    }

    #[test]
    fn source_inventory_binds_and_requires_per_entity_gate_inputs() {
        let (v, base, after, mut verdict) = fixture();
        let mut input = inputs(&v, &base, &after);
        let mut policy: Value = serde_json::from_slice(input.resolved_policy).unwrap();
        policy["gates"][0]["kind"] = json!("evidence_review");
        policy["gates"][0]["source"] = json!("source");
        policy["gates"][0]["review"] = json!("review");
        let policy_bytes = serde_json::to_vec(&policy).unwrap();
        input.resolved_policy = &policy_bytes;
        let artifact = json!({"schema":"aexeo.candidate-external-evidence/v1",
            "sources":{"article":{"source":"text","review":"review"}}, "links":null});
        let bytes = serde_json::to_vec(&artifact).unwrap();
        input.external_evidence = Some(&bytes);
        let candidate = overlay(&base, input.change, &after).unwrap();
        let hash = candidate_binding(&CandidateArtifacts {
            change: input.change,
            resolved_policy: input.resolved_policy,
            host_options: input.host_options,
            evaluator: input.evaluator_identity,
            collection: None,
            external_evidence: input.external_evidence,
            base_revision_sha256: snapshot_digest(&base).unwrap(),
            candidate_revision_sha256: snapshot_digest(&candidate).unwrap(),
        })
        .unwrap();
        verdict["binding_sha256"] =
            json!(hash.iter().map(|b| format!("{b:02x}")).collect::<String>());
        verdict["verdict"]["extensions"] =
            json!({"aexeo.change-evidence-input-binding.v1":"a".repeat(64)});
        assert!(run(&verdict, &verdict, &input));
        let change = parse_change(input.change).unwrap();
        for invalid in [
            json!({"schema":"aexeo.candidate-external-evidence/v1", "sources":{}, "links":null}),
            json!({"schema":"aexeo.candidate-external-evidence/v1", "sources":{"article":{"": "x"}}, "links":null}),
            json!({"schema":"aexeo.candidate-external-evidence/v1", "sources":{"article":{"source": 1}}, "links":null}),
            json!({"schema":"aexeo.candidate-external-evidence/v1", "sources":{"article":{}}, "links":null, "extra":true}),
        ] {
            assert!(
                candidate_evidence(invalid.to_string().as_bytes(), &change, input.host_options)
                    .is_err()
            );
        }
        let tiny_options = json!({"max_entry_bytes": 3, "max_total_bytes": 12}).to_string();
        assert!(candidate_evidence(&bytes, &change, tiny_options.as_bytes()).is_err());
        let duplicate = br#"{"schema":"aexeo.candidate-external-evidence/v1","sources":{"article":{},"article":{}},"links":null}"#;
        assert!(candidate_evidence(duplicate, &change, input.host_options).is_err());
        let checks: BTreeMap<_, _> = verdict["verdict"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|check| (check["id"].as_str().unwrap(), check))
            .collect();
        let missing = json!({"article":{"source":"text"}});
        assert!(evidence_capabilities(&policy, &change, &checks, Some(&missing), None).is_err());
        assert!(evidence_capabilities(&policy, &change, &checks, None, None).is_err());
        assert!(
            evidence_capabilities(&policy, &change, &checks, Some(&artifact["sources"]), None)
                .is_ok()
        );
        verdict["verdict"]["extensions"]["aexeo.change-evidence-input-binding.v1"] =
            json!("invalid");
        assert!(!run(&verdict, &verdict, &input));
    }

    #[test]
    fn rejects_unknown_nested_fields_duplicates_and_inconsistent_results() {
        let (v, base, after, old) = fixture();
        let input = inputs(&v, &base, &after);
        for pointer in [
            "/verdict/subject",
            "/verdict/validator",
            "/verdict/summary",
            "/verdict/checks/0",
            "/verdict/subject/entries/0",
        ] {
            let mut bad = old.clone();
            bad.pointer_mut(pointer).unwrap()["unknown"] = json!(true);
            assert!(!run(&old, &bad, &input), "{pointer}");
        }
        let mut bad = old.clone();
        bad["verdict"]["summary"]["passed"] = json!(0);
        assert!(!run(&old, &bad, &input));
        bad = old.clone();
        bad["verdict"]["checks"][0]["blocking"] = json!(false);
        assert!(!run(&old, &bad, &input));
        bad = old.clone();
        bad["verdict"]["extensions"]["unknown"] = json!("x");
        assert!(!run(&old, &bad, &input));
        bad = old.clone();
        bad["verdict"]["findings"] =
            json!([{ "rule_id":"r", "severity":"info", "message":"m", "check_id":"unknown" }]);
        assert!(!run(&old, &bad, &input));
        let duplicate = old.to_string().replacen(
            "\"schema\":",
            "\"schema\":\"content-gate.bound-verdict/v2\",\"schema\":",
            1,
        );
        assert!(compare(duplicate.as_bytes(), old.to_string().as_bytes(), &input).is_err());
    }
}
