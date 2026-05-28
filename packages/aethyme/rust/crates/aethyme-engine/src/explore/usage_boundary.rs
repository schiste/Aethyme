//! `usage_boundary_query` intent — the dead-code path.
//!
//! Unlike `task_localization_query` and `behavior_localization_query`
//! (which go through the engine daemon), this intent calls
//! `analyze_usage_boundary_scope_first` directly. Different shape,
//! different orchestration; lifting it out of the main `explore.rs`
//! body keeps the daemon-routed code paths legible.
//!
//! Public surface re-exported by `explore::*`:
//! - [`UsageBoundaryParams`]
//! - [`explore_usage_boundary`]

use std::path::Path;

use crate::graph::usage_boundary::analyze_usage_boundary_scope_first;
use crate::model::analysis::{AnswerStatus, DeadCodeCandidate};

use super::{
    AnswerItem, Confidence, ConfidenceSummary, Evidence, ExploreError, ExploreRequest,
    ExploreResponse, TrustPolicy, bucket_confidence,
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
            // narrower --scope or the Python orchestrator.
            max_answer_items: 25,
        }
    }
}

/// Run a `usage_boundary_query` intent. Find functions defined inside
/// `params.scope` whose only callers are also inside the scope (or
/// nowhere) — i.e. dead-code candidates relative to the rest of the
/// repo.
///
/// This intent does NOT use the engine daemon. The analyzer walks the
/// scope filesystem and scans source text for callers across
/// `search_roots` (or the whole repo). It runs in-process, so the
/// binary's own startup cost is the only fixed overhead.
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
    let answer = analyze_usage_boundary_scope_first(
        repo,
        &params.scope,
        &params.search_roots,
        params.include_methods,
        Some(params.budget_ms),
        params.max_evidence_per_symbol,
    )
    .map_err(ExploreError::EngineAnalyzer)?;

    Ok(build_usage_boundary_response(request, params, answer))
}

/// Convert a `DeadCodeAnswer` into the answer-json envelope shape the
/// agent contract expects. Mirrors Python's
/// `_explore_usage_boundary_query` output.
fn build_usage_boundary_response(
    request: &str,
    params: &UsageBoundaryParams,
    answer: crate::model::analysis::DeadCodeAnswer,
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

    ExploreResponse {
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
    }
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
