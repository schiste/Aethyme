//! Native Rust orchestration for `aethyme explore`.
//!
//! V2 data-source contract:
//!
//! - `task_localization_query`: graph/navigation reads come from the
//!   read-only redb store; text and filename evidence are source-text /
//!   filesystem helpers.
//! - `behavior_localization_query`: same redb reads with wider explore
//!   policy defaults for change-oriented requests.
//! - `usage_boundary_query`: dispatched in `explore/usage_boundary.rs`;
//!   redb supplies seed discovery while source text supplies evidence.
//! - Source text and filename passes are intentionally not graph-backed.
//!
//! The production explore path must not construct `RepositoryMap` or depend
//! on the engine daemon for non-usage-boundary graph/navigation reads.
//!
//! Wire shape
//! ----------
//! Output JSON matches `aethyme-explore-v1` schema produced by the Python
//! `_explore_task_localization_query` at compact detail. A consumer that
//! reads `answer[]` + `safe_to_use_as_answer` + `trust_policy` works
//! identically against either implementation.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::graph::navigation::{task_anchors_view_redb, task_next_view_redb, task_scope_view_redb};
use crate::graph::search::{SearchHit, symbol_search_redb};
use crate::model::task::TaskInput;
use crate::store::redb::graph_store::{GraphStore, ReadOnlyGraphStore};

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
    /// Runtime read-source status. Emitted only with `detail=full` or
    /// `--show-observability` so compact answer-json stays stable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observability: Option<serde_json::Value>,
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

/// The two task_localization-shaped intents handled by the redb path.
///
/// `task_localization_query` is the default: bounded answer, compact
/// detail, conservative defaults. `behavior_localization_query` is for
/// change-tasks ("what would I edit to make X happen?") — same engine
/// call, wider params.
///
/// `usage_boundary_query` is dispatched separately because it has its
/// own hybrid analyzer path. This enum only covers the
/// task-localization-shaped intents; the third intent has its own entry point.
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
            "add",
            "adds",
            "adding",
            "implement",
            "implements",
            "implementing",
            "introduce",
            "introduces",
            "introducing",
            "create",
            "creates",
            "creating",
            "build",
            "builds",
            "building",
            "wire",
            "wires",
            "wiring",
            // Modifying
            "modify",
            "modifies",
            "modifying",
            "edit",
            "edits",
            "editing",
            "change",
            "changes",
            "changing",
            "update",
            "updates",
            "updating",
            "tweak",
            "tweaks",
            // Restructuring
            "refactor",
            "refactors",
            "refactoring",
            "restructure",
            "restructures",
            "restructuring",
            "rewrite",
            "rewrites",
            "rewriting",
            "rename",
            "renames",
            "renaming",
            "extract",
            "extracts",
            "extracting",
            // Fixing
            "fix",
            "fixes",
            "fixing",
            "repair",
            "repairs",
            "repairing",
            "resolve",
            "resolves",
            "resolving",
            "patch",
            "patches",
            "patching",
            // Removing
            "remove",
            "removes",
            "removing",
            "delete",
            "deletes",
            "deleting",
            "drop",
            "drops",
            "dropping",
            "deprecate",
            "deprecates",
            "deprecating",
            "retire",
            "retires",
            "retiring",
            // Migrating
            "migrate",
            "migrates",
            "migrating",
            "port",
            "ports",
            "porting",
            "convert",
            "converts",
            "converting",
        ];
        let lower = request.to_ascii_lowercase();
        // Look only at the first ~10 tokens — verbs front-load.
        let token_iter = lower
            .split(|c: char| {
                c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'')
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

/// One rung of the progressive-disclosure ladder. The agent invokes
/// `aethyme-engine-cli explore --depth N` (0..=3) to dial in just enough
/// context to act, paying the cost only for what it asks about.
///
/// Constraints when editing this table:
///
/// 1. **Each rung must be meaningfully different from the one below.**
///    If depth=2 returns the same content as depth=1 plus 2 lines of
///    snippet, agents will skip 1 and go straight to 2 — the level
///    isn't doing real budget work.
/// 2. **`max_response_tokens` is a soft cap.** The response builder
///    truncates the answer list when serialized output approaches this
///    threshold. Setting it lets the agent treat each call as "buy at
///    most $X of context" rather than "buy whatever the engine
///    decides."
/// 3. **depth=0 must stay genuinely cheap.** This is the discovery
///    rung — agents call it first to map what's relevant. If it
///    bloats, agents stop using the ladder and fall back to bulk
///    loading.
/// 4. **depth=3 is the only rung with `include_call_graph: true`.**
///    Call-graph closure is O(graph) per call; gating it behind the
///    most-specific rung prevents accidental call-graph fan-out on
///    cheap discovery calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisclosureLevel {
    pub max_items: usize,
    pub include_signatures: bool,
    pub include_snippets: bool,
    /// Per-item snippet length cap (lines). 0 = no snippets;
    /// `usize::MAX` = full content (only at depth=3).
    pub snippet_lines: usize,
    pub include_call_graph: bool,
    /// Soft cap on serialized response size. The response builder
    /// truncates the answer list when JSON output approaches this
    /// threshold. Approximate — token counts vary by tokenizer.
    ///
    /// **Status (v1, 2026-05-09):** the value is declared here and
    /// surfaced via `apply_disclosure_level`, but the response
    /// builder does NOT yet enforce it. depth=0 currently emits
    /// ~10 KB (13 items × ~800B per item baseline) rather than the
    /// nominal ~600. The budget will be enforced when the answer-
    /// item shape is trimmed at low depths (drop the nested
    /// evidence wrapper for path-only items). Until then the cap
    /// reads as documentation, not behavior. Tracked for follow-up.
    pub max_response_tokens: usize,
}

/// Progressive-disclosure budget table.
///
/// | depth | items | sigs | snippet | call_graph | ~tokens |
/// |-------|-------|------|---------|------------|---------|
/// | 0     | 15    | no   | —       | no         | ~600    |
/// | 1     | 8     | yes  | —       | no         | ~1500   |
/// | 2     | 3     | yes  | 20 ln   | no         | ~4000   |
/// | 3     | 1     | yes  | full    | yes        | ~8000   |
///
/// Adjusting these is allowed and expected — keep it a *single edit
/// here* rather than threading a new flag through the engine. The
/// constraint comment above lists the invariants any change must
/// preserve.
pub const DISCLOSURE_LEVELS: [DisclosureLevel; 4] = [
    // depth=0 — discovery: names + paths only, no signatures, no
    // snippets. Agents call this first to triage scope. The agent
    // pays ~600 tokens for a map of up to 15 candidates and decides
    // which one to escalate on.
    DisclosureLevel {
        max_items: 15,
        include_signatures: false,
        include_snippets: false,
        snippet_lines: 0,
        include_call_graph: false,
        max_response_tokens: 600,
    },
    // depth=1 — candidates: + signatures and per-item relevance hints.
    // Agents who escalated from depth=0 use this to disambiguate
    // between top candidates without yet paying for source.
    DisclosureLevel {
        max_items: 8,
        include_signatures: true,
        include_snippets: false,
        snippet_lines: 0,
        include_call_graph: false,
        max_response_tokens: 1500,
    },
    // depth=2 — snippets: + 20-line code excerpts for top 3.
    // Agents escalate here when they've narrowed to a small set and
    // need to see what each candidate actually does.
    DisclosureLevel {
        max_items: 3,
        include_signatures: true,
        include_snippets: true,
        snippet_lines: 20,
        include_call_graph: false,
        max_response_tokens: 4000,
    },
    // depth=3 — deep dive: full content + call-graph closure for one
    // anchor. The most expensive rung; intended for a final commit
    // before the agent acts.
    DisclosureLevel {
        max_items: 1,
        include_signatures: true,
        include_snippets: true,
        snippet_lines: usize::MAX,
        include_call_graph: true,
        max_response_tokens: 8000,
    },
];

#[derive(Debug, Clone)]
pub struct ExploreParams {
    pub max_answer_items: usize,
    /// Detail level: `compact`, `standard`, or `full`. Mirrors the Python
    /// `--detail` flag. Today only `compact` is fully implemented in the
    /// native path; standard/full fall back to Python at the call site.
    pub detail: Detail,
    /// Progressive-disclosure depth (0..=3). When `Some(N)`, applies
    /// `DISCLOSURE_LEVELS[N]` as caps over the existing fields —
    /// enforcing a budget-per-call rather than the bulk-load default.
    /// `None` (legacy default) preserves the pre-2026-05-09 behavior:
    /// caps come from `Detail` and explicit `--max-answer-items`.
    /// When both `--depth` and `--detail` are provided, depth wins
    /// (most-specific budget control). Call `apply_disclosure_level()`
    /// to materialize the table values into the existing param fields.
    pub depth: Option<u8>,
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
    /// callsite expansion pass. Each hit can expand incoming redb
    /// adjacency; setting this too high inflates response time on
    /// queries that match many symbols. 4 is the Python compact default
    /// and a reasonable cap.
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
        params.max_symbol_queries = params.max_symbol_queries.saturating_mul(factor);
        params.max_symbol_results = params.max_symbol_results.saturating_mul(factor);
        params.max_symbol_files = params.max_symbol_files.saturating_mul(factor);
        params.max_text_files = params.max_text_files.saturating_mul(factor);
        params.max_text_line_refs = params.max_text_line_refs.saturating_mul(factor);
        params.max_filename_hints = params.max_filename_hints.saturating_mul(factor);
    }
}

impl Default for ExploreParams {
    fn default() -> Self {
        Self {
            max_answer_items: 5, // matches Python compact default after f1e3da5
            detail: Detail::Compact,
            depth: None, // legacy detail-based path; --depth flips it
            max_symbol_queries: 5,
            max_symbol_results: 4,
            max_symbol_files: 8, // truncated when answer list fills
            max_text_files: 5,   // matches Python compact default
            max_text_line_refs: 2,
            max_filename_hints: 3,
            max_callsite_symbols: 4,   // Python compact default
            max_callsite_results: 4,   // Python compact default
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
            "depth": self.depth,
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

    /// Apply the disclosure-level table to the existing param fields.
    ///
    /// When `depth` is `Some(N)` (0..=3), this reads
    /// `DISCLOSURE_LEVELS[N]` and writes the corresponding caps into
    /// `max_answer_items`, `max_text_line_refs`, and the deeper
    /// per-knob fields. Existing non-cap fields (callsite, filename
    /// hints) are scaled proportionally so a depth=0 call doesn't
    /// pay for a wide callsite expansion that contradicts its budget.
    ///
    /// `depth` values outside 0..=3 are clamped to the nearest valid
    /// rung so callers can pass user-supplied integers without an
    /// explicit validation step. The clamping is silent because the
    /// CLI binary validates earlier; this method's robustness is
    /// belt-and-braces for embedded callers (Python via PyO3, future
    /// MCP wiring).
    pub fn apply_disclosure_level(&mut self) {
        let Some(raw_depth) = self.depth else {
            return;
        };
        let depth = (raw_depth as usize).min(DISCLOSURE_LEVELS.len() - 1);
        let level = DISCLOSURE_LEVELS[depth];

        self.max_answer_items = level.max_items;

        // Snippet inclusion gates `max_text_line_refs`. Levels without
        // snippets get 0 line_refs (no excerpts in evidence); levels
        // with snippets get a proportional cap (1 ref at depth=1
        // signature-only, more at higher rungs).
        if level.include_snippets {
            self.max_text_line_refs = match raw_depth {
                2 => 4,
                3 => 10,
                _ => 2,
            };
        } else {
            // depth=0 strips line_refs entirely (just paths/names).
            // depth=1 keeps 1 line_ref to surface the signature
            // line — that's the "+ signatures" promise.
            self.max_text_line_refs = if level.include_signatures { 1 } else { 0 };
        }

        // Cap downstream knobs so they don't crowd the budget.
        // Symbol files / filename hints / callsite expansion all
        // contribute to answer fan-out; scale them down at low depth.
        self.max_symbol_files = self.max_symbol_files.min(level.max_items);
        self.max_text_files = self.max_text_files.min(level.max_items);
        self.max_filename_hints = match raw_depth {
            0 => 0,
            1 => 2,
            _ => self.max_filename_hints,
        };

        // Callsite expansion is deeper than the depth 0/1 contract and
        // only meaningful when the agent is actively closing a loop.
        // Disable below depth=2.
        if raw_depth < 2 {
            self.max_callsite_symbols = 0;
            self.max_callsite_results = 0;
        }
    }
}

// ── errors ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ExploreError {
    DaemonNotRunning,
    DaemonRpc(String),
    InvalidResponse(String),
    /// Engine analyzer failure — used by redb/source-text paths that do
    /// not have a more specific user-error variant.
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

mod usage_boundary;
pub use usage_boundary::{UsageBoundaryParams, explore_usage_boundary};

// ── orchestration entry point ───────────────────────────────────────────

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

    let canonical_repo = repo
        .canonicalize()
        .map_err(|e| ExploreError::EngineAnalyzer(format!("canonicalize repo: {e}")))?;
    let store = GraphStore::open_read_only(&canonical_repo)
        .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?;
    let observability = graph_store_observability(&canonical_repo);

    // 1. Graph-derived view (anchors + scope + next).
    let view = task_localize_redb(&store, request)?;

    // 2. Symbol-search evidence. If a local store read fails after the
    //    task view has succeeded, keep going with anchors/text evidence
    //    rather than block the whole request.
    let symbol_queries = extract_symbol_queries(request);
    let symbol_queries = if symbol_queries.len() > params.max_symbol_queries {
        symbol_queries[..params.max_symbol_queries].to_vec()
    } else {
        symbol_queries
    };
    let symbol_matches = if symbol_queries.is_empty() {
        SymbolBatchResults::default()
    } else {
        match symbol_batch_redb(&store, &symbol_queries, params.max_symbol_results) {
            Ok(r) => r,
            Err(_) => SymbolBatchResults::default(),
        }
    };

    // 3. Source-text evidence. Runs ripgrep client-side against the repo
    //    filesystem; doesn't need redb. Tolerates ripgrep absence:
    //    we degrade to symbol-only without failing the request.
    let text_terms = extract_text_search_terms(request);
    let text_items = text_search::source_text_files(
        &canonical_repo,
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
    let filename_items =
        filename_token_matches(&canonical_repo, &symbol_queries, params.max_filename_hints);

    // 5. Callsite expansion. For each strong symbol hit, look up
    //    its incoming redb `calls` adjacency and emit `call_site_file`
    //    AnswerItems for the caller files. This is the deepest evidence
    //    layer: not "this file defines X" but "these files actually call
    //    X." A file appearing in BOTH symbol matches AND
    //    someone-else's-callsite is the strongest cross-corroboration we
    //    produce without running tests. Store read failure here degrades
    //    silently, matching the old evidence-layer tolerance.
    let callsite_items = compute_callsite_files(
        &store,
        &symbol_matches,
        request,
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
        observability,
    ))
}

mod callsite;
use callsite::compute_callsite_files;

fn task_localize_redb(
    store: &ReadOnlyGraphStore,
    request: &str,
) -> Result<serde_json::Value, ExploreError> {
    let task = TaskInput::from_task_text(request);
    let anchors = task_anchors_view_redb(store, &task)
        .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?;
    let scope = task_scope_view_redb(store, &task)
        .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?;
    let next = task_next_view_redb(store, &task)
        .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?;
    let rendered = crate::json::task_localization_view(&anchors, &scope, &next);
    serde_json::from_str(&rendered)
        .map_err(|e| ExploreError::InvalidResponse(format!("redb task-localize JSON: {e}")))
}

fn symbol_batch_redb(
    store: &ReadOnlyGraphStore,
    queries: &[String],
    limit: usize,
) -> Result<SymbolBatchResults, ExploreError> {
    let mut by_query: std::collections::BTreeMap<String, Vec<SymbolHit>> =
        std::collections::BTreeMap::new();
    for query in queries {
        let parsed = symbol_search_redb(store, query, limit)
            .map_err(|e| ExploreError::EngineAnalyzer(e.to_string()))?
            .into_iter()
            .map(SymbolHit::from_search_hit)
            .collect();
        by_query.insert(query.clone(), parsed);
    }
    Ok(SymbolBatchResults {
        query_order: queries.to_vec(),
        by_query,
    })
}

fn graph_store_observability(repo: &Path) -> serde_json::Value {
    let store_path = GraphStore::final_path(repo);
    let fragments_path = repo.join(".aethyme").join("graph");
    let store_modified = modified_unix_secs(&store_path);
    let newest_fragment = newest_fragment_modified_unix_secs(&fragments_path);
    let stale = match (store_modified, newest_fragment) {
        (Some(store), Some(fragment)) => Some(fragment > store),
        _ => None,
    };
    let status = match stale {
        Some(true) => "stale",
        Some(false) => "fresh",
        None if store_path.is_file() => "unknown",
        None => "missing",
    };

    serde_json::json!({
        "graph_store": {
            "backend": "redb",
            "status": status,
            "exists": store_path.is_file(),
            "fragments_exist": fragments_path.is_dir(),
            "stale": stale,
            "store_modified_unix": store_modified,
            "newest_fragment_modified_unix": newest_fragment,
        }
    })
}

fn modified_unix_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(system_time_to_unix_secs)
}

fn newest_fragment_modified_unix_secs(root: &Path) -> Option<u64> {
    let mut newest = modified_unix_secs(root);
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                newest = newest.max(modified_unix_secs(&path));
            }
        }
    }
    newest
}

fn system_time_to_unix_secs(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

#[derive(Debug, Default)]
pub(super) struct SymbolBatchResults {
    /// Original query order — preserves user-intent order across the
    /// alphabetical BTreeMap iteration.
    pub(super) query_order: Vec<String>,
    pub(super) by_query: std::collections::BTreeMap<String, Vec<SymbolHit>>,
}

#[derive(Debug, Clone)]
pub(super) struct SymbolHit {
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) file: String,
    pub(super) line: u64,
    pub(super) score: i64,
}

impl SymbolHit {
    fn from_search_hit(hit: SearchHit) -> Self {
        SymbolHit {
            name: hit.name,
            kind: hit.kind,
            file: hit.file,
            line: hit.line as u64,
            score: i64::from(hit.score),
        }
    }
}

mod filename_match;
use filename_match::filename_token_matches;

mod ranking;

mod text_search;
pub(crate) use text_search::extract_text_search_terms;

// ── symbol query extraction (Rust port of _request_symbol_queries) ──────
//
// Tokenizes `request`, drops English stop words and noisy single-letter
// tokens, builds the canonical query list. When a token contains an
// underscore we add the dropped-underscore variant too (so `add_watch`
// also queries `addwatch`). Order-preserving + de-duplicated lowercase.

const STOP_WORDS: &[&str] = &[
    "about",
    "after",
    "against",
    "also",
    "and",
    "before",
    "being",
    "between",
    "bug",
    "code",
    "command",
    "could",
    "defined",
    "does",
    "done",
    "file",
    "files",
    "find",
    "fix",
    "for",
    "from",
    "have",
    "here",
    "how",
    "implement",
    "implemented",
    "implementation",
    "into",
    "issue",
    "json",
    "located",
    "make",
    "marked",
    "marks",
    "need",
    "object",
    "not",
    "only",
    "output",
    "path",
    "prose",
    "question",
    "relative",
    "report",
    "repo",
    "repository",
    "request",
    "rules",
    "shape",
    "the",
    "should",
    "specific",
    "that",
    "their",
    "there",
    "this",
    "ticket",
    "seen",
    "viewed",
    "viewing",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "with",
    "would",
    "you",
];

pub(crate) fn extract_symbol_queries(request: &str) -> Vec<String> {
    let normalized = request.replace('`', " ");
    let mut raw_terms: Vec<String> = Vec::new();
    for token in normalized
        .replace('/', " ")
        .replace('-', " ")
        .split_whitespace()
    {
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
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
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

#[derive(Debug, Clone)]
struct TokenSubsystemSummary {
    id: &'static str,
    label: &'static str,
    score: i32,
    paths: std::collections::BTreeSet<String>,
    signals: std::collections::BTreeSet<&'static str>,
}

#[derive(Debug, Clone, Copy)]
struct TokenSubsystemMatch {
    id: &'static str,
    label: &'static str,
    base_score: i32,
    signal: &'static str,
}

fn token_subsystem_summaries(
    request: &str,
    item_groups: &[&[AnswerItem]],
) -> Vec<TokenSubsystemSummary> {
    if !ranking::auth_token_focus_from_request(request) {
        return Vec::new();
    }

    let mut by_id: std::collections::BTreeMap<&'static str, TokenSubsystemSummary> =
        std::collections::BTreeMap::new();
    for group in item_groups {
        for item in *group {
            let text = token_subsystem_item_text(item);
            for matched in token_subsystem_matches(&text) {
                let entry = by_id
                    .entry(matched.id)
                    .or_insert_with(|| TokenSubsystemSummary {
                        id: matched.id,
                        label: matched.label,
                        score: 0,
                        paths: std::collections::BTreeSet::new(),
                        signals: std::collections::BTreeSet::new(),
                    });
                entry.score = entry
                    .score
                    .max(matched.base_score + token_subsystem_item_score(item));
                entry.signals.insert(matched.signal);
                if let Some(path) = item.path.as_deref().filter(|path| !path.is_empty()) {
                    entry.paths.insert(path.to_string());
                } else if !item.target.is_empty() {
                    entry.paths.insert(item.target.clone());
                }
            }
        }
    }

    let mut summaries: Vec<TokenSubsystemSummary> =
        by_id.into_iter().map(|(_, summary)| summary).collect();
    summaries.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.label.cmp(right.label))
    });
    summaries
}

fn token_subsystem_item_score(item: &AnswerItem) -> i32 {
    let raw_bonus = item
        .evidence
        .get("ranking_bonus")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let ranking_bonus = raw_bonus.max(-200).min(200) as i32;
    let confidence = (item.confidence * 100.0).round() as i32;
    let kind_bonus = match item.kind.as_str() {
        "source_text_file" => 12,
        "symbol_search_file" => 10,
        "call_site_file" => 8,
        _ => 0,
    };
    confidence + ranking_bonus + kind_bonus
}

fn token_subsystem_item_text(item: &AnswerItem) -> String {
    let mut chunks = Vec::new();
    chunks.push(item.kind.clone());
    chunks.push(item.target.clone());
    chunks.push(item.reason.clone());
    if let Some(path) = item.path.as_deref() {
        chunks.push(path.to_string());
    }
    collect_json_strings(&item.evidence, &mut chunks, 24);
    chunks.join(" ").to_ascii_lowercase()
}

fn collect_json_strings(value: &serde_json::Value, output: &mut Vec<String>, limit: usize) {
    if output.len() >= limit {
        return;
    }
    match value {
        serde_json::Value::String(text) => output.push(text.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_json_strings(value, output, limit);
                if output.len() >= limit {
                    break;
                }
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                collect_json_strings(value, output, limit);
                if output.len() >= limit {
                    break;
                }
            }
        }
        _ => {}
    }
}

fn token_subsystem_matches(lower: &str) -> Vec<TokenSubsystemMatch> {
    let mut matches = Vec::new();
    if contains_any_text(
        lower,
        &["api keys", "api-key", "api_key", "api_keys", "apikey"],
    ) {
        matches.push(TokenSubsystemMatch {
            id: "api_keys",
            label: "API keys",
            base_score: 120,
            signal: "api_key_surface",
        });
    }
    if contains_any_text(
        lower,
        &["oidc", "openid", "id token", "id_token", "idtoken"],
    ) {
        matches.push(TokenSubsystemMatch {
            id: "oidc",
            label: "OIDC",
            base_score: 95,
            signal: "oidc_surface",
        });
    }
    if lower.contains("jws")
        || lower.contains("audit_jws")
        || (lower.contains("audit")
            && contains_any_text(lower, &["jwt", "signature", "signing", "signed"]))
    {
        matches.push(TokenSubsystemMatch {
            id: "audit_jws",
            label: "audit JWS",
            base_score: 90,
            signal: "audit_jws_surface",
        });
    }
    if lower.contains("auth0_management")
        || (lower.contains("auth0") && lower.contains("management"))
        || lower.contains("management_token")
        || lower.contains("management token")
    {
        matches.push(TokenSubsystemMatch {
            id: "auth0_management",
            label: "Auth0 management",
            base_score: 80,
            signal: "auth0_management_surface",
        });
    }
    if lower.contains("profile_integrity")
        || lower.contains("profile-integrity")
        || (lower.contains("profile") && lower.contains("integrity"))
    {
        matches.push(TokenSubsystemMatch {
            id: "profile_integrity",
            label: "profile-integrity",
            base_score: 70,
            signal: "profile_integrity_surface",
        });
    }
    if lower.contains("domain_verification")
        || lower.contains("domain verification")
        || lower.contains("domain-verification")
        || (lower.contains("domain") && contains_any_text(lower, &["verify", "verification"]))
    {
        matches.push(TokenSubsystemMatch {
            id: "domain_verification",
            label: "domain verification",
            base_score: 65,
            signal: "domain_verification_surface",
        });
    }
    matches
}

fn request_names_specific_token_subsystem(request: &str) -> bool {
    let lower = request.to_ascii_lowercase();
    !token_subsystem_matches(&lower).is_empty()
}

fn token_subsystem_ambiguity_value(summaries: &[TokenSubsystemSummary]) -> serde_json::Value {
    let subsystems: Vec<serde_json::Value> = summaries
        .iter()
        .enumerate()
        .map(|(index, summary)| {
            serde_json::json!({
                "rank": index + 1,
                "id": summary.id,
                "label": summary.label,
                "score": summary.score,
                "paths": summary.paths.iter().take(4).cloned().collect::<Vec<_>>(),
                "signals": summary.signals.iter().copied().collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "kind": "token_subsystem_ambiguity",
        "status": "needs_verification",
        "reason": "Multiple token/auth subsystems matched this request; verify the top 2 before committing to one subsystem.",
        "verify_top_n": 2,
        "subsystems": subsystems,
    })
}

fn top_token_subsystem_labels(summaries: &[TokenSubsystemSummary], limit: usize) -> String {
    summaries
        .iter()
        .take(limit)
        .map(|summary| summary.label)
        .collect::<Vec<_>>()
        .join(", ")
}

fn contains_any_text(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// Translate the redb task-localize view into the answer-json
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
    observability: serde_json::Value,
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
    // Anchors that should land in `answer[]` rather than
    // `navigation_hints[]`. Renamed from `anchor_file_items` on
    // 2026-05-12 when `"symbol"` anchors started being promoted
    // alongside `"file"` anchors. Same merge pass; same downstream
    // handling (kind="anchor"); the `evidence.anchor_kind` field
    // distinguishes which sub-kind produced the item.
    let mut anchor_items: Vec<AnswerItem> = Vec::new();

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
                anchor_items.push(AnswerItem {
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
            "symbol" => {
                // Promote symbol-kind anchors into `answer[]` as
                // first-class candidates (2026-05-12). Pre-fix these
                // landed in `navigation_hints[]` via the `other` arm
                // — agents reading only `answer[]` never saw them,
                // even though symbol-name match is at least as
                // specific as filename match.
                //
                // Confidence 0.80: slightly below file anchors
                // (0.85) to acknowledge that today's `symbol_search`
                // is token-substring-based and can produce noisy
                // matches (e.g. "marks → GrammarKsh"). When the
                // symbol-search ranking gets stricter (a separate
                // follow-up), this can move to 0.85 — one-line
                // change. The intermediate value also lets the
                // trust_policy machinery flag symbol anchors as
                // "candidate but verify" without special-casing.
                //
                // Dedup at merge step (path-based) means symbol
                // anchors for files ALREADY in `answer[]` via
                // text-match are dropped silently. The real impact
                // is on files NOT yet in answer[] — the long tail
                // of graph-derived candidates that text-match
                // missed.
                let path = file.map(String::from);
                anchor_items.push(AnswerItem {
                    kind: "anchor".into(),
                    target: id.to_string(),
                    path,
                    status: "candidate".into(),
                    confidence: 0.80,
                    reason,
                    role: "anchor".into(),
                    evidence: serde_json::json!({
                        "source": "task-localize.anchors",
                        "anchor_kind": "symbol",
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
    let symbol_items = build_symbol_file_items(symbol_matches, params.max_symbol_files, request);
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
        if let Some(existing) = answers
            .iter_mut()
            .find(|a| a.path.as_deref() == item.path.as_deref())
        {
            merge_symbol_search_evidence(existing, item);
            continue;
        }
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

    for item in &anchor_items {
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
        if answers
            .iter()
            .any(|a| a.path.as_deref() == Some(file.as_str()))
        {
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

    let token_summary_groups: [&[AnswerItem]; 4] = [
        answers.as_slice(),
        symbol_items.as_slice(),
        text_items,
        callsite_items,
    ];
    let token_subsystems = token_subsystem_summaries(request, &token_summary_groups);
    let token_subsystem_ambiguous =
        token_subsystems.len() >= 2 && !request_names_specific_token_subsystem(request);
    let ambiguous = if token_subsystem_ambiguous {
        vec![token_subsystem_ambiguity_value(&token_subsystems)]
    } else {
        Vec::new()
    };

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
    let high_confidence_count = answers.iter().filter(|a| a.confidence >= 0.85).count();
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
    // symbol matches AND callsite evidence is approaching test-suite
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
        && (!cross_corroborated.is_empty() || !multi_query_symbol_files.is_empty());

    let mut policy_kind = if answers.is_empty() && nav_hints.is_empty() {
        "failed"
    } else if !cross_corroborated.is_empty() || !multi_query_symbol_files.is_empty() {
        "answer_candidate"
    } else if !text_items.is_empty() || !symbol_items.is_empty() {
        // Some text or symbol evidence but not strong enough to defend.
        "needs_verification"
    } else {
        "needs_verification"
    };
    if token_subsystem_ambiguous && policy_kind == "answer_candidate" {
        policy_kind = "needs_verification";
    }
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
        _ if token_subsystem_ambiguous => format!(
            "Multiple token/auth subsystems matched ({}); verify the top 2 \
             before relying on one implementation.",
            top_token_subsystem_labels(&token_subsystems, 6)
        ),
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
        "failed" => "No anchors, in-scope files, symbol matches, or source-text hits.".to_string(),
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
    } else if token_subsystem_ambiguous {
        vec![
            "Verify the top 2 token/auth subsystems in ambiguous[] before \
             committing to one implementation."
                .into(),
            "Then read the top answer[] item and confirm it matches the \
             intended inbound or provider-management path."
                .into(),
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
        if token_subsystem_ambiguous {
            token_subsystems.as_slice()
        } else {
            &[]
        },
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
    let observability = if verbose { Some(observability) } else { None };

    // At compact, truncate verification_steps to 2 (mirrors Python's
    // _trim_explore_response at cli.py:1752-1755). Agents follow 1-2
    // before deciding; emitting all 5+ inflates response by ~30%.
    let verification_steps =
        if matches!(params.detail, Detail::Compact) && !params.show_observability {
            verification_steps.into_iter().take(2).collect()
        } else {
            verification_steps
        };

    // Post-conditions for `answers[]`. These are debug-only; they
    // document the response contract that a downstream agent or scoring
    // pipeline can rely on.
    //
    //   - cap: `answers.len() <= max_answer_items`. The dedup loop
    //     enforces this on every push; this assert guards against a
    //     future contributor adding an unguarded `answers.push(...)`
    //     without a cap check.
    //   - distinct paths: no two items share the same `Some(path)`.
    //     The merge-into-existing branch in the callsite dedup loop
    //     depends on this — if the same path appeared twice, only
    //     the first match would receive merged evidence.
    //   - kinds belong to the answer-track set (no nav_hint kinds
    //     leaking into `answer[]`).
    debug_assert!(
        answers.len() <= params.max_answer_items,
        "answer cap violated: {} > {}",
        answers.len(),
        params.max_answer_items
    );
    {
        let mut paths: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for item in &answers {
            if let Some(p) = item.path.as_deref() {
                debug_assert!(
                    paths.insert(p),
                    "duplicate path in answers[]: {p:?}; dedup contract violated"
                );
            }
        }
    }
    debug_assert!(
        answers.iter().all(|item| matches!(
            item.kind.as_str(),
            "anchor"
                | "in_scope_file"
                | "in_scope_symbol"
                | "symbol_search"
                | "symbol_search_file"
                | "source_text_file"
                | "call_site_file"
                | "filesystem_file"
        )),
        "answer item with unexpected kind: {:?}",
        answers.iter().map(|i| i.kind.as_str()).collect::<Vec<_>>()
    );

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
        ambiguous,
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
        available_specialized_intents: vec!["behavior_localization_query", "usage_boundary_query"],
        output_adapters,
        resolved_parameters,
        observability,
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
        .filter(|item| {
            matches!(
                item.kind.as_str(),
                "symbol_search_file"
                    | "source_text_file"
                    | "call_site_file"
                    | "filesystem_file"
                    | "anchor"
                    | "in_scope_file"
            )
        })
        .collect();
    let candidate_symbols: Vec<&AnswerItem> = answers
        .iter()
        .filter(|item| {
            matches!(item.kind.as_str(), "symbol_search" | "in_scope_symbol")
                || item.evidence.get("anchor_kind").and_then(|v| v.as_str()) == Some("symbol")
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

fn merge_symbol_search_evidence(existing: &mut AnswerItem, symbol_item: &AnswerItem) {
    let mut merged = serde_json::Map::new();
    for key in [
        "matched_queries",
        "symbols",
        "combined_score",
        "ranking_bonus",
        "ranking_signals",
    ] {
        if let Some(value) = symbol_item.evidence.get(key).cloned() {
            merged.insert(key.to_string(), value);
        }
    }
    if merged.is_empty() {
        return;
    }
    if let Some(obj) = existing.evidence.as_object_mut() {
        obj.insert(
            "also_symbol_search".to_string(),
            serde_json::Value::Object(merged),
        );
    }
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
///   1. If token/auth ambiguity exists → verify the top 2 subsystems
///   2. If text evidence with line_refs → read the cited line(s)
///   3. If symbol evidence → grep callers/dispatch sites of the symbol
///   4. If failed/no answers → suggest broadening the request or running
///      Python explore for richer evidence
///   5. Generic "open top answer and confirm" as a final fallback
fn build_verification_steps(
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    trust_policy: &TrustPolicy,
    text_items: &[AnswerItem],
    token_subsystems: &[TokenSubsystemSummary],
) -> Vec<serde_json::Value> {
    let mut steps: Vec<serde_json::Value> = Vec::new();

    if token_subsystems.len() >= 2 {
        steps.push(serde_json::json!({
            "step": format!(
                "Token/auth is ambiguous: Aethyme found multiple subsystems \
                 ({}). Verify the top 2 before committing to one subsystem.",
                top_token_subsystem_labels(token_subsystems, 6)
            ),
            "rationale": "Broad token requests can match API keys, OIDC, audit \
                          JWS, provider-management, profile-integrity, and \
                          domain-verification code. Checking the top two \
                          prevents anchoring on the first plausible token hit.",
        }));
    }

    // Step 1: cite a specific line ref the agent can read.
    if let Some(top_text) = text_items.first() {
        if let Some(line_refs) = top_text
            .evidence
            .get("line_refs")
            .and_then(|v| v.as_array())
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
        .find(|a| a.evidence.get("also_symbol_search").is_some())
        .or_else(|| answers.iter().find(|a| a.kind == "symbol_search_file"))
        .or_else(|| nav_hints.iter().find(|h| h.kind == "anchor_symbol"));
    if let Some(item) = symbol_file {
        let path = item.path.as_deref().unwrap_or(item.target.as_str());
        let matched: Option<Vec<String>> = item
            .evidence
            .get("matched_queries")
            .or_else(|| {
                item.evidence
                    .get("also_symbol_search")
                    .and_then(|value| value.get("matched_queries"))
            })
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect()
            });
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
    if trust_policy.trust_policy == "failed" || trust_policy.trust_policy == "needs_verification" {
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
    request: &str,
) -> Vec<AnswerItem> {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    #[derive(Default)]
    struct PerFile {
        queries: BTreeSet<String>,
        symbol_names: BTreeSet<String>,
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
            entry.symbol_names.insert(hit.name.clone());
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
        // Auth/token requests get one extra generic surface signal:
        // inbound request-path and credential issue/auth surfaces outrank
        // incidental token helpers. Non-auth requests return score 0 here,
        // preserving the historical query/score/path ordering.
        let a_surface = ranking::auth_token_surface_signals(
            la,
            &a.symbol_names.iter().cloned().collect::<Vec<_>>(),
            request,
        );
        let b_surface = ranking::auth_token_surface_signals(
            lb,
            &b.symbol_names.iter().cloned().collect::<Vec<_>>(),
            request,
        );
        // Primary: auth surface score when relevant. Secondary: more
        // distinct queries. Tertiary: total score. Final: stable path order.
        b_surface
            .score
            .cmp(&a_surface.score)
            .then_with(|| b.queries.len().cmp(&a.queries.len()))
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| la.cmp(lb))
    });

    let mut items: Vec<AnswerItem> = Vec::new();
    for (file_path, summary) in ranked.into_iter().take(cap) {
        let matched_queries: Vec<String> = summary.queries.iter().cloned().collect();
        let multi = matched_queries.len() > 1;
        let signals = ranking::auth_token_surface_signals(
            &file_path,
            &summary.symbol_names.iter().cloned().collect::<Vec<_>>(),
            request,
        );
        let surface_confidence_bonus = if signals.score >= 120 {
            0.04
        } else if signals.score >= 70 {
            0.02
        } else {
            0.0
        };
        let confidence: f64 = if multi { 0.88 } else { 0.76 };
        let confidence =
            (((confidence + surface_confidence_bonus).min(0.92_f64)) * 100.0).round() / 100.0;
        let reason = if multi {
            "Multiple request terms matched symbols in this file."
        } else {
            "A request term matched a symbol in this file."
        };
        let symbols_preview: Vec<serde_json::Value> = summary.symbols.into_iter().take(5).collect();
        let mut evidence = serde_json::json!({
            "source": "query-symbol",
            "matched_queries": matched_queries,
            "symbols": symbols_preview,
            "combined_score": summary.score,
        });
        if signals.score != 0 {
            if let Some(obj) = evidence.as_object_mut() {
                obj.insert(
                    "ranking_bonus".to_string(),
                    serde_json::json!(signals.score),
                );
                obj.insert(
                    "ranking_signals".to_string(),
                    serde_json::json!(signals.labels),
                );
            }
        }
        items.push(AnswerItem {
            kind: "symbol_search_file".into(),
            target: file_path.clone(),
            path: Some(file_path),
            status: "candidate".into(),
            confidence,
            reason: reason.into(),
            role: "candidate".into(),
            evidence,
        });
    }
    items
}

pub(super) fn bucket_confidence(items: &[AnswerItem]) -> ConfidenceSummary {
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
mod disclosure_tests {
    //! Tests for the progressive-disclosure ladder (`DISCLOSURE_LEVELS`
    //! + `ExploreParams::apply_disclosure_level`).
    //!
    //! The table encodes a budget contract: depth=0 is cheap discovery,
    //! depth=3 is expensive deep-dive. These tests pin invariants so a
    //! contributor adjusting the table doesn't accidentally violate
    //! the "each rung must be meaningfully different" rule.
    use super::*;

    #[test]
    fn disclosure_table_has_four_rungs() {
        // Pin the count — the CLI parser bounds-checks against this
        // length, and SKILL.md teaches a 4-level ladder. A future
        // change adding a 5th rung needs to update this test, the
        // CLI bound, and the skill teaching together.
        assert_eq!(DISCLOSURE_LEVELS.len(), 4);
    }

    #[test]
    fn disclosure_levels_are_meaningfully_different() {
        // Constraint #1 from the table comment: each rung must
        // differ from the one below in at least one observable
        // way. Implementation: walk pairs and assert at least one
        // field changes.
        for i in 0..(DISCLOSURE_LEVELS.len() - 1) {
            let a = DISCLOSURE_LEVELS[i];
            let b = DISCLOSURE_LEVELS[i + 1];
            let differs = a.max_items != b.max_items
                || a.include_signatures != b.include_signatures
                || a.include_snippets != b.include_snippets
                || a.snippet_lines != b.snippet_lines
                || a.include_call_graph != b.include_call_graph
                || a.max_response_tokens != b.max_response_tokens;
            assert!(
                differs,
                "DISCLOSURE_LEVELS[{i}] and [{}] are observably \
                 identical — agents will skip the cheaper rung",
                i + 1,
            );
        }
    }

    #[test]
    fn token_budgets_are_monotonically_increasing() {
        // Constraint: deeper rungs cost more. If depth=2 cost less
        // than depth=1, the ladder shape is broken — agents have no
        // reason to stop at lower rungs.
        let budgets: Vec<usize> = DISCLOSURE_LEVELS
            .iter()
            .map(|l| l.max_response_tokens)
            .collect();
        for i in 0..(budgets.len() - 1) {
            assert!(
                budgets[i] < budgets[i + 1],
                "budgets {} -> {} not strictly increasing: {budgets:?}",
                i,
                i + 1,
            );
        }
    }

    #[test]
    fn call_graph_only_at_max_depth() {
        // Constraint #4: call-graph closure is O(graph) per call;
        // gating it behind the deepest rung prevents accidental
        // fan-out. If a future change opens this up at lower
        // depths, that should be deliberate — and tested.
        for (i, level) in DISCLOSURE_LEVELS.iter().enumerate() {
            let expect = i == DISCLOSURE_LEVELS.len() - 1;
            assert_eq!(
                level.include_call_graph, expect,
                "DISCLOSURE_LEVELS[{i}].include_call_graph must be \
                 {expect} (only the deepest rung enables call-graph)",
            );
        }
    }

    #[test]
    fn depth_zero_strips_evidence() {
        // depth=0 is the discovery rung — cheap. Apply must zero
        // out line_refs and downstream knobs that crowd the budget.
        let mut p = ExploreParams::default();
        p.depth = Some(0);
        p.apply_disclosure_level();
        assert_eq!(p.max_answer_items, 15);
        assert_eq!(p.max_text_line_refs, 0);
        assert_eq!(p.max_filename_hints, 0);
        assert_eq!(p.max_callsite_symbols, 0);
        assert_eq!(p.max_callsite_results, 0);
    }

    #[test]
    fn depth_one_keeps_signature_line_only() {
        let mut p = ExploreParams::default();
        p.depth = Some(1);
        p.apply_disclosure_level();
        assert_eq!(p.max_answer_items, 8);
        // Exactly 1 line_ref to surface the signature line — the
        // "+ signatures" promise without the snippet cost.
        assert_eq!(p.max_text_line_refs, 1);
        // Callsite expansion still gated below depth=2.
        assert_eq!(p.max_callsite_symbols, 0);
    }

    #[test]
    fn depth_two_enables_snippets_and_callsites() {
        let mut p = ExploreParams::default();
        p.depth = Some(2);
        p.apply_disclosure_level();
        assert_eq!(p.max_answer_items, 3);
        assert!(p.max_text_line_refs > 0);
        assert!(p.max_callsite_symbols > 0);
    }

    #[test]
    fn depth_three_pulls_full_content() {
        let mut p = ExploreParams::default();
        p.depth = Some(3);
        p.apply_disclosure_level();
        assert_eq!(p.max_answer_items, 1);
        // depth=3 has the call-graph flag — verify the table value.
        assert!(DISCLOSURE_LEVELS[3].include_call_graph);
        assert_eq!(DISCLOSURE_LEVELS[3].snippet_lines, usize::MAX);
    }

    #[test]
    fn out_of_range_depth_clamps_to_max() {
        // The CLI binary validates 0..=3 explicitly, but the
        // method itself is robust against bad inputs from future
        // embedded callers (PyO3, MCP). Clamps silently to the
        // top rung rather than panicking — defensive only.
        let mut p = ExploreParams::default();
        p.depth = Some(99);
        p.apply_disclosure_level();
        // Should land on the deepest rung's caps.
        assert_eq!(p.max_answer_items, 1);
    }

    #[test]
    fn no_depth_leaves_params_untouched() {
        // depth=None means use legacy detail-based defaults.
        // apply_disclosure_level must be a no-op in this case.
        let mut p = ExploreParams::default();
        let before_max_items = p.max_answer_items;
        let before_callsite = p.max_callsite_symbols;
        p.apply_disclosure_level();
        assert_eq!(p.max_answer_items, before_max_items);
        assert_eq!(p.max_callsite_symbols, before_callsite);
    }
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
            test_observability(),
        );
        assert!(!response.answer.is_empty(), "expected at least one answer");
        assert!(
            response
                .answer
                .iter()
                .any(|a| a.path.as_deref() == Some("includes/Watchlist/WatchedItemStore.php")),
            "anchor file should land in answer[]"
        );
        assert!(
            response
                .answer
                .iter()
                .any(|a| a.path.as_deref() == Some("includes/Specials/SpecialEditWatchlist.php")),
            "in-scope file should land in answer[]"
        );
        assert!(
            response
                .navigation_hints
                .iter()
                .any(|h| h.target == "includes/Watchlist"),
            "folder anchor should land in navigation_hints[]"
        );
    }

    #[test]
    fn build_response_promotes_symbol_anchors_into_answer() {
        // Regression test for the 2026-05-12 symbol-anchor promotion.
        //
        // Pre-fix: anchors with `kind: "symbol"` (produced by the
        // Unknown arm of `resolve_anchors` after 7a01c32) landed in
        // `navigation_hints[]` via the generic `other` arm. Agents
        // reading only `answer[]` never saw them, even though
        // symbol-name match is at least as specific as filename match.
        //
        // Post-fix: symbol anchors push into `answer[]` as
        // `kind: "anchor"` items with `evidence.anchor_kind: "symbol"`
        // and `confidence: 0.80`. Path-based dedup against text
        // matches still applies — see the merge step at the
        // "anchor_items" loop.
        let view = serde_json::json!({
            "task": "find watchlist handlers",
            "anchors": {
                "task": "find watchlist handlers",
                "anchors": [
                    {
                        "kind": "symbol",
                        // Qualified id mirrors the real shape:
                        // `fn:<repo>:<file>:<symbol>`.
                        "id": "fn:Mediawiki - Aethyme:includes/Page/WikiPage.php:doViewUpdates",
                        "file": "includes/Page/WikiPage.php",
                        "reason": "function-name-match via viewupdates"
                    }
                ]
            },
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
            "find watchlist handlers",
            Intent::TaskLocalization,
            IntentSource::Default,
            &view,
            &empty_symbols(),
            &[],
            &[],
            &[],
            &ExploreParams::default(),
            test_observability(),
        );

        let promoted = response
            .answer
            .iter()
            .find(|a| a.path.as_deref() == Some("includes/Page/WikiPage.php"))
            .expect(
                "symbol anchor for `doViewUpdates` should be promoted into \
                 answer[] (was previously routed to navigation_hints)",
            );

        // Pinned downstream contract: kind="anchor" with sub-kind in
        // evidence. This keeps the answer-kind list in the
        // `debug_assert!` at line ~1454 unchanged.
        assert_eq!(promoted.kind, "anchor");
        assert_eq!(
            promoted
                .evidence
                .get("anchor_kind")
                .and_then(|v| v.as_str()),
            Some("symbol")
        );

        // Confidence: 0.80 (chosen 2026-05-12). Slightly below file
        // anchors (0.85) to acknowledge symbol_search's current
        // token-substring naivete. If a future ranking improvement
        // makes symbol matches as reliable as filename matches, raise
        // this to 0.85 — single-line change.
        let confidence_diff = (promoted.confidence - 0.80_f64).abs();
        assert!(
            confidence_diff < 1e-9,
            "expected confidence 0.80; got {}",
            promoted.confidence
        );

        // The qualified symbol id flows into `target` so agents can
        // navigate to the specific symbol, not just the file.
        assert!(
            promoted.target.contains("doViewUpdates"),
            "target should preserve the qualified symbol id; got {}",
            promoted.target
        );

        // Sanity: the same anchor should NOT also appear in
        // navigation_hints (we promoted it, didn't duplicate it).
        assert!(
            response
                .navigation_hints
                .iter()
                .all(|h| h.path.as_deref() != Some("includes/Page/WikiPage.php")
                    || h.kind != "anchor_symbol"),
            "symbol anchor should be in answer[], not duplicated to \
             navigation_hints[]"
        );
    }

    #[test]
    fn build_response_symbol_anchor_dedup_against_text_match() {
        // When a file is ALREADY in answer[] via text-match, a symbol
        // anchor for the same file should be dropped (path-based
        // dedup at the merge step). The text-match item carries
        // line-level evidence which is stronger than "this file
        // contains a matching symbol name."
        let view = serde_json::json!({
            "task": "find watchlist handlers",
            "anchors": {
                "task": "find watchlist handlers",
                "anchors": [
                    {
                        "kind": "symbol",
                        "id": "fn:repo:WatchedItemStore.php:resetNotificationTimestamp",
                        "file": "includes/Watchlist/WatchedItemStore.php",
                        "reason": "function-name-match via notification"
                    }
                ]
            },
            "scope": {
                "in_scope_files": [],
                "in_scope_symbols": [],
                "in_scope_areas": [],
                "out_of_scope": [],
                "risks": []
            },
            "next": {"items": []}
        });
        let text_match = AnswerItem {
            kind: "source_text_file".into(),
            target: "includes/Watchlist/WatchedItemStore.php".into(),
            path: Some("includes/Watchlist/WatchedItemStore.php".into()),
            status: "candidate".into(),
            confidence: 0.75,
            reason: "text-match line evidence".into(),
            role: "candidate".into(),
            evidence: serde_json::json!({"source": "text-search"}),
        };
        let response = build_response(
            "find watchlist handlers",
            Intent::TaskLocalization,
            IntentSource::Default,
            &view,
            &empty_symbols(),
            &[text_match], // text-match for same file as symbol anchor
            &[],
            &[],
            &ExploreParams::default(),
            test_observability(),
        );

        // Exactly ONE answer for this file — the text-match one.
        let matches: Vec<_> = response
            .answer
            .iter()
            .filter(|a| a.path.as_deref() == Some("includes/Watchlist/WatchedItemStore.php"))
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly 1 answer for the file; got {}: {:?}",
            matches.len(),
            matches.iter().map(|m| &m.kind).collect::<Vec<_>>(),
        );
        // The survivor is the text-match (added first, stronger
        // evidence), not the symbol anchor.
        assert_eq!(matches[0].kind, "source_text_file");
    }

    #[test]
    fn build_response_caps_answer_count() {
        let mut view = sample_view();
        let scope = view.get_mut("scope").unwrap();
        scope["in_scope_files"] =
            serde_json::json!(["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
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
            test_observability(),
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
            test_observability(),
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
            test_observability(),
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
            test_observability(),
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
    fn build_response_auth_token_ranking_prefers_credential_boundary_over_incidental_token_view() {
        let mut by_query = std::collections::BTreeMap::new();
        by_query.insert(
            "token".to_string(),
            vec![
                SymbolHit {
                    name: "_issue_profile_integrity_token".into(),
                    kind: "function".into(),
                    file: "backend/accounts/platform_users_views.py".into(),
                    line: 18,
                    score: 900,
                },
                SymbolHit {
                    name: "generate_api_key".into(),
                    kind: "function".into(),
                    file: "backend/api_keys/models.py".into(),
                    line: 12,
                    score: 40,
                },
            ],
        );
        by_query.insert(
            "authenticate".to_string(),
            vec![
                SymbolHit {
                    name: "authenticate_api_key".into(),
                    kind: "function".into(),
                    file: "backend/api_keys/models.py".into(),
                    line: 36,
                    score: 40,
                },
                SymbolHit {
                    name: "authenticate_profile_token".into(),
                    kind: "function".into(),
                    file: "backend/accounts/platform_users_views.py".into(),
                    line: 42,
                    score: 40,
                },
            ],
        );
        let symbols = SymbolBatchResults {
            query_order: vec!["token".to_string(), "authenticate".to_string()],
            by_query,
        };

        let response = build_response(
            "trace token issuing and authentication behavior",
            Intent::TaskLocalization,
            IntentSource::Default,
            &sample_view(),
            &symbols,
            &[],
            &[],
            &[],
            &ExploreParams::default(),
            test_observability(),
        );

        let first_symbol_file = response
            .answer
            .iter()
            .find(|item| item.kind == "symbol_search_file")
            .expect("symbol evidence should be included in answer[]");
        assert_eq!(
            first_symbol_file.path.as_deref(),
            Some("backend/api_keys/models.py"),
            "credential boundary should outrank incidental high-scoring token view"
        );
        let signals = first_symbol_file
            .evidence
            .get("ranking_signals")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            signals
                .iter()
                .any(|signal| signal.as_str() == Some("issue_auth_credential_pair")),
            "expected credential-pair ranking evidence, got {signals:?}"
        );
    }

    #[test]
    fn build_response_auth_token_ambiguity_lists_ranked_subsystems_and_verifies_top_two() {
        let mut by_query = std::collections::BTreeMap::new();
        by_query.insert(
            "token".to_string(),
            vec![
                SymbolHit {
                    name: "generate_api_key".into(),
                    kind: "function".into(),
                    file: "backend/api_keys/models.py".into(),
                    line: 12,
                    score: 40,
                },
                SymbolHit {
                    name: "verify_oidc_token".into(),
                    kind: "function".into(),
                    file: "backend/accounts/oidc_validator.py".into(),
                    line: 28,
                    score: 80,
                },
                SymbolHit {
                    name: "verify_audit_jws".into(),
                    kind: "function".into(),
                    file: "backend/audit/jws.py".into(),
                    line: 32,
                    score: 70,
                },
                SymbolHit {
                    name: "get_management_token".into(),
                    kind: "function".into(),
                    file: "backend/accounts/auth0_management.py".into(),
                    line: 17,
                    score: 900,
                },
                SymbolHit {
                    name: "_issue_profile_integrity_token".into(),
                    kind: "function".into(),
                    file: "backend/accounts/platform_users_views.py".into(),
                    line: 18,
                    score: 850,
                },
                SymbolHit {
                    name: "issue_domain_verification_token".into(),
                    kind: "function".into(),
                    file: "backend/domains/domain_verification.py".into(),
                    line: 22,
                    score: 60,
                },
            ],
        );
        by_query.insert(
            "authenticate".to_string(),
            vec![SymbolHit {
                name: "authenticate_api_key".into(),
                kind: "function".into(),
                file: "backend/api_keys/models.py".into(),
                line: 36,
                score: 40,
            }],
        );
        let symbols = SymbolBatchResults {
            query_order: vec!["token".to_string(), "authenticate".to_string()],
            by_query,
        };

        let response = build_response(
            "trace token issuing and authentication behavior",
            Intent::TaskLocalization,
            IntentSource::Default,
            &sample_view(),
            &symbols,
            &[],
            &[],
            &[],
            &ExploreParams::default(),
            test_observability(),
        );

        assert!(
            !response.safe_to_use_as_answer,
            "broad token subsystem ambiguity should require verification"
        );
        assert_eq!(response.trust_policy.trust_policy, "needs_verification");

        let ambiguity = response
            .ambiguous
            .iter()
            .find(|item| {
                item.get("kind").and_then(|value| value.as_str())
                    == Some("token_subsystem_ambiguity")
            })
            .expect("broad token request should emit subsystem ambiguity");
        assert_eq!(
            ambiguity
                .get("verify_top_n")
                .and_then(|value| value.as_u64()),
            Some(2)
        );
        let subsystems = ambiguity
            .get("subsystems")
            .and_then(|value| value.as_array())
            .expect("subsystems should be an array");
        let labels: Vec<&str> = subsystems
            .iter()
            .filter_map(|item| item.get("label").and_then(|value| value.as_str()))
            .collect();
        for expected in [
            "API keys",
            "OIDC",
            "audit JWS",
            "Auth0 management",
            "profile-integrity",
            "domain verification",
        ] {
            assert!(
                labels.contains(&expected),
                "expected subsystem {expected:?} in {labels:?}"
            );
        }
        assert_eq!(
            labels.first().copied(),
            Some("API keys"),
            "inbound credential boundary should rank ahead of provider helpers"
        );
        let first_step = response
            .verification_steps
            .first()
            .and_then(|value| value.get("step"))
            .and_then(|value| value.as_str())
            .unwrap_or("");
        assert!(
            first_step.contains("top 2"),
            "first verification step should ask for top-2 subsystem verification: {first_step}"
        );
    }

    #[test]
    fn build_response_specific_token_subsystem_request_stays_decisive() {
        let mut by_query = std::collections::BTreeMap::new();
        by_query.insert(
            "token".to_string(),
            vec![
                SymbolHit {
                    name: "get_management_token".into(),
                    kind: "function".into(),
                    file: "backend/accounts/auth0_management.py".into(),
                    line: 17,
                    score: 900,
                },
                SymbolHit {
                    name: "generate_api_key".into(),
                    kind: "function".into(),
                    file: "backend/api_keys/models.py".into(),
                    line: 12,
                    score: 40,
                },
            ],
        );
        by_query.insert(
            "management".to_string(),
            vec![SymbolHit {
                name: "get_management_token".into(),
                kind: "function".into(),
                file: "backend/accounts/auth0_management.py".into(),
                line: 17,
                score: 900,
            }],
        );
        let symbols = SymbolBatchResults {
            query_order: vec!["token".to_string(), "management".to_string()],
            by_query,
        };

        let response = build_response(
            "trace Auth0 management token behavior",
            Intent::TaskLocalization,
            IntentSource::Default,
            &sample_view(),
            &symbols,
            &[],
            &[],
            &[],
            &ExploreParams::default(),
            test_observability(),
        );

        assert!(
            response.ambiguous.is_empty(),
            "a request that names the subsystem should not be downgraded by broad-token ambiguity"
        );
        assert!(response.safe_to_use_as_answer);
        let first_symbol_file = response
            .answer
            .iter()
            .find(|item| item.kind == "symbol_search_file")
            .expect("symbol evidence should be present");
        assert_eq!(
            first_symbol_file.path.as_deref(),
            Some("backend/accounts/auth0_management.py")
        );
    }

    #[test]
    fn build_response_verification_prefers_symbol_evidence_merged_into_top_text_answer() {
        let text_match = AnswerItem {
            kind: "source_text_file".into(),
            target: "backend/accounts/auth0_management.py".into(),
            path: Some("backend/accounts/auth0_management.py".into()),
            status: "candidate".into(),
            confidence: 0.87,
            reason: "text evidence".into(),
            role: "candidate".into(),
            evidence: serde_json::json!({
                "source": "source-text-search",
                "matched_terms": ["auth0", "management", "token"],
                "line_refs": [{"line": 114, "text": "management token", "matched_terms": ["auth0", "management", "token"]}],
            }),
        };
        let mut by_query = std::collections::BTreeMap::new();
        by_query.insert(
            "Auth0".to_string(),
            vec![
                SymbolHit {
                    name: "get_management_token".into(),
                    kind: "function".into(),
                    file: "backend/accounts/auth0_management.py".into(),
                    line: 83,
                    score: 160,
                },
                SymbolHit {
                    name: "record_auth0_idp_assets".into(),
                    kind: "function".into(),
                    file: "backend/accounts/idp_assets.py".into(),
                    line: 56,
                    score: 100,
                },
            ],
        );
        let symbols = SymbolBatchResults {
            query_order: vec!["Auth0".to_string()],
            by_query,
        };

        let response = build_response(
            "Trace Auth0 management token behavior.",
            Intent::TaskLocalization,
            IntentSource::Default,
            &sample_view(),
            &symbols,
            &[text_match],
            &[],
            &[],
            &ExploreParams::default(),
            test_observability(),
        );

        let top = response
            .answer
            .first()
            .expect("expected text answer to survive");
        assert_eq!(
            top.path.as_deref(),
            Some("backend/accounts/auth0_management.py")
        );
        assert!(
            top.evidence.get("also_symbol_search").is_some(),
            "same-path symbol evidence should merge into the top text answer"
        );
        let symbol_step = response
            .verification_steps
            .iter()
            .filter_map(|value| value.get("step").and_then(|step| step.as_str()))
            .find(|step| step.contains("callers of the symbol"))
            .unwrap_or("");
        assert!(
            symbol_step.contains("backend/accounts/auth0_management.py"),
            "symbol verification should follow the merged top answer, got {symbol_step}"
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
            test_observability(),
        );
        // One query matched is weak corroboration — bar to claim
        // `answer_candidate` is multi-term match in the SAME file.
        assert!(!response.safe_to_use_as_answer);
        assert_eq!(response.trust_policy.trust_policy, "needs_verification");
        assert_eq!(response.trust_policy.evidence_level, "graph+symbol-weak");
    }

    #[test]
    fn extract_symbol_queries_drops_stop_words_and_short_terms() {
        let queries = extract_symbol_queries("Find the file that handles WatchedItem revisions");
        // "find", "the", "that" are stop words. "Watcheditem" stays.
        assert!(
            queries
                .iter()
                .any(|q| q.eq_ignore_ascii_case("WatchedItem"))
        );
        assert!(queries.iter().any(|q| q.eq_ignore_ascii_case("revisions")));
        assert!(!queries.iter().any(|q| q.eq_ignore_ascii_case("the")));
        assert!(!queries.iter().any(|q| q.eq_ignore_ascii_case("find")));
    }

    #[test]
    fn extract_symbol_queries_adds_underscore_collapsed_variant() {
        let queries = extract_symbol_queries("trace add_watch behavior");
        // Both `add_watch` and `addwatch` should be present.
        let lower: Vec<String> = queries.iter().map(|q| q.to_ascii_lowercase()).collect();
        assert!(lower.contains(&"add_watch".to_string()));
        assert!(lower.contains(&"addwatch".to_string()));
    }

    #[test]
    fn extract_text_search_terms_extends_for_behavioural_words() {
        // For text search we keep behavioural keywords like "viewed" and
        // "seen" that the symbol-query helper drops. The trigger is
        // matching them in the request itself; if the request mentions
        // "watchlist" we add domain synonyms ("watched", "notification").
        let terms =
            extract_text_search_terms("Bug: viewing a diff revision marks watchlist as seen");
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
        assert_eq!(Intent::auto_select(request), Intent::TaskLocalization,);
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

    // ── response-shape snapshot tests ───────────────────────────────
    //
    // Goal: catch silent drift in the JSON response schema. Today's
    // hard-delete of Python explore removed the side-by-side comparison
    // that would have caught divergences naturally; these tests are
    // the standing replacement.
    //
    // What we assert:
    //   (a) Required top-level keys are always present (any agent can
    //       rely on `answer[]`, `trust_policy`, etc. existing).
    //   (b) Optional keys (`output_adapters`, `resolved_parameters`)
    //       gate correctly on `Detail::Full` / `show_observability`.
    //   (c) Nested types — e.g. `evidence.answer_count` is a number,
    //       `trust_policy.trust_policy` is one of the documented
    //       enum values.
    //
    // What we DON'T assert:
    //   - Exact bytes (too brittle — float precision, key order,
    //     etc. would break tests on benign refactors).
    //   - Exact answer-list contents (those are integration-test
    //     concerns; this is a schema test).

    fn empty_view() -> serde_json::Value {
        serde_json::json!({
            "task": "stub",
            "anchors": {"task": "stub", "anchors": []},
            "scope": {
                "task": "stub",
                "navigation_order": [],
                "in_scope_files": [],
                "in_scope_symbols": [],
                "in_scope_areas": [],
                "out_of_scope": [],
                "risks": []
            },
            "next": {"target": "stub", "relation": "next", "items": []}
        })
    }

    fn empty_symbol_matches() -> SymbolBatchResults {
        SymbolBatchResults::default()
    }

    fn test_observability() -> serde_json::Value {
        serde_json::json!({
            "graph_store": {
                "backend": "redb",
                "status": "fresh",
                "exists": true,
                "stale": false,
            }
        })
    }

    fn build_minimal_response(detail: Detail, show_observability: bool) -> ExploreResponse {
        let view = empty_view();
        let symbols = empty_symbol_matches();
        let params = ExploreParams {
            detail,
            show_observability,
            ..ExploreParams::default()
        };
        build_response(
            "stub request",
            Intent::TaskLocalization,
            IntentSource::Default,
            &view,
            &symbols,
            &[],
            &[],
            &[],
            &params,
            test_observability(),
        )
    }

    /// Required top-level keys that EVERY response must carry. Adding
    /// or removing a key here is a schema-breaking change for downstream
    /// consumers; do it deliberately, not as a side-effect.
    const REQUIRED_TOP_LEVEL_KEYS: &[&str] = &[
        "schema_version",
        "mode",
        "intent",
        "intent_source",
        "status",
        "request",
        "answer",
        "navigation_hints",
        "excluded",
        "ambiguous",
        "evidence",
        "confidence",
        "safe_to_use_as_answer",
        "safe_to_use_as_navigation",
        "trust_policy",
        "degraded_reasons",
        "verification_steps",
        "next_actions",
        "available_specialized_intents",
    ];

    #[test]
    fn response_compact_has_all_required_keys() {
        let response = build_minimal_response(Detail::Compact, false);
        let json = serde_json::to_value(&response).unwrap();
        let obj = json.as_object().expect("response is a JSON object");
        for key in REQUIRED_TOP_LEVEL_KEYS {
            assert!(
                obj.contains_key(*key),
                "compact response missing required key: {key}"
            );
        }
    }

    #[test]
    fn response_compact_omits_verbose_fields() {
        // At compact + no show_observability, the Python predecessor
        // trimmed `output_adapters` and `resolved_parameters` (cli.py
        // `_trim_explore_response`). Native preserves that contract.
        let response = build_minimal_response(Detail::Compact, false);
        let json = serde_json::to_value(&response).unwrap();
        let obj = json.as_object().unwrap();
        assert!(
            !obj.contains_key("output_adapters"),
            "compact must omit output_adapters; got {:?}",
            obj.keys().collect::<Vec<_>>()
        );
        assert!(
            !obj.contains_key("resolved_parameters"),
            "compact must omit resolved_parameters"
        );
        assert!(
            !obj.contains_key("observability"),
            "compact must omit observability"
        );
    }

    #[test]
    fn response_full_emits_verbose_fields() {
        // At Detail::Full, the verbose envelope (output_adapters +
        // resolved_parameters) lights up.
        let response = build_minimal_response(Detail::Full, false);
        let json = serde_json::to_value(&response).unwrap();
        let obj = json.as_object().unwrap();
        assert!(
            obj.contains_key("output_adapters"),
            "Detail::Full must emit output_adapters"
        );
        assert!(
            obj.contains_key("resolved_parameters"),
            "Detail::Full must emit resolved_parameters"
        );
        assert!(
            obj.contains_key("observability"),
            "Detail::Full must emit observability"
        );
    }

    #[test]
    fn response_show_observability_overrides_detail() {
        // `--show-observability` forces verbose shaping even at compact.
        let response = build_minimal_response(Detail::Compact, true);
        let json = serde_json::to_value(&response).unwrap();
        let obj = json.as_object().unwrap();
        assert!(
            obj.contains_key("output_adapters"),
            "show_observability=true must emit output_adapters"
        );
        assert!(
            obj.contains_key("resolved_parameters"),
            "show_observability=true must emit resolved_parameters"
        );
        assert!(
            obj.contains_key("observability"),
            "show_observability=true must emit observability"
        );
    }

    #[test]
    fn response_top_level_types_are_stable() {
        // Pin the type of each top-level field. A future refactor that
        // accidentally changed `answer` from array to object (for
        // example) would fail loudly here, before any consumer broke.
        let response = build_minimal_response(Detail::Compact, false);
        let json = serde_json::to_value(&response).unwrap();
        let obj = json.as_object().unwrap();

        let expectations: &[(&str, &str)] = &[
            ("schema_version", "string"),
            ("mode", "string"),
            ("intent", "string"),
            ("intent_source", "string"),
            ("status", "string"),
            ("request", "object"),
            ("answer", "array"),
            ("navigation_hints", "array"),
            ("excluded", "array"),
            ("ambiguous", "array"),
            ("evidence", "object"),
            ("confidence", "object"),
            ("safe_to_use_as_answer", "boolean"),
            ("safe_to_use_as_navigation", "boolean"),
            ("trust_policy", "object"),
            ("degraded_reasons", "array"),
            ("verification_steps", "array"),
            ("next_actions", "array"),
            ("available_specialized_intents", "array"),
        ];

        for (key, expected_type) in expectations {
            let value = obj
                .get(*key)
                .unwrap_or_else(|| panic!("missing required key {key} in response"));
            let actual_type = match value {
                serde_json::Value::Null => "null",
                serde_json::Value::Bool(_) => "boolean",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::String(_) => "string",
                serde_json::Value::Array(_) => "array",
                serde_json::Value::Object(_) => "object",
            };
            assert_eq!(
                actual_type, *expected_type,
                "field {key:?} expected type {expected_type:?}, got {actual_type:?}"
            );
        }
    }

    #[test]
    fn trust_policy_inner_shape_is_stable() {
        let response = build_minimal_response(Detail::Compact, false);
        let json = serde_json::to_value(&response).unwrap();
        let trust = json
            .get("trust_policy")
            .and_then(|v| v.as_object())
            .unwrap();

        // The trust_policy object's keys are read by every downstream
        // consumer that branches on whether to act on `answer[]`.
        for key in &[
            "safe_to_use_as_answer",
            "safe_to_use_as_navigation",
            "evidence_level",
            "authoritative_answer_count",
            "navigation_hint_count",
            "degraded",
            "trust_policy",
            "reason",
        ] {
            assert!(trust.contains_key(*key), "trust_policy missing key: {key}");
        }

        // The `trust_policy` enum value is one of the documented values
        // (mirrors Python's `_intent_catalog` declaration in cli.py:1571).
        let policy = trust.get("trust_policy").and_then(|v| v.as_str()).unwrap();
        let allowed = [
            "answer_candidate",
            "needs_verification",
            "navigation_only",
            "failed",
        ];
        assert!(
            allowed.contains(&policy),
            "trust_policy enum value {policy:?} not in documented set {allowed:?}"
        );
    }

    #[test]
    fn evidence_inner_shape_is_stable() {
        let response = build_minimal_response(Detail::Compact, false);
        let json = serde_json::to_value(&response).unwrap();
        let evidence = json.get("evidence").and_then(|v| v.as_object()).unwrap();

        for key in &["answer_count", "navigation_hint_count", "excluded_count"] {
            let v = evidence
                .get(*key)
                .and_then(|x| x.as_u64())
                .unwrap_or_else(|| panic!("evidence.{key} should be a non-negative integer"));
            // Sanity: counts are bounded and non-negative.
            assert!(v < 10_000, "evidence.{key} unrealistic: {v}");
        }
    }
}
