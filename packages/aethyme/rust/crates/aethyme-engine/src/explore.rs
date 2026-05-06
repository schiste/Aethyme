//! Native Rust orchestration for `aethyme explore`.
//!
//! Session 1 of the Rust port (see roadmap discussion in commit message of
//! 49a7beb's parent thread): the simplest happy path that crosses every
//! layer end-to-end. The scope is deliberately narrow:
//!
//! - Only `task_localization_query` intent
//! - Only the daemon-routed path (no fallback to subprocess engine here —
//!   the caller decides whether to fall through to Python on failure)
//! - Synthesizes `answer[]` items from anchors + in-scope files only
//! - Stub `verification_steps`, `excluded`, `ambiguous`
//! - Conservative `trust_policy`: always `needs_verification` because we
//!   don't yet have the symbol/text/callsite passes that justify
//!   `answer_candidate`
//!
//! Subsequent sessions add: symbol search (B2 in plan), source-text grep
//! (B3), full trust policy + verification (B4), behavior_localization (B5).
//!
//! Wire shape
//! ----------
//! Output JSON matches `aethyme-explore-v1` schema produced by the Python
//! `_explore_task_localization_query` at compact detail. A consumer that
//! reads `answer[]` + `safe_to_use_as_answer` + `trust_policy` works
//! identically against either implementation.

use std::path::Path;

use serde::Serialize;

use crate::daemon;

// ── public envelope ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ExploreResponse {
    pub schema_version: &'static str,
    pub mode: &'static str,
    pub intent: &'static str,
    pub intent_source: &'static str,
    pub status: &'static str,
    pub request: ExploreRequest,
    pub answer: Vec<AnswerItem>,
    pub navigation_hints: Vec<AnswerItem>,
    pub excluded: Vec<serde_json::Value>,
    pub ambiguous: Vec<serde_json::Value>,
    pub evidence: Evidence,
    pub confidence: Confidence,
    pub safe_to_use_as_answer: bool,
    pub safe_to_use_as_navigation: bool,
    pub trust_policy: TrustPolicy,
    pub degraded_reasons: Vec<String>,
    pub verification_steps: Vec<serde_json::Value>,
    pub next_actions: Vec<String>,
    pub available_specialized_intents: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct ExploreRequest {
    pub raw: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct AnswerItem {
    pub kind: String,
    pub target: String,
    pub path: Option<String>,
    pub status: String,
    pub confidence: f64,
    pub reason: String,
    pub role: String,
    pub evidence: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct Evidence {
    pub answer_count: usize,
    pub navigation_hint_count: usize,
    pub excluded_count: usize,
}

#[derive(Debug, Serialize)]
pub struct Confidence {
    pub overall: Option<f64>,
    pub answer_summary: ConfidenceSummary,
    pub excluded_summary: ConfidenceSummary,
    pub analyzed_summary: serde_json::Value,
}

#[derive(Debug, Default, Serialize)]
pub struct ConfidenceSummary {
    pub high: usize,
    pub medium: usize,
    pub low: usize,
}

#[derive(Debug, Serialize)]
pub struct TrustPolicy {
    pub safe_to_use_as_answer: bool,
    pub safe_to_use_as_navigation: bool,
    pub evidence_level: &'static str,
    pub authoritative_answer_count: usize,
    pub navigation_hint_count: usize,
    pub degraded: bool,
    pub trust_policy: &'static str,
    pub reason: String,
}

// ── parameters ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExploreParams {
    pub max_answer_items: usize,
    /// Detail level: `compact`, `standard`, or `full`. Mirrors the Python
    /// `--detail` flag. Today only `compact` is fully implemented in the
    /// native path; standard/full fall back to Python at the call site.
    pub detail: Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Compact,
    Standard,
    Full,
}

impl Default for ExploreParams {
    fn default() -> Self {
        Self {
            max_answer_items: 5, // matches Python compact default after f1e3da5
            detail: Detail::Compact,
        }
    }
}

// ── errors ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ExploreError {
    DaemonNotRunning,
    DaemonRpc(String),
    InvalidResponse(String),
}

impl std::fmt::Display for ExploreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DaemonNotRunning => write!(f, "engine daemon not running"),
            Self::DaemonRpc(msg) => write!(f, "engine daemon rpc: {msg}"),
            Self::InvalidResponse(msg) => write!(f, "invalid daemon response: {msg}"),
        }
    }
}

impl std::error::Error for ExploreError {}

// ── orchestration entry point ───────────────────────────────────────────

/// Run a `task_localization_query` intent against the engine daemon.
///
/// Returns `Err(DaemonNotRunning)` when there's no daemon — the caller
/// is responsible for falling back (typically to the Python orchestrator).
/// Other errors indicate the daemon was reachable but the request failed.
pub fn explore_task_localization(
    repo: &Path,
    request: &str,
    params: &ExploreParams,
) -> Result<ExploreResponse, ExploreError> {
    let socket = daemon::socket_path_for(repo);
    if !socket.exists() {
        return Err(ExploreError::DaemonNotRunning);
    }

    let rpc_request = serde_json::json!({
        "command": "task-localize",
        "task": request,
    });

    let response_text = daemon::send_request(&socket, &rpc_request)
        .map_err(ExploreError::DaemonRpc)?;
    let envelope: serde_json::Value = serde_json::from_str(response_text.trim())
        .map_err(|e| ExploreError::InvalidResponse(format!("not JSON: {e}")))?;

    if envelope.get("ok") != Some(&serde_json::Value::Bool(true)) {
        let msg = envelope
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown daemon error");
        return Err(ExploreError::DaemonRpc(msg.to_string()));
    }
    let view = envelope
        .get("result")
        .ok_or_else(|| ExploreError::InvalidResponse("missing `result`".into()))?;

    Ok(build_response(request, view, params))
}

// ── response synthesis ──────────────────────────────────────────────────

/// Translate the engine daemon's `task-localize` view into the answer-json
/// envelope the agent contract expects.
fn build_response(
    request: &str,
    view: &serde_json::Value,
    params: &ExploreParams,
) -> ExploreResponse {
    let anchors = view
        .get("anchors")
        .and_then(|a| a.get("anchors"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let scope = view.get("scope");
    let in_scope_files: Vec<String> = scope
        .and_then(|s| s.get("in_scope_files"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let in_scope_areas: Vec<String> = scope
        .and_then(|s| s.get("in_scope_areas"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Synthesize answer items.
    //
    // For session 1 we trust two signals:
    //   1. anchors with `kind = "file"` are confident enough to be
    //      `answer[]` items at confidence 0.85.
    //   2. in_scope_files become `answer[]` items at confidence 0.7,
    //      bounded by max_answer_items.
    //
    // Anchors with `kind = "folder"` and in_scope_areas become
    //   `navigation_hints[]` instead of `answer[]` because the agent
    //   is asking for FILES to act on, and a folder is one click of
    //   navigation away from being actionable.
    let mut answers: Vec<AnswerItem> = Vec::new();
    let mut nav_hints: Vec<AnswerItem> = Vec::new();

    for anchor in &anchors {
        let kind = anchor.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let id = anchor.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let file = anchor.get("file").and_then(|v| v.as_str());
        let reason = anchor
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("anchor match")
            .to_string();
        match kind {
            "file" => {
                let path = file.map(String::from).or_else(|| Some(id.to_string()));
                answers.push(AnswerItem {
                    kind: "anchor".into(),
                    target: id.to_string(),
                    path,
                    status: "candidate".into(),
                    confidence: 0.85,
                    reason,
                    role: "anchor".into(),
                    evidence: serde_json::json!({
                        "source": "task-localize.anchors",
                        "anchor_kind": "file",
                    }),
                });
            }
            "folder" | "area" => {
                nav_hints.push(AnswerItem {
                    kind: "anchor_area".into(),
                    target: id.to_string(),
                    path: file.map(String::from),
                    status: "navigation_only".into(),
                    confidence: 0.6,
                    reason,
                    role: "navigation_anchor".into(),
                    evidence: serde_json::json!({
                        "source": "task-localize.anchors",
                        "anchor_kind": kind,
                    }),
                });
            }
            other if !other.is_empty() => {
                nav_hints.push(AnswerItem {
                    kind: format!("anchor_{other}"),
                    target: id.to_string(),
                    path: file.map(String::from),
                    status: "navigation_only".into(),
                    confidence: 0.55,
                    reason,
                    role: other.to_string(),
                    evidence: serde_json::json!({
                        "source": "task-localize.anchors",
                        "anchor_kind": other,
                    }),
                });
            }
            _ => {}
        }
    }

    for file in &in_scope_files {
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers.iter().any(|a| a.path.as_deref() == Some(file.as_str())) {
            continue;
        }
        answers.push(AnswerItem {
            kind: "in_scope_file".into(),
            target: file.clone(),
            path: Some(file.clone()),
            status: "candidate".into(),
            confidence: 0.7,
            reason: "Within graph-derived scope for this task".into(),
            role: "candidate".into(),
            evidence: serde_json::json!({
                "source": "task-localize.scope",
            }),
        });
    }

    for area in &in_scope_areas {
        if nav_hints.iter().any(|h| h.target == *area) {
            continue;
        }
        nav_hints.push(AnswerItem {
            kind: "in_scope_area".into(),
            target: area.clone(),
            path: None,
            status: "navigation_only".into(),
            confidence: 0.5,
            reason: "In-scope area suggested by graph navigation".into(),
            role: "navigation_area".into(),
            evidence: serde_json::json!({
                "source": "task-localize.scope",
            }),
        });
    }

    // Cap answer count after dedup so we hit the user's intent for
    // `max_answer_items` exactly.
    answers.truncate(params.max_answer_items);

    let answer_count = answers.len();
    let navigation_hint_count = nav_hints.len();

    // Confidence summary: trivial bucketing on the answer items.
    let answer_summary = bucket_confidence(&answers);

    // Trust policy.
    //
    // The native path doesn't (yet) run symbol search, source-text grep,
    // or callsite expansion — those are the evidence sources Python uses
    // to justify `answer_candidate` trust. So at session 1 we deliberately
    // hold trust at `needs_verification` regardless of how strong the
    // anchors look. Subsequent sessions raise trust as the native pipeline
    // earns it back.
    let trust_reason = if answers.is_empty() && nav_hints.is_empty() {
        "No anchors or in-scope files found by graph navigation."
    } else {
        "Native session-1 path: anchors and scope only. Run symbol/text \
         analyzers (Python explore) before treating as authoritative."
    };
    let trust_policy = TrustPolicy {
        safe_to_use_as_answer: false,
        safe_to_use_as_navigation: !answers.is_empty() || !nav_hints.is_empty(),
        evidence_level: "graph",
        authoritative_answer_count: 0,
        navigation_hint_count,
        degraded: false,
        trust_policy: if answers.is_empty() && nav_hints.is_empty() {
            "failed"
        } else {
            "needs_verification"
        },
        reason: trust_reason.to_string(),
    };

    let status = if answers.is_empty() && nav_hints.is_empty() {
        "degraded"
    } else {
        "complete"
    };

    let next_actions = if answers.is_empty() && nav_hints.is_empty() {
        vec![
            "Refine the request — graph navigation found no anchors.".into(),
            "Try a more specific keyword from the codebase domain.".into(),
        ]
    } else {
        vec![
            "Read the top answer[] item to verify it matches the task.".into(),
            "If unsure, run `aethyme explore --detail standard` for richer \
             evidence (symbol search + source-text)."
                .into(),
        ]
    };

    ExploreResponse {
        schema_version: "aethyme-explore-v1",
        mode: "explore",
        intent: "task_localization_query",
        intent_source: "default",
        status,
        request: ExploreRequest {
            raw: request.to_string(),
            parameters: serde_json::Value::Object(serde_json::Map::new()),
        },
        answer: answers,
        navigation_hints: nav_hints,
        excluded: Vec::new(),
        ambiguous: Vec::new(),
        evidence: Evidence {
            answer_count,
            navigation_hint_count,
            excluded_count: 0,
        },
        confidence: Confidence {
            overall: overall_confidence(&_dummy_strings_for_summary()),
            answer_summary,
            excluded_summary: ConfidenceSummary::default(),
            analyzed_summary: serde_json::json!({}),
        },
        safe_to_use_as_answer: false,
        safe_to_use_as_navigation: trust_policy.safe_to_use_as_navigation,
        trust_policy,
        degraded_reasons: Vec::new(),
        verification_steps: vec![
            serde_json::json!({
                "step": "Open the top answer[] item and confirm it matches the task before relying on it.",
                "rationale": "Graph navigation found this candidate; verifying that the file genuinely handles the task is fast.",
            }),
        ],
        next_actions,
        available_specialized_intents: vec![
            "behavior_localization_query",
            "usage_boundary_query",
        ],
    }
}

fn bucket_confidence(items: &[AnswerItem]) -> ConfidenceSummary {
    let mut summary = ConfidenceSummary::default();
    for item in items {
        if item.confidence >= 0.8 {
            summary.high += 1;
        } else if item.confidence >= 0.6 {
            summary.medium += 1;
        } else {
            summary.low += 1;
        }
    }
    summary
}

fn overall_confidence(_items: &[String]) -> Option<f64> {
    // Stub — real implementation will use weighted aggregate over evidence
    // sources in a later session. Returning None keeps the contract honest:
    // "no overall confidence is computed yet."
    None
}

fn _dummy_strings_for_summary() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_view() -> serde_json::Value {
        serde_json::json!({
            "task": "find watchlist handlers",
            "anchors": {
                "task": "find watchlist handlers",
                "anchors": [
                    {"kind": "file", "id": "includes/Watchlist/WatchedItemStore.php",
                     "file": "includes/Watchlist/WatchedItemStore.php",
                     "reason": "filename match"},
                    {"kind": "folder", "id": "includes/Watchlist",
                     "file": null,
                     "reason": "area match"}
                ]
            },
            "scope": {
                "task": "find watchlist handlers",
                "navigation_order": ["includes/Watchlist", "includes/Specials"],
                "in_scope_files": [
                    "includes/Specials/SpecialEditWatchlist.php",
                    "includes/Watchlist/WatchlistManager.php"
                ],
                "in_scope_symbols": [],
                "in_scope_areas": ["includes/Watchlist"],
                "out_of_scope": [],
                "risks": []
            },
            "next": {
                "target": "find watchlist handlers",
                "relation": "next",
                "items": []
            }
        })
    }

    #[test]
    fn build_response_synthesizes_answers_and_nav_hints() {
        let response = build_response(
            "find watchlist handlers",
            &sample_view(),
            &ExploreParams::default(),
        );
        assert!(!response.answer.is_empty(), "expected at least one answer");
        assert!(
            response.answer.iter().any(|a| a.path.as_deref()
                == Some("includes/Watchlist/WatchedItemStore.php")),
            "anchor file should land in answer[]"
        );
        assert!(
            response.answer.iter().any(|a| a.path.as_deref()
                == Some("includes/Specials/SpecialEditWatchlist.php")),
            "in-scope file should land in answer[]"
        );
        assert!(
            response.navigation_hints.iter().any(|h| h.target == "includes/Watchlist"),
            "folder anchor should land in navigation_hints[]"
        );
    }

    #[test]
    fn build_response_caps_answer_count() {
        let mut view = sample_view();
        let scope = view.get_mut("scope").unwrap();
        scope["in_scope_files"] = serde_json::json!(
            ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]
        );
        let response = build_response(
            "test",
            &view,
            &ExploreParams {
                max_answer_items: 3,
                detail: Detail::Compact,
            },
        );
        assert_eq!(response.answer.len(), 3);
    }

    #[test]
    fn build_response_degraded_when_empty() {
        let view = serde_json::json!({
            "anchors": {"anchors": []},
            "scope": {
                "in_scope_files": [],
                "in_scope_symbols": [],
                "in_scope_areas": [],
                "out_of_scope": [],
                "risks": []
            },
            "next": {"items": []}
        });
        let response =
            build_response("nothing matches", &view, &ExploreParams::default());
        assert_eq!(response.status, "degraded");
        assert!(response.answer.is_empty());
        assert!(response.navigation_hints.is_empty());
        assert_eq!(response.trust_policy.trust_policy, "failed");
    }

    #[test]
    fn trust_policy_session_one_caps_at_needs_verification() {
        let response = build_response(
            "find handlers",
            &sample_view(),
            &ExploreParams::default(),
        );
        // Session 1 contract: even with strong anchors, never claim
        // `answer_candidate` because we haven't run symbol/text/callsite
        // analyzers yet.
        assert!(!response.safe_to_use_as_answer);
        assert_eq!(response.trust_policy.trust_policy, "needs_verification");
    }
}
