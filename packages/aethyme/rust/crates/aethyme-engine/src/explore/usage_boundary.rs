//! `usage_boundary_query` intent — the dead-code path.
//!
//! Like the other V2 explore intents, this intent opens the redb graph store
//! for bounded graph/navigation discovery. It then calls the usage-boundary
//! analyzer directly because the response shape is dead-code specific and
//! because source-text caller evidence remains query-time data.
//!
//! Public surface re-exported by `explore::*`:
//! - [`UsageBoundaryParams`]
//! - [`explore_usage_boundary`]

use std::path::Path;

use crate::graph::usage_boundary::analyze_usage_boundary_scope_first_redb_with_request;
use crate::model::analysis::{AnswerStatus, DeadCodeCandidate};
use crate::store::redb::graph_store::GraphStore;

use super::{
    AnswerItem, Confidence, ConfidenceSummary, Evidence, ExploreError, ExploreRequest,
    ExploreResponse, TrustPolicy, bucket_confidence, graph_store_explore_error,
    graph_store_observability,
};

/// Parameters for `usage_boundary_query` intent.
///
/// Unlike `task_localization_query` (which derives queries from the
/// request text), usage_boundary needs explicit scope: "find unused
/// public functions IN this directory". The agent specifies the
/// directory via `--scope`; the rest have safe defaults.
#[derive(Debug, Clone)]
pub struct UsageBoundaryParams {
    /// Repo-relative directory to analyze. Required.
    pub scope: String,
    /// Repo-relative directories to search for external callers. Empty
    /// = whole repo (excluding standard ignore-dirs handled inside
    /// the engine analyzer).
    pub search_roots: Vec<String>,
    /// Include `public function` methods (true) or top-level functions
    /// only (false). Default true: most "find dead code" requests
    /// include methods.
    pub include_methods: bool,
    /// Wall-clock budget for the scan in milliseconds. The engine
    /// returns degraded results if it exceeds this. Default 10s.
    pub budget_ms: u64,
    /// Maximum evidence items per candidate symbol (internal_callers,
    /// external_callers samples). Capped to keep responses bounded.
    pub max_evidence_per_symbol: usize,
    /// Maximum number of candidates to emit in `answer[]`. The
    /// MediaWiki measurement showed the analyzer can produce 49
    /// candidates on a moderate scope, blowing the response to 20K
    /// tokens. This cap keeps responses agent-readable; consumers
    /// that need the full list can iterate via narrower scopes.
    /// Candidates are pre-sorted by status (Unused first) and
    /// confidence (highest first), so truncation keeps the strongest
    /// evidence.
    pub max_answer_items: usize,
}

impl Default for UsageBoundaryParams {
    fn default() -> Self {
        Self {
            scope: String::new(),
            search_roots: Vec::new(),
            include_methods: true,
            budget_ms: 10_000,
            max_evidence_per_symbol: 5,
            // Mirrors task_localization compact default. 25 is enough
            // for the agent to triage; full lists are available via
            // narrower --scope calls.
            max_answer_items: 25,
        }
    }
}

/// Run a `usage_boundary_query` intent. Find functions defined inside
/// `params.scope` whose only callers are also inside the scope (or
/// nowhere) — i.e. dead-code candidates relative to the rest of the
/// repo.
///
/// The analyzer reads candidate symbols/files from the local redb graph store,
/// then scans source text for caller evidence across `search_roots` (or the
/// whole repo). It runs in-process, so the binary's own startup cost is the
/// only fixed overhead.
pub fn explore_usage_boundary(
    repo: &Path,
    request: &str,
    params: &UsageBoundaryParams,
) -> Result<ExploreResponse, ExploreError> {
    if params.scope.trim().is_empty() {
        return Err(ExploreError::BadParams(
            "usage_boundary_query requires --scope (a repo-relative path)".into(),
        ));
    }
    let canonical_repo = repo
        .canonicalize()
        .map_err(|error| ExploreError::EngineAnalyzer(format!("resolve repo: {error}")))?;
    let store = GraphStore::open_read_only(&canonical_repo).map_err(graph_store_explore_error)?;
    let answer = analyze_usage_boundary_scope_first_redb_with_request(
        &canonical_repo,
        &store,
        &params.scope,
        &params.search_roots,
        params.include_methods,
        Some(request),
        Some(params.budget_ms),
        params.max_evidence_per_symbol,
    )
    .map_err(ExploreError::EngineAnalyzer)?;

    let observability = graph_store_observability(&canonical_repo);
    Ok(build_usage_boundary_response(
        request,
        params,
        answer,
        observability,
    ))
}

/// Convert a `DeadCodeAnswer` into the answer-json envelope shape the
/// agent contract expects. Mirrors Python's
/// `_explore_usage_boundary_query` output.
fn build_usage_boundary_response(
    request: &str,
    params: &UsageBoundaryParams,
    answer: crate::model::analysis::DeadCodeAnswer,
    observability: serde_json::Value,
) -> ExploreResponse {
    // Split candidates by status: Unused/Ambiguous → answer[],
    // Used → excluded[]. The agent acts on `answer[]` items only.
    //
    // Pre-sort candidates so truncation later keeps the strongest:
    // Unused > Ambiguous, then by confidence descending.
    let mut sorted: Vec<&DeadCodeCandidate> = answer.candidates.iter().collect();
    sorted.sort_by(|a, b| {
        // Unused (0) ranks ahead of Ambiguous (1) ahead of Used (2).
        // Reverse-compare confidence inside same status.
        let status_rank = |s: &AnswerStatus| match s {
            AnswerStatus::Unused => 0u8,
            AnswerStatus::Ambiguous => 1,
            AnswerStatus::Used => 2,
        };
        status_rank(&a.status)
            .cmp(&status_rank(&b.status))
            .then_with(|| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let mut answers: Vec<AnswerItem> = Vec::new();
    let mut excluded: Vec<serde_json::Value> = Vec::new();
    let mut ambiguous: Vec<serde_json::Value> = Vec::new();

    for candidate in sorted {
        match candidate.status {
            AnswerStatus::Unused | AnswerStatus::Ambiguous => {
                if answers.len() >= params.max_answer_items {
                    // Cap reached; remaining candidates are trimmed.
                    // Pre-sort guarantees what we kept is the strongest
                    // evidence by status × confidence.
                    continue;
                }
                let item = candidate_to_answer_item(candidate);
                if matches!(candidate.status, AnswerStatus::Ambiguous) {
                    ambiguous.push(serde_json::to_value(&item).unwrap_or_default());
                }
                answers.push(item);
            }
            AnswerStatus::Used => {
                excluded.push(candidate_to_value(candidate));
            }
        }
    }

    let answer_count = answers.len();
    let excluded_count = excluded.len();

    // Confidence summary.
    let answer_summary = bucket_confidence(&answers);

    // Trust policy. The dead-code analyzer is conservative by design:
    // a candidate is "Unused" only when no callers were found in any
    // search root. We treat Unused at confidence ≥0.85 as
    // `answer_candidate`. Ambiguous always gets `needs_verification`
    // because the analyzer flagged uncertainty.
    let high_confidence_unused = answer
        .candidates
        .iter()
        .filter(|c| matches!(c.status, AnswerStatus::Unused) && c.confidence >= 0.85)
        .count();
    let any_ambiguous = answer
        .candidates
        .iter()
        .any(|c| matches!(c.status, AnswerStatus::Ambiguous));
    let policy_kind: &'static str = if answer_count == 0 {
        "failed"
    } else if high_confidence_unused >= 1 && !any_ambiguous {
        "answer_candidate"
    } else {
        "needs_verification"
    };
    let safe_to_use_as_answer = matches!(policy_kind, "answer_candidate");
    let degraded = !answer.observability.degraded_reasons.is_empty();
    let trust_reason = match policy_kind {
        "answer_candidate" => format!(
            "Found {high_confidence_unused} high-confidence unused symbol(s) \
             with no callers in any search root."
        ),
        "failed" => "No unused or ambiguous candidates found in scope.".to_string(),
        _ if any_ambiguous => "Some candidates are ambiguous (callers \
                                detected but uncertain). Verify each before \
                                deletion."
            .to_string(),
        _ => "Candidates found but confidence is below the answer threshold. \
              Verify before treating as removable."
            .to_string(),
    };
    let trust_policy = TrustPolicy {
        safe_to_use_as_answer,
        safe_to_use_as_navigation: answer_count > 0,
        evidence_level: "graph+source-text-callers".into(),
        authoritative_answer_count: high_confidence_unused,
        navigation_hint_count: 0,
        degraded,
        trust_policy: policy_kind,
        reason: trust_reason,
    };

    let safe_to_use_as_navigation = trust_policy.safe_to_use_as_navigation;
    let verification_steps = vec![
        serde_json::json!({
            "step": format!(
                "Open the top answer[] item ({}) and confirm it's truly \
                 unused: search the wider codebase for the function name \
                 (some calls may use reflection or string lookups the \
                 analyzer can't see).",
                answers
                    .first()
                    .map(|a| a.target.as_str())
                    .unwrap_or("(no candidates)")
            ),
            "rationale": "The analyzer only scans direct call sites by name. \
                          Dynamic dispatch, hooks, and string-keyed lookups \
                          are not visible to it.",
        }),
        serde_json::json!({
            "step": "Re-run with --search-roots set to specific subdirs if \
                     the default whole-repo scan timed out (degraded_reasons \
                     will mention budget_exceeded).",
            "rationale": "The default 10s budget can be partial on large \
                          repos; narrowing search_roots avoids degraded \
                          output.",
        }),
    ];

    let degraded_reasons: Vec<String> = answer.observability.degraded_reasons.clone();
    let observability = usage_boundary_observability(
        observability,
        &trust_policy,
        &degraded_reasons,
        &answer.observability,
        answer_count,
        excluded_count,
    );

    let response = ExploreResponse {
        schema_version: "aethyme-explore-v1",
        mode: "explore",
        intent: "usage_boundary_query",
        intent_source: "explicit",
        status: if degraded { "degraded" } else { "complete" },
        request: ExploreRequest {
            raw: request.to_string(),
            parameters: serde_json::json!({
                "scope": params.scope,
                "search_roots": params.search_roots,
                "include_methods": params.include_methods,
                "budget_ms": params.budget_ms,
                "max_evidence_per_symbol": params.max_evidence_per_symbol,
            }),
        },
        answer: answers,
        navigation_hints: Vec::new(),
        excluded,
        ambiguous,
        subsystems: Vec::new(),
        evidence: Evidence {
            answer_count,
            navigation_hint_count: 0,
            excluded_count,
        },
        confidence: Confidence {
            overall: None,
            answer_summary,
            excluded_summary: ConfidenceSummary::default(),
            analyzed_summary: serde_json::json!({
                "graph_counts": answer.observability.graph_counts,
                "fact_counts": answer.observability.fact_counts,
                "confidence_summary": answer.observability.confidence_summary,
            }),
        },
        safe_to_use_as_answer,
        safe_to_use_as_navigation,
        trust_policy,
        degraded_reasons,
        verification_steps,
        next_actions: vec![
            "Use answer[] as the primary task result (Unused + Ambiguous candidates).".into(),
            "Cross-reference each candidate with git log to confirm it's not \
             called from a deleted/renamed code path."
                .into(),
        ],
        available_specialized_intents: vec![
            "task_localization_query",
            "behavior_localization_query",
        ],
        output_chars_estimate: 0,
        truncated: false,
        // Dead-code eval scoring reads `output_adapters.dead_code_eval_json.unused_functions`
        // directly — this adapter is the SCORER input, not just verbose
        // diagnostics. Always emit (no detail gate) for usage_boundary;
        // omitting it would silently break the eval pipeline. Mirrors
        // Python at cli.py:4700, which also emits unconditionally for
        // this intent.
        output_adapters: Some(serde_json::json!({
            "dead_code_eval_json": {
                "unused_functions": answer
                    .candidates
                    .iter()
                    .filter(|c| matches!(c.status, AnswerStatus::Unused))
                    .map(|c| serde_json::json!({
                        "name": c.function.name,
                        "defined_in": c.function.defined_in,
                        "confidence": c.confidence,
                    }))
                    .collect::<Vec<_>>(),
            }
        })),
        resolved_parameters: Some(serde_json::json!({
            "scope": params.scope,
            "search_roots": params.search_roots,
            "include_methods": params.include_methods,
            "budget_ms": params.budget_ms,
            "max_evidence_per_symbol": params.max_evidence_per_symbol,
            "max_answer_items": params.max_answer_items,
        })),
        observability: Some(observability),
    };
    super::response_with_output_estimate(response)
}

fn usage_boundary_observability(
    mut observability: serde_json::Value,
    trust_policy: &TrustPolicy,
    degraded_reasons: &[String],
    analyzer_observability: &crate::model::analysis::DeadCodeObservability,
    answer_count: usize,
    excluded_count: usize,
) -> serde_json::Value {
    let graph_status = observability
        .get("graph_freshness")
        .or_else(|| observability.get("graph_store"))
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string();
    let surface_flow_status = observability
        .get("surface_flow_graph")
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string();
    let fresh_enough = graph_status == "fresh";
    let complete_enough = fresh_enough
        && !degraded_reasons.iter().any(|reason| {
            reason.contains("budget_exceeded")
                || reason.contains("redb_")
                || reason.contains("search_root_missing")
        });
    let explainable = answer_count > 0
        || excluded_count > 0
        || analyzer_observability.fact_counts.public_functions > 0;
    let answer_safe_after_observability =
        trust_policy.safe_to_use_as_answer && complete_enough && explainable;
    let navigation_only_after_observability =
        trust_policy.safe_to_use_as_navigation && !answer_safe_after_observability;
    let mode = if answer_safe_after_observability {
        "answer_safe"
    } else if navigation_only_after_observability {
        "navigation_only"
    } else {
        "failed"
    };

    let mut top_signals_used = vec![
        serde_json::json!({"signal": "usage_boundary_analyzer", "count": 1}),
        serde_json::json!({"signal": "redb_seed_discovery", "count": analyzer_observability.fact_counts.public_functions}),
        serde_json::json!({"signal": "source_text_usage_scan", "count": analyzer_observability.fact_counts.usage_facts}),
    ];
    if analyzer_observability.fact_counts.docs_config_references > 0 {
        top_signals_used.push(serde_json::json!({
            "signal": "docs_config_references",
            "count": analyzer_observability.fact_counts.docs_config_references,
        }));
    }
    if analyzer_observability.fact_counts.external_callers > 0 {
        top_signals_used.push(serde_json::json!({
            "signal": "external_caller_evidence",
            "count": analyzer_observability.fact_counts.external_callers,
        }));
    }
    if analyzer_observability.fact_counts.internal_callers > 0 {
        top_signals_used.push(serde_json::json!({
            "signal": "internal_caller_evidence",
            "count": analyzer_observability.fact_counts.internal_callers,
        }));
    }

    let mut top_signals_absent = Vec::new();
    if !fresh_enough {
        top_signals_absent.push(serde_json::json!({
            "signal": "fresh_graph_store",
            "reason": "The redb graph store is missing, stale, or freshness could not be proven.",
        }));
    }
    if analyzer_observability.fact_counts.external_callers == 0 {
        top_signals_absent.push(serde_json::json!({
            "signal": "external_caller_evidence",
            "reason": "No external caller evidence was found for the analyzed scope.",
        }));
    }
    if analyzer_observability.fact_counts.docs_config_references == 0 {
        top_signals_absent.push(serde_json::json!({
            "signal": "docs_config_references",
            "reason": "No docs/config references were found for the analyzed symbols.",
        }));
    }
    if degraded_reasons
        .iter()
        .any(|reason| reason.contains("budget_exceeded"))
    {
        top_signals_absent.push(serde_json::json!({
            "signal": "complete_usage_scan",
            "reason": "The usage-boundary analyzer reported a budget-exceeded degradation.",
        }));
    }

    if let Some(obj) = observability.as_object_mut() {
        obj.insert(
            "usage_boundary_analyzer".into(),
            serde_json::json!({
                "graph_counts": analyzer_observability.graph_counts,
                "fact_counts": analyzer_observability.fact_counts,
                "confidence_summary": analyzer_observability.confidence_summary,
                "degraded_reasons": degraded_reasons,
            }),
        );
        obj.insert(
            "ranking_explainability".into(),
            serde_json::json!({
                "degraded_ranking_reasons": degraded_reasons,
                "top_signals_used": top_signals_used,
                "top_signals_absent": top_signals_absent,
                "subsystem_ambiguous": false,
            }),
        );
        obj.insert(
            "answer_safety".into(),
            serde_json::json!({
                "mode": mode,
                "answer_safe_by_evidence": trust_policy.safe_to_use_as_answer,
                "answer_safe_after_observability": answer_safe_after_observability,
                "navigation_only_after_observability": navigation_only_after_observability,
                "trust_policy": trust_policy.trust_policy,
                "evidence_level": trust_policy.evidence_level,
                "reason": trust_policy.reason,
            }),
        );
        obj.insert(
            "readiness".into(),
            serde_json::json!({
                "status": mode,
                "fresh_enough": fresh_enough,
                "complete_enough": complete_enough,
                "surface_flow_relevant": false,
                "surface_flow_complete": true,
                "explainable": explainable,
                "answer_safe_by_evidence": trust_policy.safe_to_use_as_answer,
                "answer_safe_after_observability": answer_safe_after_observability,
                "navigation_only_after_observability": navigation_only_after_observability,
                "graph_freshness_status": graph_status,
                "surface_flow_graph_status": surface_flow_status,
                "degraded_reasons": degraded_reasons,
            }),
        );
    }

    observability
}

fn candidate_to_answer_item(c: &DeadCodeCandidate) -> AnswerItem {
    let status = match c.status {
        AnswerStatus::Unused => "Unused",
        AnswerStatus::Ambiguous => "Ambiguous",
        AnswerStatus::Used => "Used",
    };
    AnswerItem {
        kind: "unused_function".into(),
        target: c.function.name.clone(),
        path: Some(c.function.defined_in.clone()),
        status: status.into(),
        confidence: c.confidence as f64,
        reason: c.rationale.clone(),
        role: "removal_candidate".into(),
        evidence: serde_json::json!({
            "source": "usage-boundary-analyzer",
            "function": {
                "name": c.function.name,
                "file": c.function.defined_in,
                "line": c.function.line,
                "qualified_name": c.function.qualified_name,
                "language": c.function.language,
            },
            "internal_callers": c.evidence.internal_callers,
            "external_callers": c.evidence.external_callers,
            "docs_config_references": c.evidence.docs_config_references,
            "ambiguity": c.ambiguity,
        }),
    }
}

fn candidate_to_value(c: &DeadCodeCandidate) -> serde_json::Value {
    serde_json::json!({
        "kind": "used_function",
        "function_name": c.function.name,
        "defined_in": c.function.defined_in,
        "status": match c.status {
            AnswerStatus::Unused => "Unused",
            AnswerStatus::Ambiguous => "Ambiguous",
            AnswerStatus::Used => "Used",
        },
        "confidence": c.confidence,
        "rationale": c.rationale,
        "internal_callers": c.evidence.internal_callers,
        "external_callers": c.evidence.external_callers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::analysis::{
        DeadCodeAnswer, DeadCodeConfidenceSummary, DeadCodeFactCounts, DeadCodeGraphCounts,
        DeadCodeObservability, DeadCodeQuery, DeadCodeSummary, EvidencePacket, ExposureKind,
        FunctionFact,
    };

    #[test]
    fn usage_boundary_response_emits_enterprise_observability() {
        let function = FunctionFact {
            id: "fn:demo:unused".into(),
            name: "unused_widget".into(),
            qualified_name: "Demo::unused_widget".into(),
            defined_in: "src/demo.php".into(),
            line: 7,
            language: "php".into(),
            parent_class: None,
            exposure_kind: ExposureKind::ExportedTopLevel,
        };
        let answer = DeadCodeAnswer {
            analyzer: "usage-boundary".into(),
            version: "test".into(),
            query: DeadCodeQuery {
                scope: "src".into(),
                searched_roots: vec!["src".into(), "tests".into()],
                include_methods: true,
            },
            candidates: vec![DeadCodeCandidate {
                function,
                status: AnswerStatus::Unused,
                confidence: 0.93,
                evidence: EvidencePacket {
                    searched_roots: vec!["src".into(), "tests".into()],
                    internal_callers: Vec::new(),
                    external_callers: Vec::new(),
                    docs_config_references: Vec::new(),
                },
                ambiguity: Vec::new(),
                rationale: "No callers found.".into(),
            }],
            excluded: Vec::new(),
            summary: DeadCodeSummary {
                total_candidates: 1,
                unused: 1,
                ambiguous: 0,
                used: 0,
            },
            observability: DeadCodeObservability {
                graph_counts: DeadCodeGraphCounts {
                    functions: 1,
                    docs: 0,
                    configs: 0,
                    edges: 0,
                },
                fact_counts: DeadCodeFactCounts {
                    public_functions: 1,
                    usage_facts: 1,
                    internal_callers: 0,
                    external_callers: 0,
                    docs_config_references: 0,
                },
                confidence_summary: DeadCodeConfidenceSummary {
                    high: 1,
                    medium: 0,
                    low: 0,
                    min: Some(0.93),
                    max: Some(0.93),
                },
                degraded_reasons: Vec::new(),
            },
        };

        let response = build_usage_boundary_response(
            "find unused functions",
            &UsageBoundaryParams {
                scope: "src".into(),
                ..UsageBoundaryParams::default()
            },
            answer,
            serde_json::json!({
                "graph_freshness": {"status": "fresh", "fresh": true},
                "graph_store": {"status": "fresh"},
                "surface_flow_graph": {"status": "no_surface_signals"},
                "missing_expected_surfaces": [],
            }),
        );

        let observability = response
            .observability
            .as_ref()
            .expect("usage boundary emits observability");
        assert_eq!(
            observability
                .get("answer_safety")
                .and_then(|value| value.get("mode"))
                .and_then(|value| value.as_str()),
            Some("answer_safe")
        );
        assert_eq!(
            observability
                .get("usage_boundary_analyzer")
                .and_then(|value| value.get("fact_counts"))
                .and_then(|value| value.get("public_functions"))
                .and_then(|value| value.as_u64()),
            Some(1)
        );
        let used_signals = observability
            .get("ranking_explainability")
            .and_then(|value| value.get("top_signals_used"))
            .and_then(|value| value.as_array())
            .expect("used signals");
        assert!(
            used_signals.iter().any(|value| {
                value.get("signal").and_then(|signal| signal.as_str())
                    == Some("redb_seed_discovery")
            }),
            "expected redb seed signal in {used_signals:?}"
        );
    }
}
