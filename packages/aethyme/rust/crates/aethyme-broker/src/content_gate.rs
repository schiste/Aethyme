//! A partial, fail-closed precheck for the content-gate v1 subject.
//!
//! Success is not permission to publish: validate the complete JSON schemas and
//! independently bind all other decision inputs at the publication boundary.
pub mod strict;

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
struct Base {
    kind: String,
    revision: String,
}
#[derive(Deserialize)]
struct Change {
    schema: String,
    change_id: String,
    base: Base,
    entries: Vec<ChangeEntry>,
}
#[derive(Deserialize)]
struct ChangeEntry {
    entity_id: String,
    operation: String,
    after: Option<After>,
}
#[derive(Deserialize)]
struct After {
    sha256: String,
}
#[derive(Deserialize)]
struct Verdict {
    schema: String,
    status: String,
    subject: Subject,
    checks: Vec<Check>,
}
#[derive(Deserialize)]
struct Subject {
    change_id: String,
    base: Base,
    entries: Vec<SubjectEntry>,
}
#[derive(Deserialize)]
struct SubjectEntry {
    entity_id: String,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    sha256: Option<String>,
}
#[derive(Deserialize)]
struct Check {
    status: String,
    blocking: bool,
}

/// Success only proves the bounded checks described by [`precheck_v1`].
/// It is intentionally not a publication authorization.
#[derive(Debug, PartialEq, Eq)]
pub struct SubjectPrechecked(());

/// Check execution, status, exact subject and current content bytes.
///
/// `current` contains one entry for each entity: `Some(bytes)` for creates,
/// updates and renames, `None` for deletes. The caller must obtain this map
/// from the actual publication candidate. Extra, missing and duplicate entity
/// identities are rejected. No generic tree-hash cache may replace this call.
///
/// This does not validate the complete v1 JSON schemas, authenticate the
/// validator, validate its policy, or bind metadata, configuration, collection
/// snapshots or external observations. Before publication the host must check
/// those independently and repeat this precheck against the exact current
/// candidate, under whatever concurrency control its publication system needs.
pub fn precheck_v1(
    change_json: &[u8],
    verdict_json: &[u8],
    exit_code: Option<i32>,
    timed_out: bool,
    current: &BTreeMap<String, Option<Vec<u8>>>,
) -> Result<SubjectPrechecked, &'static str> {
    if timed_out || exit_code != Some(0) {
        return Err("validator did not exit successfully within its deadline");
    }
    let change: Change =
        serde_json::from_slice(change_json).map_err(|_| "invalid change document")?;
    let verdict: Verdict =
        serde_json::from_slice(verdict_json).map_err(|_| "invalid verdict document")?;
    if change.schema != "content-gate.change/v1" || verdict.schema != "content-gate.verdict/v1" {
        return Err("unsupported schema");
    }
    if verdict.status != "pass" {
        return Err("verdict is not pass");
    }
    if verdict.checks.iter().any(|c| {
        !matches!(c.status.as_str(), "passed" | "failed" | "not_checked")
            || (c.blocking && c.status != "passed")
    }) {
        return Err("invalid or non-passing blocking check");
    }
    if change.change_id.is_empty()
        || change.base.revision.is_empty()
        || !matches!(change.base.kind.as_str(), "git" | "opaque")
        || change.change_id != verdict.subject.change_id
        || change.base.kind != verdict.subject.base.kind
        || change.base.revision != verdict.subject.base.revision
    {
        return Err("subject change or base mismatch");
    }
    let mut expected = BTreeMap::new();
    for entry in change.entries {
        if entry.entity_id.is_empty() {
            return Err("empty entity id");
        }
        let sha = match (entry.operation.as_str(), entry.after) {
            ("delete", None) => None,
            ("create" | "update" | "rename", Some(after)) if valid_sha(&after.sha256) => {
                Some(after.sha256)
            }
            _ => return Err("invalid operation or content digest"),
        };
        if expected.insert(entry.entity_id, sha).is_some() {
            return Err("duplicate change entity");
        }
    }
    if expected.is_empty() {
        return Err("empty change");
    }
    let mut actual = BTreeMap::new();
    for entry in verdict.subject.entries {
        if entry.entity_id.is_empty()
            || entry.sha256.as_ref().is_some_and(|s| !valid_sha(s))
            || actual.insert(entry.entity_id, entry.sha256).is_some()
        {
            return Err("invalid or duplicate verdict entity");
        }
    }
    if actual != expected {
        return Err("verdict subject entries mismatch");
    }
    if current.keys().collect::<BTreeSet<_>>() != expected.keys().collect::<BTreeSet<_>>() {
        return Err("current candidate entities mismatch");
    }
    for (id, sha) in expected {
        let actual_sha = current[&id]
            .as_ref()
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
        if sha != actual_sha {
            return Err("current candidate content mismatch");
        }
    }
    Ok(SubjectPrechecked(()))
}

fn valid_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn fixtures() -> (Value, Value, BTreeMap<String, Option<Vec<u8>>>) {
        let digest = format!("{:x}", Sha256::digest(b"hello"));
        (
            json!({"schema":"content-gate.change/v1","change_id":"c","base":{"kind":"opaque","revision":"r"},"entries":[{"entity_id":"a","operation":"update","after":{"sha256":digest}}]}),
            json!({"schema":"content-gate.verdict/v1","status":"pass","subject":{"change_id":"c","base":{"kind":"opaque","revision":"r"},"entries":[{"entity_id":"a","sha256":digest}]},"checks":[{"status":"passed","blocking":true}]}),
            BTreeMap::from([("a".into(), Some(b"hello".to_vec()))]),
        )
    }
    fn run(
        c: &Value,
        v: &Value,
        exit: Option<i32>,
        timeout: bool,
        bytes: &BTreeMap<String, Option<Vec<u8>>>,
    ) -> bool {
        precheck_v1(
            c.to_string().as_bytes(),
            v.to_string().as_bytes(),
            exit,
            timeout,
            bytes,
        )
        .is_ok()
    }
    #[test]
    fn precheck_rejects_stale_missing_and_inconsistent_evidence() {
        let (c, v, bytes) = fixtures();
        assert!(run(&c, &v, Some(0), false, &bytes));
        for exit in [None, Some(1), Some(2), Some(70)] {
            assert!(!run(&c, &v, exit, false, &bytes));
        }
        assert!(!run(&c, &v, Some(0), true, &bytes));
        assert!(!precheck_v1(b"{}", b"", Some(0), false, &bytes).is_ok());
        for (pointer, value) in [
            ("/status", json!("not_checked")),
            ("/status", json!("fail")),
            ("/schema", json!("content-gate.verdict/v2")),
            ("/subject/change_id", json!("other")),
            ("/subject/base/revision", json!("other")),
            ("/subject/entries/0/sha256", json!("0".repeat(64))),
            ("/checks/0/status", json!("failed")),
        ] {
            let mut changed = v.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(!run(&c, &changed, Some(0), false, &bytes), "{pointer}");
        }
        let mut changed = v.clone();
        let duplicate = changed["subject"]["entries"][0].clone();
        changed["subject"]["entries"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(!run(&c, &changed, Some(0), false, &bytes));
        assert!(!run(&c, &v, Some(0), false, &BTreeMap::new()));
        assert!(!run(
            &c,
            &v,
            Some(0),
            false,
            &BTreeMap::from([("a".into(), Some(b"changed".to_vec()))])
        ));
    }
    #[test]
    fn deletes_are_bound_to_absence_and_advisory_failures_are_allowed() {
        let (mut c, mut v, _) = fixtures();
        c["entries"][0] = json!({"entity_id":"a","operation":"delete"});
        v["subject"]["entries"][0]["sha256"] = Value::Null;
        v["checks"][0] = json!({"status":"failed","blocking":false});
        assert!(run(
            &c,
            &v,
            Some(0),
            false,
            &BTreeMap::from([("a".into(), None)])
        ));
        assert!(!run(
            &c,
            &v,
            Some(0),
            false,
            &BTreeMap::from([("a".into(), Some(vec![]))])
        ));
    }
}
