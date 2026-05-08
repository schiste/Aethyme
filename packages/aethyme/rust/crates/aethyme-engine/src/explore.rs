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
use crate::graph::usage_boundary::analyze_usage_boundary_scope_first;
use crate::model::analysis::{AnswerStatus, DeadCodeCandidate};

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
    /// Downstream-friendly repackaging of the response. Mirrors Python's
    /// `output_adapters.task_localization_json` / `dead_code_eval_json`
    /// at `cli.py:2088-2118` and `cli.py:4700`.
    ///
    /// Gated by `detail==Full` OR `show_observability` to mirror
    /// `_trim_explore_response` at `cli.py:1735-1739` — at compact and
    /// standard the canonical `answer[]` is what consumers read; the
    /// adapter is redundant repackaging that costs tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_adapters: Option<serde_json::Value>,
    /// Echo of the effective `ExploreParams` after intent + detail
    /// widening. Mirrors Python's `resolved_parameters`. Same gate as
    /// `output_adapters` (full or show-observability) — internal tuning
    /// knobs aren't actionable by the agent at compact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_parameters: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct ExploreRequest {
    pub raw: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
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
    pub evidence_level: String,
    pub authoritative_answer_count: usize,
    pub navigation_hint_count: usize,
    pub degraded: bool,
    pub trust_policy: &'static str,
    pub reason: String,
}

// ── intents ────────────────────────────────────────────────────────────

/// The two task_localization-shaped intents handled by the daemon path.
///
/// `task_localization_query` is the default: bounded answer, compact
/// detail, conservative defaults. `behavior_localization_query` is for
/// change-tasks ("what would I edit to make X happen?") — same engine
/// call, wider params.
///
/// `usage_boundary_query` is dispatched separately because it doesn't
/// use the daemon — it calls `analyze_usage_boundary_scope_first`
/// directly via `explore_usage_boundary`. This enum only covers the
/// daemon-routed intents; the third intent has its own entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    TaskLocalization,
    BehaviorLocalization,
}

impl Intent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Intent::TaskLocalization => "task_localization_query",
            Intent::BehaviorLocalization => "behavior_localization_query",
        }
    }

    /// Apply intent-specific overrides to params. behavior_localization
    /// widens the search: more text candidates, more callsite breadth.
    /// Mirrors Python's behavior_params dict in cli.py:432-436.
    pub fn apply_param_defaults(&self, params: &mut ExploreParams) {
        match self {
            Intent::TaskLocalization => {}
            Intent::BehaviorLocalization => {
                if params.max_text_files < 10 {
                    params.max_text_files = 10;
                }
                if params.max_filename_hints < 5 {
                    params.max_filename_hints = 5;
                }
                // Also expand symbol coverage — change tasks need to
                // see more candidate sites.
                if params.max_symbol_files < 12 {
                    params.max_symbol_files = 12;
                }
            }
        }
    }

    /// Heuristic intent selection from request text.
    ///
    /// Returns `BehaviorLocalization` when the request opens with a
    /// change-task verb ("add", "implement", "fix", etc.) within the
    /// first 10 tokens — that's where intent verbs front-load.
    /// Otherwise returns `TaskLocalization`.
    ///
    /// Cost asymmetry: this heuristic intentionally leans toward
    /// `BehaviorLocalization` when uncertain. Picking behavior when
    /// the user wanted task only costs a slightly wider search;
    /// picking task when the user wanted behavior costs missed
    /// candidate sites — a real quality regression. The signal-set
    /// is conservative (only confident change-verbs) but the bias
    /// is to surface change-shape evidence when it's plausible.
    ///
    /// Currently opt-in via `--intent auto` from the CLI; the
    /// default stays at `TaskLocalization` for back-compat. Once
    /// evals validate the heuristic's hit rate on real requests,
    /// this can become the default.
    pub fn auto_select(request: &str) -> Self {
        const CHANGE_VERBS: &[&str] = &[
            // Additive
            "add", "adds", "adding", "implement", "implements",
            "implementing", "introduce", "introduces", "introducing",
            "create", "creates", "creating", "build", "builds",
            "building", "wire", "wires", "wiring",
            // Modifying
            "modify", "modifies", "modifying", "edit", "edits", "editing",
            "change", "changes", "changing", "update", "updates",
            "updating", "tweak", "tweaks",
            // Restructuring
            "refactor", "refactors", "refactoring", "restructure",
            "restructures", "restructuring", "rewrite", "rewrites",
            "rewriting", "rename", "renames", "renaming", "extract",
            "extracts", "extracting",
            // Fixing
            "fix", "fixes", "fixing", "repair", "repairs", "repairing",
            "resolve", "resolves", "resolving", "patch", "patches",
            "patching",
            // Removing
            "remove", "removes", "removing", "delete", "deletes",
            "deleting", "drop", "drops", "dropping", "deprecate",
            "deprecates", "deprecating", "retire", "retires", "retiring",
            // Migrating
            "migrate", "migrates", "migrating", "port", "ports",
            "porting", "convert", "converts", "converting",
        ];
        let lower = request.to_ascii_lowercase();
        // Look only at the first ~10 tokens — verbs front-load.
        let token_iter = lower
            .split(|c: char| {
                c.is_whitespace()
                    || matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'')
            })
            .filter(|s| !s.is_empty())
            .take(10);
        for token in token_iter {
            if CHANGE_VERBS.contains(&token) {
                return Intent::BehaviorLocalization;
            }
        }
        Intent::TaskLocalization
    }
}

// ── parameters ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExploreParams {
    pub max_answer_items: usize,
    /// Detail level: `compact`, `standard`, or `full`. Mirrors the Python
    /// `--detail` flag. Today only `compact` is fully implemented in the
    /// native path; standard/full fall back to Python at the call site.
    pub detail: Detail,
    /// Number of distinct symbol queries to derive from the request. The
    /// Python compact default is 5; matched here.
    pub max_symbol_queries: usize,
    /// Per-query result cap for symbol search.
    pub max_symbol_results: usize,
    /// Number of symbol-search-derived files to include in `answer[]`.
    /// Caps independently of `max_answer_items` so symbol evidence
    /// doesn't crowd out anchor evidence.
    pub max_symbol_files: usize,
    /// Maximum source-text candidate files emitted to `answer[]`.
    pub max_text_files: usize,
    /// Per-file cap on the `evidence.line_refs` excerpt list. Only the
    /// highest-scoring lines per file appear in the response — agents read
    /// 1-2; emitting all hits would be ~6,000 tokens of noise on a
    /// well-matching file.
    pub max_text_line_refs: usize,
    /// Number of filename-token matches to surface in
    /// `navigation_hints[]`. Filename-only matches aren't
    /// authoritative; this caps how many we suggest as "look here".
    pub max_filename_hints: usize,
    /// Number of strongest symbol-search hits to feed into the
    /// callsite expansion pass. Each hit costs one `callers-of`
    /// daemon RPC; setting this too high inflates response time on
    /// queries that match many symbols. 4 is the Python compact
    /// default and a reasonable cap.
    pub max_callsite_symbols: usize,
    /// Per-symbol cap on the caller files surfaced as
    /// `call_site_file` answer items. Most agents read the top 3-4;
    /// emitting all callers (which can be hundreds for popular
    /// functions) inflates the response unnecessarily.
    pub max_callsite_results: usize,
    /// When true, emit a richer observability envelope. Mirrors Python's
    /// `--show-observability` flag at `cli.py:396-401`. Compact form
    /// (default) keeps the response small; full form includes graph
    /// counts, fact counts, confidence summary, and degraded reasons
    /// for downstream introspection.
    pub show_observability: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Compact,
    Standard,
    Full,
}

impl Detail {
    /// Apply detail-level overrides to params. Standard widens caps
    /// roughly 2x; Full widens 4x. Compact uses the user-provided
    /// (or default) values unchanged.
    ///
    /// We widen the *existing* evidence caps rather than emitting
    /// new fields (the way Python's `--detail standard` does with
    /// `output_adapters` etc.) because the agent flow rarely needs
    /// the verbose envelope — what helps is more candidates to
    /// triage. Output_adapters and observability stay compact-shaped.
    ///
    /// Why 2x (not Python's ~5x) at standard: the predecessor Python
    /// `_task_localization_detail_defaults` jumped `max_answer_items`
    /// from 5 (compact) to 24 (standard) — a 4.8x widening. We picked
    /// 2x because the 2026-05-07 evals (GRC + MediaWiki bug-fix-1)
    /// showed agents triage well with 10 candidates at standard;
    /// pushing to ~22 inflates response tokens without observable
    /// quality gain. Callers needing the wider pool can ask for
    /// `--detail full` (4x) or set `--max-answer-items` explicitly.
    /// This is a deliberate divergence from the Python predecessor,
    /// not an oversight (cleanup ladder #5 / project_native_explore_parity_2026_05_07.md).
    pub fn apply_param_widening(&self, params: &mut ExploreParams) {
        let factor: usize = match self {
            Detail::Compact => return,
            Detail::Standard => 2,
            Detail::Full => 4,
        };
        params.max_answer_items = params.max_answer_items.saturating_mul(factor);
        params.max_symbol_queries =
            params.max_symbol_queries.saturating_mul(factor);
        params.max_symbol_results =
            params.max_symbol_results.saturating_mul(factor);
        params.max_symbol_files = params.max_symbol_files.saturating_mul(factor);
        params.max_text_files = params.max_text_files.saturating_mul(factor);
        params.max_text_line_refs =
            params.max_text_line_refs.saturating_mul(factor);
        params.max_filename_hints =
            params.max_filename_hints.saturating_mul(factor);
    }
}

impl Default for ExploreParams {
    fn default() -> Self {
        Self {
            max_answer_items: 5, // matches Python compact default after f1e3da5
            detail: Detail::Compact,
            max_symbol_queries: 5,
            max_symbol_results: 4,
            max_symbol_files: 8, // truncated when answer list fills
            max_text_files: 5,   // matches Python compact default
            max_text_line_refs: 2,
            max_filename_hints: 3,
            max_callsite_symbols: 4,  // Python compact default
            max_callsite_results: 4,  // Python compact default
            show_observability: false, // Python default; --show-observability flips it
        }
    }
}

impl ExploreParams {
    /// Serialize the resolved params as a JSON object for
    /// `resolved_parameters` echo. We don't auto-derive Serialize on
    /// the struct because some downstream consumers expect specific
    /// field naming and Detail's enum form needs a string (not a
    /// debug-formatted variant).
    pub fn to_json(&self) -> serde_json::Value {
        let detail = match self.detail {
            Detail::Compact => "compact",
            Detail::Standard => "standard",
            Detail::Full => "full",
        };
        serde_json::json!({
            "max_answer_items": self.max_answer_items,
            "detail": detail,
            "max_symbol_queries": self.max_symbol_queries,
            "max_symbol_results": self.max_symbol_results,
            "max_symbol_files": self.max_symbol_files,
            "max_text_files": self.max_text_files,
            "max_text_line_refs": self.max_text_line_refs,
            "max_filename_hints": self.max_filename_hints,
            "max_callsite_symbols": self.max_callsite_symbols,
            "max_callsite_results": self.max_callsite_results,
            "show_observability": self.show_observability,
        })
    }
}

// ── errors ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ExploreError {
    DaemonNotRunning,
    DaemonRpc(String),
    InvalidResponse(String),
    /// Engine analyzer failure — used by paths that don't go through the
    /// daemon (e.g. usage_boundary_query, which calls
    /// `analyze_usage_boundary_scope_first` directly on the filesystem).
    EngineAnalyzer(String),
    /// Caller passed insufficient or malformed parameters for the
    /// requested intent. Distinguishes user error from system error so
    /// CLIs can return exit code 2 instead of 1.
    BadParams(String),
}

impl std::fmt::Display for ExploreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DaemonNotRunning => write!(f, "engine daemon not running"),
            Self::DaemonRpc(msg) => write!(f, "engine daemon rpc: {msg}"),
            Self::InvalidResponse(msg) => write!(f, "invalid daemon response: {msg}"),
            Self::EngineAnalyzer(msg) => write!(f, "engine analyzer: {msg}"),
            Self::BadParams(msg) => write!(f, "bad params: {msg}"),
        }
    }
}

impl std::error::Error for ExploreError {}

// ── usage_boundary_query (Rust port of _explore_usage_boundary_query) ───

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
/// scope filesystem, parses with tree-sitter, scans for callers
/// across `search_roots` (or the whole repo). It runs in-process, so
/// the binary's own startup cost is the only fixed overhead.
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
    explore_with_intent(
        repo,
        request,
        Intent::TaskLocalization,
        IntentSource::Default,
        params,
    )
}

/// How the intent was selected. Reported back in the response so
/// consumers can attribute the choice (an agent that explicitly
/// requested behavior_localization should know its choice was honored,
/// vs the heuristic having picked it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentSource {
    /// No --intent flag — the default (TaskLocalization) was used.
    Default,
    /// Caller passed --intent <X> explicitly.
    Explicit,
    /// Caller passed --intent auto and the heuristic picked X.
    Auto,
}

impl IntentSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            IntentSource::Default => "default",
            IntentSource::Explicit => "explicit",
            IntentSource::Auto => "auto",
        }
    }
}

/// Run an explore intent with explicit intent selection.
///
/// `task_localization` is the read-only default ("where is X?").
/// `behavior_localization` is for change-tasks ("what do I edit to
/// make X happen?") — wider param defaults, otherwise same path.
pub fn explore_with_intent(
    repo: &Path,
    request: &str,
    intent: Intent,
    intent_source: IntentSource,
    params: &ExploreParams,
) -> Result<ExploreResponse, ExploreError> {
    let socket = daemon::socket_path_for(repo);
    if !socket.exists() {
        return Err(ExploreError::DaemonNotRunning);
    }

    let mut effective_params = params.clone();
    // Order: detail widening first (compact → standard/full caps),
    // then intent overrides (behavior_localization wider than
    // task_localization). Intent overrides apply MIN-bound semantics
    // — they widen but never shrink — so order is fine either way,
    // but doing detail first feels like the user-visible flag should
    // dominate.
    let detail = effective_params.detail;
    detail.apply_param_widening(&mut effective_params);
    intent.apply_param_defaults(&mut effective_params);
    let params = &effective_params;

    // 1. Graph-derived view (anchors + scope + next).
    let view = call_task_localize(&socket, request)?;

    // 2. Symbol-search evidence. On failure (degraded daemon, network
    //    blip), keep going with the anchors-only path rather than block
    //    the whole request.
    let symbol_queries = extract_symbol_queries(request);
    let symbol_queries = if symbol_queries.len() > params.max_symbol_queries {
        symbol_queries[..params.max_symbol_queries].to_vec()
    } else {
        symbol_queries
    };
    let symbol_matches = if symbol_queries.is_empty() {
        SymbolBatchResults::default()
    } else {
        match call_symbol_batch(&socket, &symbol_queries, params.max_symbol_results) {
            Ok(r) => r,
            Err(_) => SymbolBatchResults::default(),
        }
    };

    // 3. Source-text evidence. Runs ripgrep client-side against the repo
    //    filesystem; doesn't need the daemon. Tolerates ripgrep absence:
    //    we degrade to symbol-only without failing the request.
    let text_terms = extract_text_search_terms(request);
    let text_items = source_text_files(
        repo,
        &text_terms,
        params.max_text_files,
        params.max_text_line_refs,
    );

    // 4. Filename-token matches. These are navigation_hints, not
    //    answers — a filename match alone is a "look here next"
    //    signal, not authoritative. Catches the case where the
    //    canonical file's NAME contains the request terms but its
    //    symbols don't (e.g. `suppliers_grader.py` for "find
    //    suppliers grader" — its functions are named
    //    `_default_graders` etc).
    let filename_items = filename_token_matches(
        repo,
        &symbol_queries,
        params.max_filename_hints,
    );

    // 5. Callsite expansion. For each strong symbol hit, look up
    //    its callers (via the daemon's callers-of RPC) and emit
    //    `call_site_file` AnswerItems for the caller files. This is
    //    the deepest evidence layer: not "this file defines X" but
    //    "these files actually call X." A file appearing in BOTH
    //    symbol matches AND someone-else's-callsite is the strongest
    //    cross-corroboration we produce without running tests.
    //
    //    Tolerates daemon failure on this RPC the same way we do for
    //    symbol-batch — degrade silently, keep the rest of the
    //    answer.
    let callsite_items = compute_callsite_files(
        &socket,
        &symbol_matches,
        params.max_callsite_symbols,
        params.max_callsite_results,
    )
    .unwrap_or_default();

    Ok(build_response(
        request,
        intent,
        intent_source,
        &view,
        &symbol_matches,
        &text_items,
        &filename_items,
        &callsite_items,
        params,
    ))
}

/// Pick the strongest symbol hits, look up callers via the daemon,
/// and emit one `call_site_file` AnswerItem per distinct caller file.
///
/// Strategy:
///   1. Walk symbol_matches in query-order, collect distinct symbol
///      ids up to `max_symbols`. Prefer high-score hits.
///   2. Issue one `callers-of` RPC with the batch (single roundtrip).
///   3. Group caller paths by file, dedup, score by how many distinct
///      symbols routed to that file (signal multiplier — a file
///      calling 2+ of our candidate symbols is stronger evidence).
///   4. Take top `max_results`.
fn compute_callsite_files(
    socket: &Path,
    symbol_matches: &SymbolBatchResults,
    max_symbols: usize,
    max_results: usize,
) -> Result<Vec<AnswerItem>, ExploreError> {
    if max_symbols == 0 || max_results == 0 {
        return Ok(Vec::new());
    }
    // Collect distinct symbol ids round-robin across queries: take the
    // highest-scoring hit from each query first, then the second, etc.
    // This is critical when one query (e.g. "suppliers") returns 20
    // hits while another ("grader") returns 2 — depth-first iteration
    // would burn the entire `max_symbols` budget on the first query and
    // never reach the second. The whole point of callsite expansion is
    // to find files that bridge the user's distinct concepts, so each
    // concept must contribute.
    let mut symbol_ids: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut pass = 0usize;
    loop {
        if symbol_ids.len() >= max_symbols {
            break;
        }
        let mut added_this_pass = false;
        for query in &symbol_matches.query_order {
            if symbol_ids.len() >= max_symbols {
                break;
            }
            let Some(hits) = symbol_matches.by_query.get(query) else {
                continue;
            };
            let Some(hit) = hits.get(pass) else { continue };
            // SymbolHit name is what the engine accepts via callers-of
            // (matched against the symbol-name index). For canonical
            // tightness we could parse and pass full ids, but names
            // route correctly today.
            if seen.insert(hit.name.clone()) {
                symbol_ids.push(hit.name.clone());
                added_this_pass = true;
            }
        }
        if !added_this_pass {
            // No query had a hit at this depth — nothing left to drain.
            break;
        }
        pass += 1;
    }
    if symbol_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rpc = serde_json::json!({
        "command": "callers-of",
        "targets": symbol_ids,
    });
    let response_text = daemon::send_request(socket, &rpc)
        .map_err(ExploreError::DaemonRpc)?;
    let envelope: serde_json::Value =
        serde_json::from_str(response_text.trim())
            .map_err(|e| ExploreError::InvalidResponse(format!("not JSON: {e}")))?;
    if envelope.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Ok(Vec::new());
    }
    let result_obj = match envelope.get("result").and_then(|v| v.as_object()) {
        Some(o) => o,
        None => return Ok(Vec::new()),
    };

    // file_path -> (Set<symbol_name>, hit_count, sample_callers)
    let mut by_file: std::collections::BTreeMap<
        String,
        (
            std::collections::BTreeSet<String>,
            usize,
            Vec<serde_json::Value>,
        ),
    > = std::collections::BTreeMap::new();
    for (symbol, callers) in result_obj {
        let arr = match callers.as_array() {
            Some(a) => a,
            None => continue,
        };
        for caller in arr {
            // The id format is `<kind>:<repo>:<path>:<symbol>` —
            // splitting on ':' once, the file path lives between the
            // 2nd and last segment. Easier: use the `path` we asked
            // for if the daemon provided it directly... actually we
            // didn't include path in the response. Fall back to
            // parsing `id`.
            let id = match caller.get("id").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let Some(file_path) = file_path_from_caller_id(id) else {
                continue;
            };
            let entry = by_file.entry(file_path.clone()).or_insert_with(|| {
                (
                    std::collections::BTreeSet::new(),
                    0,
                    Vec::new(),
                )
            });
            entry.0.insert(symbol.clone());
            entry.1 += 1;
            if entry.2.len() < 5 {
                entry.2.push(serde_json::json!({
                    "symbol": symbol,
                    "caller_id": id,
                    "display": caller.get("display"),
                }));
            }
        }
    }

    // Rank: more distinct symbols routing through this file = stronger
    // evidence. Tiebreak on hit_count then path (alphabetical).
    let mut ranked: Vec<(String, std::collections::BTreeSet<String>, usize, Vec<serde_json::Value>)> =
        by_file
            .into_iter()
            .map(|(path, (syms, hits, samples))| (path, syms, hits, samples))
            .collect();
    ranked.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked.truncate(max_results);

    Ok(ranked
        .into_iter()
        .map(|(path, symbols, hit_count, samples)| {
            let symbol_count = symbols.len();
            let multi = symbol_count >= 2;
            let confidence = if multi { 0.86 } else { 0.74 };
            let reason = if multi {
                "Multiple candidate symbols are called from this file; \
                 likely a usage entry point or dispatch hub."
            } else {
                "This file calls one of the candidate symbols; verify \
                 whether it's the primary caller or one of many."
            };
            let symbols_list: Vec<&String> = symbols.iter().collect();
            AnswerItem {
                kind: "call_site_file".into(),
                target: path.clone(),
                path: Some(path),
                status: "candidate".into(),
                confidence,
                reason: reason.into(),
                role: "callsite".into(),
                evidence: serde_json::json!({
                    "source": "callers-of",
                    "symbols": symbols_list,
                    "hit_count": hit_count,
                    "samples": samples,
                }),
            }
        })
        .collect())
}

/// Parse a caller's structured id of the form
/// `<kind>:<repo>:<path>:<symbol>` and return the path segment.
/// Returns `None` if the id doesn't match the expected shape, which
/// happens when the engine emits a node id with no file (e.g.
/// area-level nodes).
fn file_path_from_caller_id(id: &str) -> Option<String> {
    // The engine canonicalizes path-bearing ids as
    //   `kind:repo_name:relative/path:symbol`
    // For our purposes we want the third colon-separated segment.
    // Split with limit so a colon inside the symbol name doesn't
    // break the parse.
    let parts: Vec<&str> = id.splitn(4, ':').collect();
    if parts.len() < 4 {
        return None;
    }
    let path = parts[2];
    if path.is_empty() {
        return None;
    }
    Some(path.to_string())
}

fn call_task_localize(
    socket: &Path,
    request: &str,
) -> Result<serde_json::Value, ExploreError> {
    let rpc_request = serde_json::json!({
        "command": "task-localize",
        "task": request,
    });
    let response_text = daemon::send_request(socket, &rpc_request)
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
    envelope
        .get("result")
        .cloned()
        .ok_or_else(|| ExploreError::InvalidResponse("missing `result`".into()))
}

fn call_symbol_batch(
    socket: &Path,
    queries: &[String],
    limit: usize,
) -> Result<SymbolBatchResults, ExploreError> {
    let rpc_request = serde_json::json!({
        "command": "symbol-batch",
        "queries": queries,
        "limit": limit,
    });
    let response_text = daemon::send_request(socket, &rpc_request)
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
    let result_obj = envelope
        .get("result")
        .and_then(|v| v.as_object())
        .ok_or_else(|| ExploreError::InvalidResponse("missing/object `result`".into()))?;

    let mut by_query: std::collections::BTreeMap<String, Vec<SymbolHit>> =
        std::collections::BTreeMap::new();
    for (query, hits) in result_obj {
        let arr = match hits.as_array() {
            Some(a) => a,
            None => continue,
        };
        let parsed: Vec<SymbolHit> = arr
            .iter()
            .filter_map(SymbolHit::from_value)
            .collect();
        by_query.insert(query.clone(), parsed);
    }
    Ok(SymbolBatchResults {
        query_order: queries.to_vec(),
        by_query,
    })
}

#[derive(Debug, Default)]
struct SymbolBatchResults {
    /// Original query order — preserves user-intent order across the
    /// alphabetical BTreeMap iteration.
    query_order: Vec<String>,
    by_query: std::collections::BTreeMap<String, Vec<SymbolHit>>,
}

#[derive(Debug, Clone)]
struct SymbolHit {
    name: String,
    kind: String,
    file: String,
    line: u64,
    score: i64,
}

impl SymbolHit {
    fn from_value(v: &serde_json::Value) -> Option<Self> {
        let obj = v.as_object()?;
        Some(SymbolHit {
            name: obj.get("name")?.as_str()?.to_string(),
            kind: obj.get("kind")?.as_str()?.to_string(),
            file: obj.get("file")?.as_str()?.to_string(),
            line: obj.get("line")?.as_u64().unwrap_or(0),
            score: obj.get("score")?.as_i64().unwrap_or(0),
        })
    }
}

// ── filename-token matching (Rust port of _task_localization_filesystem_items) ──
//
// Catches the case where the relevant file's NAME contains the request
// terms but its symbols don't. Example from the Mockup measurement:
// query "find suppliers grader" — `suppliers_grader.py` is the obvious
// answer, but its functions are named `_default_graders` etc. Symbol
// search misses the file; filename matching catches it.
//
// Output goes to `navigation_hints[]`, NOT `answer[]`: a filename-only
// match is a hint to look at, not authoritative evidence. Confidence
// stays low (0.28-0.38). Mirrors Python's
// `_task_localization_filesystem_items` contract.

const FILENAME_ALLOWED_SUFFIXES: &[&str] = &[
    "c", "cc", "cpp", "cs", "go", "h", "hpp", "java", "js", "jsx",
    "kt", "mjs", "php", "py", "rb", "rs", "swift", "ts", "tsx", "vue",
];

fn filename_token_matches(
    repo: &Path,
    terms: &[String],
    max_items: usize,
) -> Vec<AnswerItem> {
    if terms.is_empty() || max_items == 0 {
        return Vec::new();
    }
    // `rg --files` walks the repo respecting gitignore, returns one
    // path per line. Way faster than std::fs traversal on 7K-file repos
    // and skips junk (.venv, node_modules, etc) by default.
    let output = match std::process::Command::new(RIPGREP_BIN)
        .arg("--files")
        .arg("--no-messages")
        .arg(repo)
        .output()
    {
        Ok(o) if o.status.success() || !o.stdout.is_empty() => o,
        _ => return Vec::new(),
    };
    let stdout = match std::str::from_utf8(&output.stdout) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let lowered_terms: Vec<String> =
        terms.iter().map(|t| t.to_ascii_lowercase()).collect();
    let mut scored: Vec<(i32, Vec<String>, String)> = Vec::new();
    for abs_line in stdout.lines() {
        if abs_line.is_empty() {
            continue;
        }
        let abs = Path::new(abs_line);
        // Suffix gate: only consider source-code files.
        let suffix = abs
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());
        match suffix.as_deref() {
            Some(s) if FILENAME_ALLOWED_SUFFIXES.contains(&s) => {}
            _ => continue,
        }
        let rel_path = match abs.strip_prefix(repo) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let (score, matched) = filename_match_score(&rel_path, &lowered_terms);
        if score <= 0 {
            continue;
        }
        scored.push((score, matched, rel_path));
    }

    // Sort by score descending, prefer SHORTER paths within same score
    // (less-nested = more likely to be the canonical home of the
    // concept), then alphabetical for stability.
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.2.len().cmp(&b.2.len()))
            .then_with(|| a.2.cmp(&b.2))
    });
    scored.truncate(max_items);

    scored
        .into_iter()
        .map(|(score, matched_terms, rel_path)| {
            let multi = matched_terms.len() > 1;
            let confidence = if multi { 0.38 } else { 0.28 };
            AnswerItem {
                kind: "filesystem_file".into(),
                target: rel_path.clone(),
                path: Some(rel_path),
                status: "navigation_hint".into(),
                confidence,
                reason: "Filename-only match. Use as a search/navigation hint, \
                         not as primary answer evidence."
                    .into(),
                role: "navigation_filename".into(),
                evidence: serde_json::json!({
                    "source": "filesystem-filename",
                    "matched_terms": matched_terms,
                    "score": score,
                }),
            }
        })
        .collect()
}

/// Score a path against query terms, mirroring Python's
/// `_filesystem_match_score`. Higher = stronger filename signal.
///   - exact stem match:    +20
///   - stem prefix:         +12
///   - substring in stem:    +8
///   - substring in basename:+5
///   - substring in path:    +2
fn filename_match_score(path: &str, lowered_terms: &[String]) -> (i32, Vec<String>) {
    let lowered_path = path.to_ascii_lowercase();
    let filename = Path::new(&lowered_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&lowered_path)
        .to_string();
    let stem = Path::new(&lowered_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&filename)
        .to_string();

    let mut score = 0;
    let mut matched: Vec<String> = Vec::new();
    for term in lowered_terms {
        if term == &stem {
            score += 20;
            matched.push(term.clone());
        } else if stem.starts_with(term) {
            score += 12;
            matched.push(term.clone());
        } else if stem.contains(term) {
            score += 8;
            matched.push(term.clone());
        } else if filename.contains(term) {
            score += 5;
            matched.push(term.clone());
        } else if lowered_path.contains(term) {
            score += 2;
            matched.push(term.clone());
        }
    }
    (score, matched)
}

// ── source-text search (Rust port of _task_localization_text_items) ────
//
// Strategy: shell out to ripgrep for the heavy lifting (file walking,
// suffix filtering, gitignore-respecting traversal, multi-pattern match)
// and do the scoring + ranking in Rust. Ripgrep at 161ms on Mockup for a
// single term means the whole multi-term pass lands well under 1s — fast
// enough that we don't need to background it.
//
// What we do NOT port from the Python helper (yet, deferred to later
// sessions): per-line symbol clustering, file-role classification, the
// elaborate `_text_candidate_score` weighting heuristic. This session's
// port is a correct-but-simpler version: per-file hit count × distinct
// term coverage, with a cap on the line-ref preview list to keep the
// response token-cheap.

const RIPGREP_BIN: &str = "rg";
const SOURCE_TEXT_FILE_SIZE_CAP_BYTES: u64 = 750_000;

#[derive(Debug, Clone)]
struct TextHit {
    path: String,
    matched_terms: std::collections::BTreeSet<String>,
    hit_count: usize,
    line_refs: Vec<TextLineRef>,
}

#[derive(Debug, Clone)]
struct TextLineRef {
    line: u64,
    text: String,
    matched_terms: Vec<String>,
}

/// Build the term list for source-text search. Wider than
/// `extract_symbol_queries` — keeps behavioural words ("view", "seen")
/// that are too noisy for symbol search but useful for line-level
/// evidence. Mirrors `_request_text_search_terms` in cli.py.
pub(crate) fn extract_text_search_terms(request: &str) -> Vec<String> {
    let mut terms = extract_symbol_queries(request);
    let lowered = request.to_ascii_lowercase();
    let mut extras: Vec<&str> = Vec::new();
    if lowered.contains("watchlist") || lowered.contains("watchlisted") {
        extras.extend(["watchlist", "watchlisted", "watched", "notification"]);
    }
    if lowered.contains("seen") {
        extras.extend(["seen", "notification", "timestamp"]);
    }
    if lowered.contains("view") {
        extras.extend(["view", "viewed", "viewing"]);
    }
    if lowered.contains("diff") {
        extras.extend(["diff", "difference", "diffonly"]);
    }
    if lowered.contains("revision") || lowered.contains("oldid") {
        extras.extend(["revision", "revisions", "oldid"]);
    }
    let mut seen: std::collections::HashSet<String> = terms
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect();
    for extra in extras {
        let lower = extra.to_ascii_lowercase();
        if seen.insert(lower) {
            terms.push(extra.to_string());
        }
    }
    terms
}

/// Walk the repo with ripgrep across all terms in one pass, score each
/// matching file by hit count × distinct-term coverage, return up to
/// `max_files` candidates.
fn source_text_files(
    repo: &Path,
    terms: &[String],
    max_files: usize,
    max_line_refs: usize,
) -> Vec<AnswerItem> {
    if terms.is_empty() || max_files == 0 {
        return Vec::new();
    }

    let mut hits_by_file: std::collections::BTreeMap<String, TextHit> =
        std::collections::BTreeMap::new();

    for chunk in terms.chunks(8) {
        let pattern = chunk
            .iter()
            .map(|t| regex::escape(t))
            .collect::<Vec<_>>()
            .join("|");
        if pattern.is_empty() {
            continue;
        }
        let lowered_terms: Vec<String> =
            chunk.iter().map(|t| t.to_ascii_lowercase()).collect();
        let output = match std::process::Command::new(RIPGREP_BIN)
            .arg("-i")
            .arg("--no-heading")
            .arg("--with-filename")
            .arg("--line-number")
            .arg("--max-filesize")
            .arg(SOURCE_TEXT_FILE_SIZE_CAP_BYTES.to_string())
            .arg("--no-messages")
            .arg("-e")
            .arg(&pattern)
            .arg(repo)
            .output()
        {
            Ok(o) => o,
            Err(_) => continue,
        };
        if !output.stdout.is_empty() {
            ingest_rg_output(
                &output.stdout,
                repo,
                &lowered_terms,
                &mut hits_by_file,
            );
        }
    }

    let mut ranked: Vec<TextHit> = hits_by_file.into_values().collect();
    // Sort by composite score: (suffix_class_rank desc, distinct_terms desc,
    // hit_count desc, path asc). suffix_class_rank pushes executable source
    // ahead of locale data / changelogs so the agent doesn't see a wall of
    // i18n JSON files when their query terms happen to be common words.
    // Mirrors the Python helper's role-aware penalty without porting the
    // full heuristic (deferred to session 4+).
    ranked.sort_by(|a, b| {
        suffix_class_rank(&b.path)
            .cmp(&suffix_class_rank(&a.path))
            .then_with(|| b.matched_terms.len().cmp(&a.matched_terms.len()))
            .then_with(|| b.hit_count.cmp(&a.hit_count))
            .then_with(|| a.path.cmp(&b.path))
    });

    ranked
        .into_iter()
        .take(max_files)
        .map(|hit| {
            let matched_count = hit.matched_terms.len();
            let confidence = if matched_count >= 3 {
                0.84
            } else if matched_count == 2 {
                0.78
            } else {
                0.70
            };
            let reason = if matched_count >= 2 {
                "Source text matched multiple request terms in executable code; \
                 line refs are evidence, not filename-only hints."
            } else {
                "Source text matched one request term; verify the line context \
                 before treating as authoritative."
            };
            let mut line_refs: Vec<&TextLineRef> = hit.line_refs.iter().collect();
            // Highest-scoring lines = most distinct matched terms,
            // then earliest line number for stability.
            line_refs.sort_by(|a, b| {
                b.matched_terms
                    .len()
                    .cmp(&a.matched_terms.len())
                    .then_with(|| a.line.cmp(&b.line))
            });
            let line_refs_json: Vec<serde_json::Value> = line_refs
                .into_iter()
                .take(max_line_refs)
                .map(|r| {
                    serde_json::json!({
                        "line": r.line,
                        "text": r.text,
                        "matched_terms": r.matched_terms,
                    })
                })
                .collect();
            AnswerItem {
                kind: "source_text_file".into(),
                target: hit.path.clone(),
                path: Some(hit.path),
                status: "candidate".into(),
                confidence,
                reason: reason.into(),
                role: "candidate".into(),
                evidence: serde_json::json!({
                    "source": "source-text-search",
                    "matched_terms": hit.matched_terms.iter().collect::<Vec<_>>(),
                    "hit_count": hit.hit_count,
                    "line_refs": line_refs_json,
                }),
            }
        })
        .collect()
}

/// Coarse file-class ranking for source-text matches. Higher is better.
///
/// The query "find logic" matches lots of locale JSON files because every
/// translated string contains "logic". Without a class signal, those
/// files swamp `answer[]`. We rank executable source highest, then docs,
/// then changelogs/data lowest. Inside a class, finer ranking falls back
/// to term coverage and hit count.
///
/// This is intentionally simpler than the Python helper's weighting (which
/// combines file role, path patterns, enclosing-symbol presence, etc).
/// Captures the 80% case at 20% of the code.
fn suffix_class_rank(path: &str) -> i32 {
    let lower = path.to_ascii_lowercase();
    // Strong demote: locale/translation files. The `/locales/` segment is
    // the canonical pattern across most monorepos.
    if lower.contains("/locales/")
        || lower.contains("/locale/")
        || lower.contains("/i18n/")
        || lower.contains("/translations/")
    {
        return 0;
    }
    // Top-level data / metadata files. Match common request terms but
    // rarely the actual answer.
    let basename = lower.rsplit('/').next().unwrap_or(&lower);
    if basename == "changelog.md"
        || basename == "history.md"
        || basename == "package-lock.json"
        || basename == "yarn.lock"
        || basename == "pnpm-lock.yaml"
    {
        return 1;
    }
    // Test-file demote MUST be checked BEFORE the source-code arm —
    // a test file in a source language (test_foo.py, auth.spec.ts)
    // would otherwise hit the rank-5 source arm and ignore the
    // "tests rank slightly lower" intent.
    let is_test = lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.contains("_test.");
    let suffix = lower.rsplit('.').next().unwrap_or("");
    let is_source = matches!(
        suffix,
        "rs" | "py"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "go"
            | "java"
            | "kt"
            | "swift"
            | "rb"
            | "php"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "cs"
            | "mjs"
            | "cjs"
            | "vue"
            | "svelte"
    );
    if is_source && is_test {
        return 4;
    }
    if is_source {
        return 5;
    }
    if is_test {
        // Non-source test fixture (.json, .yaml, etc) — weaker than
        // tests in source languages but still has signal.
        return 3;
    }
    match suffix {
        "md" | "rst" | "adoc" | "txt" => 3,
        "yml" | "yaml" | "toml" | "ini" | "conf" | "config" => 2,
        // Generic JSON / data — common text matches but weak signal.
        _ => 1,
    }
}

fn ingest_rg_output(
    stdout: &[u8],
    repo: &Path,
    lowered_terms: &[String],
    hits_by_file: &mut std::collections::BTreeMap<String, TextHit>,
) {
    let text = match std::str::from_utf8(stdout) {
        Ok(s) => s,
        Err(_) => return,
    };
    for line in text.lines() {
        // ripgrep default format: <path>:<line>:<text>
        let mut parts = line.splitn(3, ':');
        let abs_path = match parts.next() {
            Some(p) => p,
            None => continue,
        };
        let line_no: u64 = match parts.next().and_then(|n| n.parse().ok()) {
            Some(n) => n,
            None => continue,
        };
        let line_text = parts.next().unwrap_or("");
        let rel_path = match Path::new(abs_path).strip_prefix(repo) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => abs_path.to_string(),
        };
        let lower = line_text.to_ascii_lowercase();
        let matched: Vec<String> = lowered_terms
            .iter()
            .filter(|t| lower.contains(t.as_str()))
            .cloned()
            .collect();
        if matched.is_empty() {
            // `rg` matched but our local lowercase scan missed (rare —
            // could happen with regex metachar quirks). Skip.
            continue;
        }
        let entry = hits_by_file
            .entry(rel_path.clone())
            .or_insert_with(|| TextHit {
                path: rel_path,
                matched_terms: std::collections::BTreeSet::new(),
                hit_count: 0,
                line_refs: Vec::new(),
            });
        for term in &matched {
            entry.matched_terms.insert(term.clone());
        }
        entry.hit_count += 1;
        // Cap stored line refs per file to bound memory; the ranking
        // step picks the top N by term coverage afterwards.
        if entry.line_refs.len() < 32 {
            entry.line_refs.push(TextLineRef {
                line: line_no,
                text: line_text.trim().chars().take(220).collect(),
                matched_terms: matched,
            });
        }
    }
}

// ── symbol query extraction (Rust port of _request_symbol_queries) ──────
//
// Tokenizes `request`, drops English stop words and noisy single-letter
// tokens, builds the canonical query list. When a token contains an
// underscore we add the dropped-underscore variant too (so `add_watch`
// also queries `addwatch`). Order-preserving + de-duplicated lowercase.

const STOP_WORDS: &[&str] = &[
    "about", "after", "against", "also", "and", "before", "being", "between",
    "bug", "code", "command", "could", "defined", "does", "done", "file",
    "files", "find", "fix", "for", "from", "have", "here", "how", "implement",
    "implemented", "implementation", "into", "issue", "json", "located",
    "make", "marked", "marks", "need", "object", "not", "only", "output",
    "path", "prose", "question", "relative", "report", "repo", "repository",
    "request", "rules", "shape", "the", "should", "specific", "that", "their",
    "there", "this", "ticket", "seen", "viewed", "viewing", "what", "when",
    "where", "which", "who", "why", "with", "would", "you",
];

pub(crate) fn extract_symbol_queries(request: &str) -> Vec<String> {
    let normalized = request.replace('`', " ");
    let mut raw_terms: Vec<String> = Vec::new();
    for token in normalized.replace('/', " ").replace('-', " ").split_whitespace() {
        let term: String = token
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if term.len() < 3 {
            continue;
        }
        let lowered = term.to_ascii_lowercase();
        if STOP_WORDS.contains(&lowered.as_str()) {
            continue;
        }
        raw_terms.push(term);
    }

    let mut queries: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for term in &raw_terms {
        let mut variants: Vec<String> = vec![term.clone()];
        if term.contains('_') {
            variants.push(term.replace('_', ""));
        }
        for variant in variants {
            let lowered = variant.to_ascii_lowercase();
            if seen.insert(lowered) {
                queries.push(variant);
            }
        }
    }
    queries
}

// ── response synthesis ──────────────────────────────────────────────────

/// Translate the engine daemon's `task-localize` view into the answer-json
/// envelope the agent contract expects.
fn build_response(
    request: &str,
    intent: Intent,
    intent_source: IntentSource,
    view: &serde_json::Value,
    symbol_matches: &SymbolBatchResults,
    text_items: &[AnswerItem],
    filename_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
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
    // Insertion order = priority order (each loop respects the
    // `max_answer_items` cap and skips paths already added):
    //
    //   1. source_text_file  — line-level evidence, strongest signal
    //   2. symbol_search_file — name-match evidence
    //   3. anchor             — graph-derived seed (heuristic, weaker)
    //   4. in_scope_file      — area-membership-only (weakest)
    //
    // This matters: anchors are heuristic seeds (e.g. "package.json"
    // matched a generic config-anchor weight). They're weaker
    // evidence than a line that literally contains the request's
    // terms in executable code. Putting them last among
    // answer-track items reflects that.
    //
    // anchors with `kind = "folder" | "area"` and in_scope_areas
    // are routed to `navigation_hints[]` because the agent is asking
    // for FILES to act on, not directories.
    let mut answers: Vec<AnswerItem> = Vec::new();
    let mut nav_hints: Vec<AnswerItem> = Vec::new();
    let mut anchor_file_items: Vec<AnswerItem> = Vec::new();

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
                anchor_file_items.push(AnswerItem {
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

    // Slot budgeting: if symbol search has multi-query hits, reserve up
    // to 2 slots so they always land in `answer[]` even when text
    // matches are plentiful. Without this, a query like "find suppliers
    // grader scoring logic" gets 5 weak text matches and zero symbol
    // matches in the response — even when the most relevant file
    // (suppliers_grader.py) was found by symbol search.
    //
    // The reservation is conservative: only ≥2 slots, only when symbol
    // has multi-query hits. Single-query symbol matches stay weakly
    // ranked.
    let symbol_items = build_symbol_file_items(symbol_matches, params.max_symbol_files);
    let multi_query_symbol_count = symbol_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_queries")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() >= 2)
                .unwrap_or(false)
        })
        .count();
    let symbol_reserved = multi_query_symbol_count.min(2);
    let text_budget = params.max_answer_items.saturating_sub(symbol_reserved);

    for item in text_items.iter().take(text_budget.max(1)) {
        if answers.len() >= text_budget {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
    }

    for item in &symbol_items {
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
    }

    // Callsite evidence: files that CALL one of our candidate symbols.
    // Ranks just after symbol_search_file because "this file calls X"
    // is behavioural evidence (similar strength to "this file's
    // source-text contains X's name"). Multi-symbol callsites
    // (rank ≥0.86 confidence) often surface dispatch hubs the agent
    // cares about more than the symbol's home file.
    for item in callsite_items {
        // Always allow merging into an existing answer (no new slot
        // consumed); only the push-new branch checks the cap.
        if let Some(existing) = answers
            .iter_mut()
            .find(|a| a.path.as_deref() == item.path.as_deref())
        {
            // Pull symbols list from the callsite item's evidence and
            // attach it to the existing item under `also_callsite_for`.
            // Bump confidence by a small amount (capped at 0.9) since
            // multiple corroborating sources increase trust.
            if let Some(syms) = item.evidence.get("symbols").cloned() {
                if let Some(obj) = existing.evidence.as_object_mut() {
                    obj.insert("also_callsite_for".to_string(), syms);
                    obj.insert(
                        "callsite_hit_count".to_string(),
                        item.evidence
                            .get("hit_count")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
            }
            existing.confidence = ((existing.confidence + 0.05).min(0.9) * 100.0).round() / 100.0;
            continue;
        }
        if answers.len() >= params.max_answer_items {
            continue;
        }
        answers.push(item.clone());
    }

    // Backfill text again now that symbol items have landed — the
    // budget cap above held remaining text out; if there's room left
    // (no symbol items, or symbol items dedup'd against text), let
    // text fill the rest.
    for item in text_items {
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
    }

    for item in &anchor_file_items {
        if answers.len() >= params.max_answer_items {
            break;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        answers.push(item.clone());
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

    // Filename-token matches: navigation hints, NOT answers. The
    // contract is "look here next" rather than "this IS the answer".
    // Skip files that already appear in answer[] (those have stronger
    // evidence and the agent has them in context already).
    for item in filename_items {
        if nav_hints
            .iter()
            .any(|h| h.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        if answers
            .iter()
            .any(|a| a.path.as_deref() == item.path.as_deref())
        {
            continue;
        }
        nav_hints.push(item.clone());
    }

    // Cap answer count after dedup so we hit the user's intent for
    // `max_answer_items` exactly.
    answers.truncate(params.max_answer_items);

    let answer_count = answers.len();
    let navigation_hint_count = nav_hints.len();

    // Confidence summary: trivial bucketing on the answer items.
    let answer_summary = bucket_confidence(&answers);

    // Trust policy. Tightens as more evidence sources land:
    //
    //   session 1: anchors + scope only          → needs_verification
    //   session 2: + symbol search                → answer_candidate when
    //                                              ≥2 distinct query terms
    //                                              matched in the same file
    //   session 3 (this commit):
    //              + source-text + corroboration → answer_candidate raised
    //                                              when text + symbol agree
    //                                              on the same file (the
    //                                              strongest signal short
    //                                              of running the test
    //                                              suite); weaker shapes
    //                                              degrade gracefully.
    //   session 4+: callsite expansion            → tighter still
    let high_confidence_count = answers
        .iter()
        .filter(|a| a.confidence >= 0.85)
        .count();
    let multi_query_symbol_files: Vec<&str> = symbol_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_queries")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() >= 2)
                .unwrap_or(false)
        })
        .filter_map(|item| item.path.as_deref())
        .collect();
    let strong_text_files: Vec<&str> = text_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_terms")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() >= 2)
                .unwrap_or(false)
        })
        .filter_map(|item| item.path.as_deref())
        .collect();
    // Symbol + text agree on the same file = a strong local signal.
    let cross_corroborated: Vec<&&str> = multi_query_symbol_files
        .iter()
        .filter(|p| strong_text_files.contains(p))
        .collect();

    // Callsite evidence raises trust further: a file with multi-query
    // symbol matches AND callers-of evidence is approaching test-suite
    // territory. We track multi-symbol callsite files separately
    // because that's the strongest dispatch signal we produce.
    let strong_callsite_files: Vec<&str> = callsite_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("symbols")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len() >= 2)
                .unwrap_or(false)
        })
        .filter_map(|item| item.path.as_deref())
        .collect();
    let triple_corroborated: bool = !strong_callsite_files.is_empty()
        && (!cross_corroborated.is_empty()
            || !multi_query_symbol_files.is_empty());

    let policy_kind = if answers.is_empty() && nav_hints.is_empty() {
        "failed"
    } else if !cross_corroborated.is_empty()
        || !multi_query_symbol_files.is_empty()
    {
        "answer_candidate"
    } else if !text_items.is_empty() || !symbol_items.is_empty() {
        // Some text or symbol evidence but not strong enough to defend.
        "needs_verification"
    } else {
        "needs_verification"
    };
    let evidence_level = if triple_corroborated {
        "graph+symbol+text+callsite"
    } else if !cross_corroborated.is_empty() {
        "graph+symbol+text"
    } else if !strong_callsite_files.is_empty() {
        "graph+symbol+callsite"
    } else if !multi_query_symbol_files.is_empty() && !text_items.is_empty() {
        "graph+symbol+text-weak"
    } else if !multi_query_symbol_files.is_empty() {
        "graph+symbol"
    } else if !callsite_items.is_empty() {
        "graph+callsite-weak"
    } else if !text_items.is_empty() {
        "graph+text"
    } else if !symbol_items.is_empty() {
        "graph+symbol-weak"
    } else {
        "graph"
    };
    let safe_to_use_as_answer = matches!(policy_kind, "answer_candidate");
    let trust_reason = match policy_kind {
        "answer_candidate" if !cross_corroborated.is_empty() => format!(
            "Symbol search and source-text both matched {} candidate file(s); \
             cross-corroborated evidence treated as authoritative.",
            cross_corroborated.len()
        ),
        "answer_candidate" => format!(
            "Symbol search matched {} distinct request terms in the same \
             file; treating as authoritative answer candidate.",
            symbol_items
                .iter()
                .filter_map(|item| item
                    .evidence
                    .get("matched_queries")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.len()))
                .max()
                .unwrap_or(2)
        ),
        "failed" => {
            "No anchors, in-scope files, symbol matches, or source-text hits."
                .to_string()
        }
        _ => "Evidence present but not strong enough to defend as an \
              authoritative answer. Verify before acting."
            .to_string(),
    };
    let trust_policy = TrustPolicy {
        safe_to_use_as_answer,
        safe_to_use_as_navigation: !answers.is_empty() || !nav_hints.is_empty(),
        evidence_level: evidence_level.to_string(),
        authoritative_answer_count: high_confidence_count,
        navigation_hint_count,
        degraded: false,
        trust_policy: policy_kind,
        reason: trust_reason,
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

    // Compute fields that need by-ref reads BEFORE moving the values
    // into the response struct.
    let safe_to_use_as_answer = trust_policy.safe_to_use_as_answer;
    let safe_to_use_as_navigation = trust_policy.safe_to_use_as_navigation;
    let verification_steps = build_verification_steps(
        &answers,
        &nav_hints,
        &trust_policy,
        text_items,
    );

    // Build output_adapters and resolved_parameters only when the
    // caller has asked for verbose shaping. Mirrors Python's
    // _trim_explore_response gate at cli.py:1732-1743. At compact the
    // canonical answer[] is sufficient; the repackaging is redundant.
    let verbose = matches!(params.detail, Detail::Full) || params.show_observability;
    let output_adapters = if verbose {
        Some(build_output_adapters(
            &answers,
            &nav_hints,
            &next_actions,
            &verification_steps,
            params.detail,
        ))
    } else {
        None
    };
    let resolved_parameters = if verbose {
        Some(params.to_json())
    } else {
        None
    };

    // At compact, truncate verification_steps to 2 (mirrors Python's
    // _trim_explore_response at cli.py:1752-1755). Agents follow 1-2
    // before deciding; emitting all 5+ inflates response by ~30%.
    let verification_steps = if matches!(params.detail, Detail::Compact)
        && !params.show_observability
    {
        verification_steps.into_iter().take(2).collect()
    } else {
        verification_steps
    };

    ExploreResponse {
        schema_version: "aethyme-explore-v1",
        mode: "explore",
        intent: intent.as_str(),
        intent_source: intent_source.as_str(),
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
            // Aggregate "overall" confidence is intentionally None for the
            // task/behavior path: each AnswerItem carries its own
            // confidence, and a single weighted aggregate would obscure
            // the distinction between "one strong + four weak" and "five
            // medium." Consumers should read per-item confidence + the
            // trust_policy verdict.
            overall: None,
            answer_summary,
            excluded_summary: ConfidenceSummary::default(),
            analyzed_summary: serde_json::json!({}),
        },
        safe_to_use_as_answer,
        safe_to_use_as_navigation,
        trust_policy,
        degraded_reasons: Vec::new(),
        verification_steps,
        next_actions,
        available_specialized_intents: vec![
            "behavior_localization_query",
            "usage_boundary_query",
        ],
        output_adapters,
        resolved_parameters,
    }
}

/// Build the `output_adapters.task_localization_json` structure that
/// downstream consumers (skills, eval scoring, agent post-processing)
/// read instead of poking through the heterogeneous `answer[]` list.
///
/// Filtering rules mirror Python at `cli.py:2088-2118`:
///   - candidate_files  → kinds {symbol_search_file, source_text_file,
///                        call_site_file, filesystem_file, anchor,
///                        in_scope_file} that have a `path`.
///   - candidate_symbols → kinds {symbol_search, in_scope_symbol} OR
///                        items whose evidence carries `anchor_kind ==
///                        "symbol"`.
///   - navigation_hints → empty when `detail == compact`; otherwise
///                        echoes the response's nav_hints.
fn build_output_adapters(
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    next_actions: &[String],
    verification_steps: &[serde_json::Value],
    detail: Detail,
) -> serde_json::Value {
    let candidate_files: Vec<&AnswerItem> = answers
        .iter()
        .filter(|item| item.path.is_some())
        .filter(|item| matches!(
            item.kind.as_str(),
            "symbol_search_file"
                | "source_text_file"
                | "call_site_file"
                | "filesystem_file"
                | "anchor"
                | "in_scope_file"
        ))
        .collect();
    let candidate_symbols: Vec<&AnswerItem> = answers
        .iter()
        .filter(|item| {
            matches!(item.kind.as_str(), "symbol_search" | "in_scope_symbol")
                || item
                    .evidence
                    .get("anchor_kind")
                    .and_then(|v| v.as_str())
                    == Some("symbol")
        })
        .collect();
    let navigation_hints_field: Vec<&AnswerItem> = match detail {
        Detail::Compact => Vec::new(),
        _ => nav_hints.iter().collect(),
    };
    serde_json::json!({
        "task_localization_json": {
            "candidate_files": candidate_files,
            "candidate_symbols": candidate_symbols,
            "next_actions": next_actions,
            "verification_steps": verification_steps,
            "navigation_hints": navigation_hints_field,
        }
    })
}

/// Tailor the verification steps to the evidence we actually produced.
///
/// Mirrors the philosophy of Python's
/// `_task_localization_verification_steps`: the steps an agent should
/// take depend on what we found and how confident we are. Generic
/// "verify before acting" is honest but unhelpful; pointing at a
/// specific line ref or symbol gives the agent a concrete thing to do.
///
/// Step priority (we emit at most 4):
///   1. If text evidence with line_refs → read the cited line(s)
///   2. If symbol evidence → grep callers/dispatch sites of the symbol
///   3. If failed/no answers → suggest broadening the request or running
///      Python explore for richer evidence
///   4. Generic "open top answer and confirm" as a final fallback
fn build_verification_steps(
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    trust_policy: &TrustPolicy,
    text_items: &[AnswerItem],
) -> Vec<serde_json::Value> {
    let mut steps: Vec<serde_json::Value> = Vec::new();

    // Step 1: cite a specific line ref the agent can read.
    if let Some(top_text) = text_items.first() {
        if let Some(line_refs) =
            top_text.evidence.get("line_refs").and_then(|v| v.as_array())
        {
            if let Some(first_ref) = line_refs.first() {
                let line = first_ref.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                let path = top_text.path.as_deref().unwrap_or("(unknown)");
                steps.push(serde_json::json!({
                    "step": format!(
                        "Read {}:{} and confirm the matched terms appear in \
                         executable code (not a comment or stringified \
                         translation).",
                        path, line
                    ),
                    "rationale": "Source-text evidence is line-level; verifying \
                                  the line context takes one Read tool call.",
                }));
            }
        }
    }

    // Step 2: when symbol-search anchored a file (different from text
    // top), suggest checking the symbol's call sites.
    let symbol_file = answers
        .iter()
        .find(|a| a.kind == "symbol_search_file")
        .or_else(|| nav_hints.iter().find(|h| h.kind == "anchor_symbol"));
    if let Some(item) = symbol_file {
        let path = item.path.as_deref().unwrap_or(item.target.as_str());
        let matched: Option<Vec<String>> = item
            .evidence
            .get("matched_queries")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|t| t.as_str().map(String::from)).collect());
        let term_hint = match matched {
            Some(t) if !t.is_empty() => format!(" (matched {})", t.join(", ")),
            _ => String::new(),
        };
        steps.push(serde_json::json!({
            "step": format!(
                "Search the codebase for callers of the symbol(s) Aethyme \
                 found in {}{}; the call sites confirm whether this file is \
                 the entry point or just one of many implementations.",
                path, term_hint
            ),
            "rationale": "Symbol-name matches show definition; callers show \
                          actual usage and surface dispatch.",
        }));
    }

    // Step 3: degraded/failed → suggest rerun.
    if trust_policy.trust_policy == "failed"
        || trust_policy.trust_policy == "needs_verification"
    {
        if answers.is_empty() && nav_hints.is_empty() {
            steps.push(serde_json::json!({
                "step": "Broaden the request: include domain terms (entity \
                         names, file types) or rerun with `--detail standard` \
                         for wider symbol/text coverage.",
                "rationale": "No candidates surfaced — the request may not \
                              tokenize into useful query terms.",
            }));
        } else if trust_policy.trust_policy == "needs_verification" {
            steps.push(serde_json::json!({
                "step": "If the task requires high confidence, rerun with \
                         `--detail standard` or via the Python explore for \
                         additional source-callsite expansion and evidence \
                         aggregation.",
                "rationale": "Native session-3 path covers the common cases; \
                              richer evidence sources are deferred to the \
                              Python orchestrator.",
            }));
        }
    }

    // Final fallback if we somehow produced nothing actionable above.
    if steps.is_empty() {
        steps.push(serde_json::json!({
            "step": "Open the top answer[] item and confirm it matches the \
                     task before relying on it.",
            "rationale": "Graph navigation found this candidate; verifying \
                          that the file genuinely handles the task is fast.",
        }));
    }

    // Cap at 4: longer lists are noise; the first 1-2 are typically the
    // strongest moves an agent can take.
    steps.truncate(4);
    steps
}

/// Group symbol-search hits by file, rank by query coverage + cumulative
/// score, emit AnswerItems with `kind = "symbol_search_file"`. Mirrors
/// `_task_localization_symbol_file_items` in the Python orchestrator so
/// downstream consumers see the same shape.
///
/// Confidence scoring:
///   - 2+ distinct queries matched in this file → 0.88 (multi-term match)
///   - 1 query matched                          → 0.76
///
/// These are the same numbers Python uses; preserving them keeps the
/// trust-policy heuristics consistent across implementations.
fn build_symbol_file_items(
    symbol_matches: &SymbolBatchResults,
    cap: usize,
) -> Vec<AnswerItem> {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    #[derive(Default)]
    struct PerFile {
        queries: BTreeSet<String>,
        symbols: Vec<serde_json::Value>,
        score: i64,
    }

    let mut by_file: BTreeMap<String, PerFile> = BTreeMap::new();
    // Iterate queries in original order so the most relevant query
    // dominates `symbols[0]` for a given file.
    for query in &symbol_matches.query_order {
        let Some(hits) = symbol_matches.by_query.get(query) else {
            continue;
        };
        for hit in hits {
            if hit.file.trim().is_empty() {
                continue;
            }
            let entry = by_file.entry(hit.file.clone()).or_default();
            entry.queries.insert(query.clone());
            entry.symbols.push(serde_json::json!({
                "name": hit.name,
                "kind": hit.kind,
                "line": hit.line,
                "score": hit.score,
            }));
            entry.score += hit.score;
        }
    }

    let mut ranked: Vec<(String, PerFile)> = by_file.into_iter().collect();
    ranked.sort_by(|(la, a), (lb, b)| {
        // Primary: more distinct queries. Secondary: total score.
        // Tertiary: filename alphabetical for stability.
        b.queries
            .len()
            .cmp(&a.queries.len())
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| la.cmp(lb))
    });

    let mut items: Vec<AnswerItem> = Vec::new();
    for (file_path, summary) in ranked.into_iter().take(cap) {
        let matched_queries: Vec<String> = summary.queries.iter().cloned().collect();
        let multi = matched_queries.len() > 1;
        let confidence = if multi { 0.88 } else { 0.76 };
        let reason = if multi {
            "Multiple request terms matched symbols in this file."
        } else {
            "A request term matched a symbol in this file."
        };
        let symbols_preview: Vec<serde_json::Value> =
            summary.symbols.into_iter().take(5).collect();
        items.push(AnswerItem {
            kind: "symbol_search_file".into(),
            target: file_path.clone(),
            path: Some(file_path),
            status: "candidate".into(),
            confidence,
            reason: reason.into(),
            role: "candidate".into(),
            evidence: serde_json::json!({
                "source": "query-symbol",
                "matched_queries": matched_queries,
                "symbols": symbols_preview,
                "combined_score": summary.score,
            }),
        });
    }
    items
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

    fn empty_symbols() -> SymbolBatchResults {
        SymbolBatchResults::default()
    }

    fn symbols_for(file: &str, queries: &[(&str, i64)]) -> SymbolBatchResults {
        let mut by_query = std::collections::BTreeMap::new();
        let mut order = Vec::new();
        for (q, score) in queries {
            order.push((*q).to_string());
            by_query.insert(
                (*q).to_string(),
                vec![SymbolHit {
                    name: format!("hit_for_{q}"),
                    kind: "function".into(),
                    file: file.to_string(),
                    line: 42,
                    score: *score,
                }],
            );
        }
        SymbolBatchResults {
            query_order: order,
            by_query,
        }
    }

    #[test]
    fn build_response_synthesizes_answers_and_nav_hints() {
        let response = build_response(
            "find watchlist handlers",
            Intent::TaskLocalization,
            IntentSource::Default,
            &sample_view(),
            &empty_symbols(),
            &[],
            &[],
            &[],
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
            Intent::TaskLocalization,
            IntentSource::Default,
            &view,
            &empty_symbols(),
            &[],
            &[],
            &[],
            &ExploreParams {
                max_answer_items: 3,
                ..ExploreParams::default()
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
        let response = build_response(
            "nothing matches",
            Intent::TaskLocalization,
            IntentSource::Default,
            &view,
            &empty_symbols(),
            &[],
            &[],
            &[],
            &ExploreParams::default(),
        );
        assert_eq!(response.status, "degraded");
        assert!(response.answer.is_empty());
        assert!(response.navigation_hints.is_empty());
        assert_eq!(response.trust_policy.trust_policy, "failed");
    }

    #[test]
    fn trust_policy_without_symbol_evidence_is_needs_verification() {
        let response = build_response(
            "find handlers",
            Intent::TaskLocalization,
            IntentSource::Default,
            &sample_view(),
            &empty_symbols(),
            &[],
            &[],
            &[],
            &ExploreParams::default(),
        );
        // Without symbol evidence, anchors+scope alone don't earn
        // `answer_candidate` — the cross-corroboration is missing.
        assert!(!response.safe_to_use_as_answer);
        assert_eq!(response.trust_policy.trust_policy, "needs_verification");
        assert_eq!(response.trust_policy.evidence_level, "graph");
    }

    #[test]
    fn multi_query_symbol_match_elevates_to_answer_candidate() {
        let symbols = symbols_for(
            "src/auth/SessionStore.php",
            &[("session", 200), ("authenticate", 300)],
        );
        let response = build_response(
            "find session authenticate handlers",
            Intent::TaskLocalization,
            IntentSource::Default,
            &sample_view(),
            &symbols,
            &[],
            &[],
            &[],
            &ExploreParams::default(),
        );
        assert!(response.safe_to_use_as_answer);
        assert_eq!(response.trust_policy.trust_policy, "answer_candidate");
        assert_eq!(response.trust_policy.evidence_level, "graph+symbol");
        // The matched file should appear in answer[] as a symbol_search_file
        // ahead of in_scope_file items because symbol evidence is stronger.
        let symbol_match_position = response
            .answer
            .iter()
            .position(|a| a.kind == "symbol_search_file");
        assert!(
            symbol_match_position.is_some(),
            "symbol_search_file should be present in answer[]"
        );
    }

    #[test]
    fn single_query_symbol_match_stays_at_needs_verification() {
        let symbols = symbols_for("src/util/helpers.php", &[("helper", 100)]);
        let response = build_response(
            "find helper code",
            Intent::TaskLocalization,
            IntentSource::Default,
            &sample_view(),
            &symbols,
            &[],
            &[],
            &[],
            &ExploreParams::default(),
        );
        // One query matched is weak corroboration — bar to claim
        // `answer_candidate` is multi-term match in the SAME file.
        assert!(!response.safe_to_use_as_answer);
        assert_eq!(response.trust_policy.trust_policy, "needs_verification");
        assert_eq!(response.trust_policy.evidence_level, "graph+symbol-weak");
    }

    #[test]
    fn extract_symbol_queries_drops_stop_words_and_short_terms() {
        let queries = extract_symbol_queries(
            "Find the file that handles WatchedItem revisions",
        );
        // "find", "the", "that" are stop words. "Watcheditem" stays.
        assert!(queries.iter().any(|q| q.eq_ignore_ascii_case("WatchedItem")));
        assert!(queries.iter().any(|q| q.eq_ignore_ascii_case("revisions")));
        assert!(!queries.iter().any(|q| q.eq_ignore_ascii_case("the")));
        assert!(!queries.iter().any(|q| q.eq_ignore_ascii_case("find")));
    }

    #[test]
    fn extract_symbol_queries_adds_underscore_collapsed_variant() {
        let queries = extract_symbol_queries("trace add_watch behavior");
        // Both `add_watch` and `addwatch` should be present.
        let lower: Vec<String> =
            queries.iter().map(|q| q.to_ascii_lowercase()).collect();
        assert!(lower.contains(&"add_watch".to_string()));
        assert!(lower.contains(&"addwatch".to_string()));
    }

    #[test]
    fn extract_text_search_terms_extends_for_behavioural_words() {
        // For text search we keep behavioural keywords like "viewed" and
        // "seen" that the symbol-query helper drops. The trigger is
        // matching them in the request itself; if the request mentions
        // "watchlist" we add domain synonyms ("watched", "notification").
        let terms = extract_text_search_terms(
            "Bug: viewing a diff revision marks watchlist as seen",
        );
        let lower: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
        // The request word "viewing" survives (symbol-search would drop it
        // as too noisy, text-search keeps it).
        assert!(lower.contains(&"viewing".to_string()));
        // Domain expansions added by the watchlist trigger:
        assert!(lower.contains(&"watched".to_string()));
        assert!(lower.contains(&"notification".to_string()));
        // Domain expansions added by the diff/revision trigger:
        assert!(lower.contains(&"diff".to_string()));
        assert!(lower.contains(&"revisions".to_string()));
        // No duplicates (case-insensitive):
        let mut sorted = lower.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), lower.len(), "duplicate term in {lower:?}");
    }

    // ── Intent::auto_select heuristic ──────────────────────────────────

    #[test]
    fn auto_select_picks_behavior_for_change_verbs() {
        // Standard change-task openings.
        assert_eq!(
            Intent::auto_select("Add a new authentication provider"),
            Intent::BehaviorLocalization,
        );
        assert_eq!(
            Intent::auto_select("Implement caching for the snapshot builder"),
            Intent::BehaviorLocalization,
        );
        assert_eq!(
            Intent::auto_select("Fix the bug where viewing a diff marks all revisions as seen"),
            Intent::BehaviorLocalization,
        );
        assert_eq!(
            Intent::auto_select("Refactor the suppliers grader scoring logic"),
            Intent::BehaviorLocalization,
        );
        assert_eq!(
            Intent::auto_select("Remove the deprecated v1 API surface"),
            Intent::BehaviorLocalization,
        );
        assert_eq!(
            Intent::auto_select("Migrate from SurrealDB to redb"),
            Intent::BehaviorLocalization,
        );
    }

    #[test]
    fn auto_select_picks_task_for_read_only_verbs() {
        // "where/find/show/explain" — the user wants to LOCATE, not change.
        assert_eq!(
            Intent::auto_select("Where does suppliers grader live?"),
            Intent::TaskLocalization,
        );
        assert_eq!(
            Intent::auto_select("Find files that handle watchlist notifications"),
            Intent::TaskLocalization,
        );
        assert_eq!(
            Intent::auto_select("Show me the auth flow"),
            Intent::TaskLocalization,
        );
        assert_eq!(
            Intent::auto_select("Explain how the snapshot builder works"),
            Intent::TaskLocalization,
        );
        // No verb at all — default to task.
        assert_eq!(
            Intent::auto_select("authentication provider"),
            Intent::TaskLocalization,
        );
    }

    #[test]
    fn auto_select_only_scans_first_10_tokens() {
        // Long preamble whose CHANGE verb sits past the 10-token window
        // should NOT trigger BehaviorLocalization. Verbs front-load in
        // requests; mid-sentence verbs are usually descriptive.
        let request = "Where does the file that the recent CI failure last \
                       Tuesday refers to add a new feature live?";
        // "add" appears at token ~13. Should still be TaskLocalization.
        assert_eq!(
            Intent::auto_select(request),
            Intent::TaskLocalization,
        );
    }

    #[test]
    fn auto_select_ignores_verb_inside_word() {
        // "padding" contains "add" as substring — must NOT trigger.
        // Tokenization is the safety net: we match whole tokens, not
        // substrings.
        assert_eq!(
            Intent::auto_select("Where is the padding logic for the form?"),
            Intent::TaskLocalization,
        );
    }

    // ── suffix_class_rank: regression tests for the test-demote bugfix ─

    #[test]
    fn suffix_class_rank_demotes_source_language_tests() {
        // Source-language test files should rank BELOW non-test source
        // (4 vs 5). Pre-bugfix, the test arm was unreachable for these.
        assert!(suffix_class_rank("src/auth.rs") > suffix_class_rank("tests/auth_test.rs"));
        assert!(suffix_class_rank("backend/grader.py") > suffix_class_rank("backend/tests/test_grader.py"));
        assert!(suffix_class_rank("packages/auth/src/login.ts") > suffix_class_rank("packages/auth/src/login.test.ts"));
    }

    #[test]
    fn suffix_class_rank_orders_categories_correctly() {
        let source = suffix_class_rank("src/foo.rs");
        let test = suffix_class_rank("tests/foo_test.rs");
        let docs = suffix_class_rank("README.md");
        let config = suffix_class_rank("config.yml");
        let data = suffix_class_rank("data/users.json");
        let locale = suffix_class_rank("packages/app/locales/en.json");
        let lockfile = suffix_class_rank("package-lock.json");
        assert!(source > test);
        assert!(test > docs);
        assert!(docs > config);
        assert!(config > data);
        assert!(data >= lockfile);
        assert!(lockfile > locale);
    }

    #[test]
    fn auto_select_handles_punctuation_around_verbs() {
        // Common patterns where the verb has punctuation neighbors.
        assert_eq!(
            Intent::auto_select("Bug: \"add\" feature broken"),
            Intent::BehaviorLocalization,
        );
        assert_eq!(
            Intent::auto_select("TODO -- implement the missing handler"),
            Intent::BehaviorLocalization,
        );
    }
}
