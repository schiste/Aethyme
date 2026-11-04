//! Contribution-context selection (#661) against golden vectors from an
//! independent reference implementation (`tests/fixtures/context_selection.json`):
//! ranking and tie-breaks, every budget, coverage and freshness, visibility.

use aethyme_broker::collaboration_context::{
    AnalysisSummary, Binding, Budget, Candidate, CandidateBrief, ReasonValue, SelectionQuery,
    select,
};
use aethyme_contracts::experimental_v0::analysis::Status;
use serde_json::Value;

fn status(text: &str) -> Status {
    match text {
        "complete" => Status::Complete,
        "partial" => Status::Partial,
        "truncated" => Status::Truncated,
        "stale" => Status::Stale,
        "unavailable" => Status::Unavailable,
        "incompatible" => Status::Incompatible,
        other => panic!("status {other}"),
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn query(case: &Value) -> SelectionQuery {
    let q = &case["query"];
    let budget = &q["budget"];
    let related: Vec<(Vec<u8>, String)> = q["related"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    (
                        item["path"].as_str().unwrap().as_bytes().to_vec(),
                        item["edge"].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut analysis: Vec<AnalysisSummary> = q["analysis"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| AnalysisSummary {
                    status: status(item["status"].as_str().unwrap()),
                    binding: match item["binding"].as_str().unwrap() {
                        "bound" => Binding::Bound,
                        "other_source" => Binding::OtherSource,
                        "unbound" => Binding::Unbound,
                        other => panic!("binding {other}"),
                    },
                    scoped: item.get("scoped").and_then(Value::as_bool).unwrap_or(true),
                    related: Vec::new(),
                })
                .collect()
        })
        .unwrap_or_default();
    // Related paths belong to the analysis; the reference keeps them flat.
    if let Some(first) = analysis.first_mut() {
        first.related = related;
    } else {
        assert!(related.is_empty(), "related paths need an analysis");
    }
    let flag = |name: &str| q.get(name).and_then(Value::as_bool);
    SelectionQuery {
        has_source: flag("has_source").unwrap_or(true),
        related_truncated: flag("related_truncated").unwrap_or(false),
        candidates_truncated: flag("candidates_truncated").unwrap_or(false),
        scope: strings(&q["scope"])
            .into_iter()
            .map(String::into_bytes)
            .collect(),
        analysis,
        budget: Budget {
            max_items: budget["max_items"].as_u64().unwrap() as usize,
            max_brief_tokens: budget["max_brief_tokens"].as_u64().unwrap() as usize,
            max_matched_paths: budget["max_matched_paths"].as_u64().unwrap() as usize,
            max_bytes: Budget::default().max_bytes,
        },
        unreadable: strings(&q["unreadable"]),
    }
}

fn candidates(case: &Value) -> Vec<Candidate> {
    case["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| Candidate {
            id: c["id"].as_str().unwrap().to_string(),
            seq: c["seq"].as_i64().unwrap(),
            changed: strings(&c["changed"])
                .into_iter()
                .map(String::into_bytes)
                .collect(),
            brief: c.get("brief").map(|brief| CandidateBrief {
                tokens: brief["tokens"].as_u64().unwrap() as usize,
                scope_refs: strings(&brief["scope_refs"]),
            }),
            visible: c.get("visible").and_then(Value::as_bool).unwrap_or(true),
        })
        .collect()
}

fn reason_values(values: &[ReasonValue]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|value| match value {
                ReasonValue::Path(path) => Value::String(String::from_utf8(path.clone()).unwrap()),
                ReasonValue::ScopeRef(reference) => Value::String(reference.clone()),
                ReasonValue::Edge(path, edge) => serde_json::json!({
                    "path": String::from_utf8(path.clone()).unwrap(),
                    "edge": edge,
                }),
            })
            .collect(),
    )
}

#[test]
fn selection_matches_the_reference() {
    let vectors: Value =
        serde_json::from_str(include_str!("fixtures/context_selection.json")).unwrap();
    let cases = vectors["cases"].as_array().unwrap();
    assert!(cases.len() >= 19);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let candidates = candidates(case);
        let selection = select(&query(case), &candidates);
        let expected = &case["expected"];
        let items: Vec<Value> = selection
            .items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "id": candidates[item.candidate].id,
                    "rank": item.rank,
                    "reasons": item.reasons.iter().map(|reason| serde_json::json!({
                        "kind": reason.kind.as_str(),
                        "values": reason_values(&reason.values),
                        "total": reason.total,
                    })).collect::<Vec<_>>(),
                    "brief_included": item.brief_included,
                })
            })
            .collect();
        assert_eq!(Value::Array(items), expected["items"], "{name}: items");
        assert_eq!(selection.matched as u64, expected["matched"], "{name}");
        assert_eq!(
            selection.brief_tokens as u64, expected["brief_tokens"],
            "{name}"
        );
        let truncated: Vec<&str> = selection.truncated_by.iter().copied().collect();
        assert_eq!(
            serde_json::json!(truncated),
            expected["truncated_by"],
            "{name}"
        );
        let gaps: Vec<&str> = selection.gaps.iter().map(String::as_str).collect();
        assert_eq!(serde_json::json!(gaps), expected["gaps"], "{name}");
        assert_eq!(selection.coverage(), expected["coverage"], "{name}");
        assert_eq!(
            selection.freshness().map_or(Value::Null, Value::from),
            expected["freshness"],
            "{name}"
        );
        assert_eq!(selection.limits(), expected["limits"], "{name}");
        assert_eq!(
            selection.absence_is_evidence(),
            expected["absence_is_evidence"],
            "{name}"
        );
    }
}

/// The order never depends on the order candidates arrive in.
#[test]
fn selection_is_independent_of_input_order() {
    let vectors: Value =
        serde_json::from_str(include_str!("fixtures/context_selection.json")).unwrap();
    for case in vectors["cases"].as_array().unwrap() {
        let query = query(case);
        let forward = candidates(case);
        let mut backward = forward.clone();
        backward.reverse();
        let ids = |candidates: &[Candidate]| -> Vec<String> {
            select(&query, candidates)
                .items
                .iter()
                .map(|item| candidates[item.candidate].id.clone())
                .collect()
        };
        assert_eq!(ids(&forward), ids(&backward), "{}", case["name"]);
    }
}
