//! The graph-free answer-json envelope: graph-store errors mapped to
//! `ExploreError`, and the bounded source-search response built when the
//! optional graph store cannot answer.

use super::*;

pub(super) fn graph_store_explore_error(error: GraphStoreError) -> ExploreError {
    let (status, reason) = match error {
        GraphStoreError::MissingGraphStore { .. } => (
            "missing",
            "the local derived graph store has not been materialized".into(),
        ),
        GraphStoreError::SchemaMismatch { found, expected } => (
            "incompatible",
            format!("graph store schema {found} does not match required schema {expected}"),
        ),
        GraphStoreError::IncompatibleRedbFileFormat { found, .. } => (
            "incompatible",
            format!("graph store file format {found} is incompatible with this runtime"),
        ),
        GraphStoreError::Io(_) => (
            "unavailable",
            "the local graph store could not be read".into(),
        ),
        GraphStoreError::Db(_) => (
            "unavailable",
            "the local graph database could not be opened".into(),
        ),
        GraphStoreError::Encode(_) => (
            "unavailable",
            "the local graph store contains undecodable data".into(),
        ),
    };
    ExploreError::GraphUnavailable { status, reason }
}

/// Whether a graph-free top hit that passes [`source_fallback::answer_safety`]
/// is promoted to `answer[]` with `safe_to_use_as_answer: true`.
///
/// Off: on the 2026-09-24 held-out run the rule marked 3 answers safe and one
/// was wrong, and agents act on this flag without verifying. The rule's
/// verdict is still reported in observability (`source_fallback.answer_safety`)
/// so its precision can be measured; turn this on only once that precision is
/// at least 95% on the development set.
const GRAPH_FREE_ANSWER_PROMOTION: bool = false;

/// Evidence keys a graph-free hint keeps in the default compact profile.
/// `line_refs` anchors `verify-targets` and is what an agent reads next; the
/// matched terms, role and defining symbol are already spelled out in the
/// hint's `reason`. `answer_rule` names why a promoted hit is answer-safe.
const COMPACT_HINT_EVIDENCE_KEYS: [&str; 2] = ["line_refs", "answer_rule"];

/// Shape a graph-free envelope for the requested output profile.
///
/// [`graph_unavailable_response`] builds every field; this trims what an
/// agent does not need to act on, so the default call stays small:
///
/// - `--detail compact` without `--show-observability` (the default agent
///   call): each hint's `evidence` keeps only `line_refs`, and
///   `observability` keeps only `readiness`.
/// - `--show-observability`, or `--detail standard`: the full per-hint
///   evidence (`score`, `term_coverage`, `matched_terms`, `path_role`,
///   `symbol_match`, ...). `--show-observability` also keeps
///   `observability.source_fallback` and `graph_store`.
/// - `--detail full`: everything, including the scoring-model constants in
///   `observability.source_fallback.scoring`.
///
/// Ranking, hint order and line spans are identical in every profile; only
/// diagnostics are omitted. `output_chars_estimate` is recomputed.
pub fn project_graph_free_output(
    mut response: ExploreResponse,
    detail: Detail,
    show_observability: bool,
) -> ExploreResponse {
    let full = matches!(detail, Detail::Full);
    let keep_hint_diagnostics = show_observability || !matches!(detail, Detail::Compact);
    if !keep_hint_diagnostics {
        for item in response
            .answer
            .iter_mut()
            .chain(response.navigation_hints.iter_mut())
        {
            if let Some(evidence) = item.evidence.as_object_mut() {
                evidence.retain(|key, _| COMPACT_HINT_EVIDENCE_KEYS.contains(&key.as_str()));
            }
        }
    }
    if !full && let Some(observability) = response.observability.as_mut() {
        if show_observability {
            if let Some(source) = observability
                .get_mut("source_fallback")
                .and_then(|value| value.as_object_mut())
            {
                source.remove("scoring");
            }
        } else if let Some(map) = observability.as_object_mut() {
            map.retain(|key, _| key == "readiness");
        }
    }
    response_with_output_estimate(response)
}

/// Build the stable answer-json contract for a repository whose optional
/// local graph store cannot currently answer. A bounded full-content source
/// search supplies ranked navigation hints with line spans. Graph-free hits
/// are navigation only while [`GRAPH_FREE_ANSWER_PROMOTION`] is off; they are
/// never caller or impact evidence.
pub fn graph_unavailable_response(
    repo: &Path,
    request: &str,
    intent: &'static str,
    intent_source: &'static str,
    status: &'static str,
    reason: String,
) -> ExploreResponse {
    graph_unavailable_response_with(
        repo,
        request,
        intent,
        intent_source,
        status,
        reason,
        &SourceSearchOptions::default(),
    )
}

/// [`graph_unavailable_response`] with explicit source-search knobs (hit
/// count, wall-time budget, symbol-index cache location).
pub fn graph_unavailable_response_with(
    repo: &Path,
    request: &str,
    intent: &'static str,
    intent_source: &'static str,
    status: &'static str,
    reason: String,
    options: &SourceSearchOptions,
) -> ExploreResponse {
    let mut fallback = source_fallback::inspect_with(repo, request, options);
    let source_observability = fallback.observability();
    let mut hints = std::mem::take(&mut fallback.hints);
    let subsystems = std::mem::take(&mut fallback.subsystems);
    let safety = fallback.answer_safety;
    let answer = if GRAPH_FREE_ANSWER_PROMOTION && safety.safe && !hints.is_empty() {
        let mut top = hints.remove(0);
        top.status = "content_evidence".into();
        top.evidence["answer_rule"] = serde_json::json!(safety.rule);
        vec![top]
    } else {
        Vec::new()
    };
    let answer_count = answer.len();
    let hint_count = hints.len();
    let safe_to_use_as_answer = answer_count > 0;
    let safe_to_use_as_navigation = answer_count + hint_count > 0;
    let trust_reason = if safe_to_use_as_answer {
        format!(
            "The graph is unavailable. The top source-search hit passed the `{}` content-evidence rule over a complete search: it is source-navigation evidence for where the request is defined, not caller or impact evidence.",
            safety.rule
        )
    } else {
        format!(
            "The graph is unavailable. Ranked source-search hints are navigation only (content-evidence rule: `{}`), not caller or impact evidence.",
            safety.rule
        )
    };
    let policy = aethyme_graph_storage::GraphIntegrityPolicy::load(repo);
    let next_action = match policy {
        Ok(policy) if policy.enforces_committed_fragments() => {
            "Run `aethyme graph materialize --repo .`; if committed fragments are stale, review `aethyme graph refresh plan --repo . --diff`."
        }
        Ok(_) => {
            "Graph support is optional. Continue with bounded source inspection, or explicitly enroll with `aethyme deploy --repo . --with-graph`."
        }
        Err(_) => {
            "The repository graph policy is invalid. Run `aethyme graph status --repo .` for the exact diagnosis; bounded source inspection remains available."
        }
    };
    response_with_output_estimate(ExploreResponse {
        schema_version: "aethyme-explore-v1",
        mode: "explore",
        intent,
        intent_source,
        status: "degraded",
        request: ExploreRequest {
            raw: request.to_string(),
            parameters: serde_json::json!({}),
        },
        confidence: Confidence {
            overall: answer.first().map(|item| item.confidence),
            answer_summary: bucket_confidence(&answer),
            excluded_summary: ConfidenceSummary::default(),
            analyzed_summary: serde_json::json!({"graph_available": false}),
        },
        answer,
        navigation_hints: hints,
        excluded: Vec::new(),
        ambiguous: Vec::new(),
        subsystems,
        evidence: Evidence {
            answer_count,
            navigation_hint_count: hint_count,
            excluded_count: 0,
        },
        safe_to_use_as_answer,
        safe_to_use_as_navigation,
        trust_policy: TrustPolicy {
            safe_to_use_as_answer,
            safe_to_use_as_navigation,
            evidence_level: if safe_to_use_as_navigation {
                "source_navigation"
            } else {
                "none"
            }
            .into(),
            authoritative_answer_count: answer_count,
            navigation_hint_count: hint_count,
            degraded: true,
            trust_policy: if safe_to_use_as_answer {
                "answer_candidate"
            } else {
                "verify_before_use"
            },
            reason: trust_reason,
        },
        degraded_reasons: vec![format!("graph_store_{status}")],
        verification_steps: vec![serde_json::json!({
            "kind": "manual_source_inspection",
            "reason": "Verify the ranked source spans; no graph-backed semantic claims are available"
        })],
        next_actions: vec![next_action.into()],
        available_specialized_intents: vec!["behavior_localization_query", "usage_boundary_query"],
        output_chars_estimate: 0,
        truncated: fallback.truncated,
        output_adapters: None,
        resolved_parameters: None,
        observability: Some(serde_json::json!({
            "readiness": if fallback.complete {
                serde_json::json!({
                    "status": "ready",
                    "reason": "source_search_complete",
                    "mode": "bounded_content_search"
                })
            } else {
                serde_json::json!({
                    "status": "partial",
                    "reason": fallback.incomplete_reason,
                    "mode": "bounded_content_search"
                })
            },
            "source_fallback": source_observability,
            "graph_store": {
                "status": status,
                "source_of_truth": "graph_fragments",
                "derived_query_artifact": "redb_graph_store",
                "reason": reason
            }
        })),
    })
}
