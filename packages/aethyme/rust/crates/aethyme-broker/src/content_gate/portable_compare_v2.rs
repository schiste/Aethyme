//! Experimental offline v2 comparison, not a publication authorization.
//! The caller must independently acquire and protect every input, authenticate
//! the fresh evaluator, establish inventory completeness and freshness, and
//! synchronize the final check with publication.
use super::candidate_overlay_v2::overlay;
use super::candidate_revision_v2::{Item, Snapshot, snapshot_digest};
use super::portable_v2::{CandidateArtifacts, candidate_binding};
use super::strict::{DecisionInputs, parse_change, parse_unique, strict_precheck_v1};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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
                "aexeo.html-content-scope.v1",
            ],
        )?;
        let offline = ext.get("aexeo.offline-input-binding.v1");
        let observed = ext.get("aexeo.observed-link-binding.v1");
        let valid_offline = offline.and_then(Value::as_str).is_some_and(valid_sha);
        let valid_observed = observed.is_some_and(|value| {
            closed(value, &["inputs_sha256", "observations_sha256"]).is_ok()
                && value.as_object().is_some_and(|m| m.len() == 2)
                && ["inputs_sha256", "observations_sha256"]
                    .iter()
                    .all(|key| value[*key].as_str().is_some_and(valid_sha))
        });
        if (valid_offline as u8 + valid_observed as u8) != 1
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
) -> Result<(), &'static str> {
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
        let collection = has_collection && kind == "unique_collection_metadata";
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
                if checks
                    .get(check_id.as_str())
                    .is_none_or(|c| c["blocking"].as_bool() != Some(blocking))
                {
                    return Err("resolved policy check is missing or has inconsistent blocking");
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
    if let Some(bytes) = input.collection {
        validate_collection(bytes, &candidate, &change)?;
    }
    if let Some(bytes) = input.external_evidence {
        validate_link_evidence(bytes, &change)?;
    }
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
    for env in [&previous, &fresh] {
        if env["binding_sha256"] != binding_hex {
            return Err("candidate input binding differs");
        }
        let verdict = &env["verdict"];
        let observed = verdict
            .get("extensions")
            .and_then(|ext| ext.get("aexeo.observed-link-binding.v1"))
            .is_some();
        if observed != input.external_evidence.is_some() {
            return Err("link evidence does not match the verdict profile");
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
        coverage = Some(policy_coverage(
            &policy,
            &change,
            input.collection.is_some(),
            &checks,
        )?);
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
        assert_eq!(policy_coverage(&policy, &change, false, &checks), Ok((1, 1)));
        policy["gates"] = json!([]);
        assert_eq!(
            policy_coverage(&policy, &change, false, &checks),
            Err("empty resolved policy")
        );
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
    fn rejects_malformed_link_evidence_and_profile_mismatches() {
        let (v, base, after, mut verdict) = fixture();
        let change = parse_change(v["artifacts"]["change"].as_str().unwrap().as_bytes()).unwrap();
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
