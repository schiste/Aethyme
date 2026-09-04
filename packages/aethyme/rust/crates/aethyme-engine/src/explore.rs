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
//! Observability reports both redb freshness and bounded Surface/Flow coverage:
//! a fresh store can still be `partial` when source paths suggest an ingress,
//! proxy, middleware, credential, or live-test surface that committed graph
//! fragments/index shards do not expose.
//!
//! Wire shape
//! ----------
//! Output JSON matches `aethyme-explore-v1` schema produced by the Python
//! `_explore_task_localization_query` at compact detail. A consumer that
//! reads `answer[]` + `safe_to_use_as_answer` + `trust_policy` works
//! identically against either implementation.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::graph::navigation::{task_anchors_view_redb, task_next_view_redb, task_scope_view_redb};
use crate::graph::search::{SearchHit, symbol_search_redb};
use crate::model::edge::EdgeKind;
use crate::model::task::TaskInput;
#[cfg(test)]
use crate::store::redb::graph_store::SymbolMatchSignals;
use crate::store::redb::graph_store::{
    GraphStore, GraphStoreError, NodeDisplay, ReadOnlyGraphStore, StoredNodeKind,
    SubsystemCandidate, SurfaceFlowCandidate, SurfacePathCandidate,
};

const SURFACE_FLOW_COVERAGE_SCHEMA_VERSION: u8 = 1;
const SURFACE_FLOW_MAX_FRAGMENT_BYTES: u64 = 8_192;
const SURFACE_FLOW_MAX_PATH_HINTS: usize = 8;
const SURFACE_FLOW_MAX_SCANNED_PATHS: usize = 20_000;
const AGENT_OUTPUT_MAX_ANSWER_ITEMS: usize = 4;
const AGENT_OUTPUT_MAX_NAVIGATION_HINTS: usize = 0;
const AGENT_OUTPUT_MAX_SUBSYSTEMS: usize = 3;
const AGENT_OUTPUT_MAX_SUBSYSTEM_PATHS: usize = 2;
const AGENT_OUTPUT_MAX_SUBSYSTEM_TARGETS: usize = 2;
const AGENT_OUTPUT_MAX_SUBSYSTEM_SIGNALS: usize = 3;
const AGENT_OUTPUT_MAX_TARGET_REASON_CHARS: usize = 140;
const AGENT_OUTPUT_MAX_WARNINGS: usize = 3;
const AGENT_OUTPUT_MAX_DEGRADED_REASONS: usize = 6;
const AGENT_OUTPUT_MAX_NEXT_ACTIONS: usize = 3;
const AGENT_OUTPUT_MAX_VERIFICATION_STEPS: usize = 2;
const AGENT_OUTPUT_MAX_EVIDENCE_ARRAY_ITEMS: usize = 4;
const AGENT_OUTPUT_MAX_RANKING_SIGNALS: usize = 4;
const AGENT_OUTPUT_MAX_MATCHED_TERMS: usize = 6;
const AGENT_OUTPUT_MAX_MATCHED_QUERIES: usize = 4;
const AGENT_OUTPUT_MAX_SYMBOLS: usize = 3;
const AGENT_OUTPUT_MAX_LINE_REFS: usize = 2;
const AGENT_OUTPUT_MAX_AMBIGUITY_ARRAY_ITEMS: usize = 4;
const AGENT_OUTPUT_MAX_MISSING_SURFACES: usize = 8;
const SURFACE_FLOW_IGNORED_DIRS: &[&str] = &[
    ".aethyme",
    ".git",
    ".hg",
    ".svn",
    ".tox",
    ".venv",
    "__pycache__",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "target",
    "vendor",
];

const BACKEND_PATTERNS: &[&str] = &[
    "backend/",
    "backend.",
    "server/",
    "server.",
    "api/",
    "api.",
    "services/",
    "services.",
];
const EDGE_PROXY_PATTERNS: &[&str] = &[
    "gcp-run-proxy/",
    "cloudflare",
    "edge/",
    "functions/_middleware",
    "middleware.ts",
    "middleware.js",
    "netlify/functions",
    "proxy/",
    "proxy.",
    "vercel.json",
    "worker.",
    "workers/",
];
const ROUTE_PATTERNS: &[&str] = &[
    "api_views.",
    "app.py",
    "controller",
    "routes/",
    "routes.",
    "router",
    "urls.py",
    "views.py",
];
const MIDDLEWARE_PATTERNS: &[&str] = &["middleware", "interceptor", "filter"];
const WEBHOOK_PATTERNS: &[&str] = &["webhook", "hooks/"];
const CLI_PATTERNS: &[&str] = &["/bin/", "cli.", "cmd/", "command", "manage.py", "scripts/"];
const JOB_QUEUE_PATTERNS: &[&str] = &[
    "celery",
    "cron",
    "job",
    "queue",
    "scheduler",
    "tasks.py",
    "worker.",
];
const CREDENTIAL_PATTERNS: &[&str] = &[
    "api_key",
    "auth",
    "credential",
    "jwt",
    "oauth",
    "oidc",
    "permission",
    "rbac",
    "token",
];
const LIVE_TEST_PATTERNS: &[&str] = &[
    ".spec.",
    ".test.",
    "/test/",
    "/tests/",
    "e2e/",
    "fixture",
    "integration",
];
const BACKEND_SEMANTIC_TERMS: &[&str] = &[];
const EDGE_PROXY_SEMANTIC_TERMS: &[&str] = &["worker_surface", "proxy_surface", "forwards_to"];
const ROUTE_SEMANTIC_TERMS: &[&str] = &["route_surface"];
const MIDDLEWARE_SEMANTIC_TERMS: &[&str] = &["middleware_installation", "installs_middleware"];
const WEBHOOK_SEMANTIC_TERMS: &[&str] = &["webhook_surface"];
const CLI_SEMANTIC_TERMS: &[&str] = &["cli_surface"];
const JOB_QUEUE_SEMANTIC_TERMS: &[&str] = &["job_surface", "queue_surface"];
const CREDENTIAL_SEMANTIC_TERMS: &[&str] = &[
    "credential_operation",
    "validates_credential",
    "authorizes",
    "issues_credential",
    "rewrites_header",
    "stores_credential",
    "uses_credential",
];
const LIVE_TEST_SEMANTIC_TERMS: &[&str] = &["behavior_test_surface", "tested_by"];
const SURFACE_FLOW_SEMANTIC_TERMS: &[&str] = &[
    "route_surface",
    "worker_surface",
    "proxy_surface",
    "webhook_surface",
    "cli_surface",
    "job_surface",
    "queue_surface",
    "middleware_installation",
    "credential_operation",
    "behavior_test_surface",
    "forwards_to",
    "installs_middleware",
    "validates_credential",
    "authorizes",
    "issues_credential",
    "rewrites_header",
    "stores_credential",
    "tested_by",
    "uses_credential",
];

#[derive(Clone, Copy)]
struct SurfaceCoverageSpec {
    key: &'static str,
    label: &'static str,
    patterns: &'static [&'static str],
    semantic_terms: &'static [&'static str],
}

const SURFACE_COVERAGE_SPECS: &[SurfaceCoverageSpec] = &[
    SurfaceCoverageSpec {
        key: "backend",
        label: "backend service/application code",
        patterns: BACKEND_PATTERNS,
        semantic_terms: BACKEND_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "edge_proxy",
        label: "edge worker/proxy/gateway ingress",
        patterns: EDGE_PROXY_PATTERNS,
        semantic_terms: EDGE_PROXY_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "route",
        label: "HTTP route/controller/view surface",
        patterns: ROUTE_PATTERNS,
        semantic_terms: ROUTE_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "middleware",
        label: "middleware/filter/interceptor installation",
        patterns: MIDDLEWARE_PATTERNS,
        semantic_terms: MIDDLEWARE_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "webhook",
        label: "webhook ingress surface",
        patterns: WEBHOOK_PATTERNS,
        semantic_terms: WEBHOOK_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "cli",
        label: "CLI/command entrypoint surface",
        patterns: CLI_PATTERNS,
        semantic_terms: CLI_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "job_queue",
        label: "job/queue/cron/worker surface",
        patterns: JOB_QUEUE_PATTERNS,
        semantic_terms: JOB_QUEUE_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "credential",
        label: "credential issue/store/use/validation surface",
        patterns: CREDENTIAL_PATTERNS,
        semantic_terms: CREDENTIAL_SEMANTIC_TERMS,
    },
    SurfaceCoverageSpec {
        key: "live_behavior_test",
        label: "integration/e2e/spec test surface",
        patterns: LIVE_TEST_PATTERNS,
        semantic_terms: LIVE_TEST_SEMANTIC_TERMS,
    },
];

#[derive(Debug)]
struct SurfaceSemanticHit {
    path: String,
    terms: Vec<&'static str>,
}

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
    pub subsystems: Vec<ExploreSubsystem>,
    pub evidence: Evidence,
    pub confidence: Confidence,
    pub safe_to_use_as_answer: bool,
    pub safe_to_use_as_navigation: bool,
    pub trust_policy: TrustPolicy,
    pub degraded_reasons: Vec<String>,
    pub verification_steps: Vec<serde_json::Value>,
    pub next_actions: Vec<String>,
    pub available_specialized_intents: Vec<&'static str>,
    /// Approximate serialized JSON size after output-profile shaping.
    ///
    /// The value is computed from the response itself just before returning
    /// to the CLI. It is intentionally character-oriented rather than
    /// tokenizer-specific so shell callers can compare it directly to
    /// command-output budgets.
    pub output_chars_estimate: usize,
    /// True when the response was capped by the agent-facing output budget.
    ///
    /// The ranking/indexing passes still see the full internal evidence; this
    /// flag only describes what was omitted from the serialized envelope.
    pub truncated: bool,
    /// Downstream-friendly repackaging of the response. Mirrors Python's
    /// `output_adapters.task_localization_json` / `dead_code_eval_json`
    /// at `cli.py:2088-2118` and `cli.py:4700`.
    ///
    /// Gated by `detail==Full`. Agent-mode `--show-observability` emits
    /// compact trust/coverage fields, but adapters are redundant
    /// repackaging that costs tokens on the first Explore call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_adapters: Option<serde_json::Value>,
    /// Echo of the effective `ExploreParams` after intent + detail
    /// widening. Same gate as `output_adapters`: internal tuning knobs
    /// are not actionable by the agent at compact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_parameters: Option<serde_json::Value>,
    /// Runtime read-source status. `--show-observability` emits a compact
    /// agent summary; `--detail full` emits the verbose debugging envelope.
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

#[derive(Debug, Clone, Serialize)]
pub struct ExploreSubsystem {
    pub rank: usize,
    pub id: String,
    pub label: String,
    pub role: String,
    pub confidence: f64,
    pub paths: Vec<String>,
    pub token_subsystems: Vec<&'static str>,
    pub top_verification_targets: Vec<ExploreSubsystemTarget>,
    pub signals: Vec<String>,
    pub missing_coverage_warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExploreSubsystemTarget {
    pub kind: String,
    pub target: String,
    pub path: Option<String>,
    pub reason: String,
    pub confidence: f64,
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
    /// **Status (v1, 2026-07-29):** the response builder enforces
    /// this at the representation layer by capping agent-facing
    /// answer, navigation, subsystem, evidence, and observability
    /// arrays. It is still a soft cap because JSON overhead and
    /// request-specific strings vary, but depth=0 no longer emits
    /// the full debugging observability envelope.
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
    /// When true, emit agent-facing observability. Compact/standard output
    /// includes trust, freshness, Surface/Flow coverage, warnings, and ranking
    /// summaries; `--detail full --show-observability` emits the verbose
    /// debugging envelope.
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
    /// The optional local graph query artifact is unavailable. The CLI
    /// converts this into a successful but unsafe answer-json envelope so
    /// agents can follow explicit enrollment/materialization remediation.
    GraphUnavailable {
        status: &'static str,
        reason: String,
    },
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
            Self::GraphUnavailable { status, reason } => {
                write!(f, "graph {status}: {reason}")
            }
            Self::BadParams(msg) => write!(f, "bad params: {msg}"),
        }
    }
}

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

/// Build the stable answer-json contract for a repository whose optional
/// local graph store cannot currently answer. No source scan is attempted:
/// callers receive an explicit unsafe result and deterministic recovery.
pub fn graph_unavailable_response(
    repo: &Path,
    request: &str,
    intent: &'static str,
    intent_source: &'static str,
    status: &'static str,
    reason: String,
) -> ExploreResponse {
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
        answer: Vec::new(),
        navigation_hints: Vec::new(),
        excluded: Vec::new(),
        ambiguous: Vec::new(),
        subsystems: Vec::new(),
        evidence: Evidence {
            answer_count: 0,
            navigation_hint_count: 0,
            excluded_count: 0,
        },
        confidence: Confidence {
            overall: None,
            answer_summary: ConfidenceSummary::default(),
            excluded_summary: ConfidenceSummary::default(),
            analyzed_summary: serde_json::json!({"graph_available": false}),
        },
        safe_to_use_as_answer: false,
        safe_to_use_as_navigation: false,
        trust_policy: TrustPolicy {
            safe_to_use_as_answer: false,
            safe_to_use_as_navigation: false,
            evidence_level: "none".into(),
            authoritative_answer_count: 0,
            navigation_hint_count: 0,
            degraded: true,
            trust_policy: "verify_before_use",
            reason: "The optional graph query artifact is unavailable; no repository location was inferred."
                .into(),
        },
        degraded_reasons: vec![format!("graph_store_{status}")],
        verification_steps: vec![serde_json::json!({
            "kind": "manual_source_inspection",
            "reason": "Explore returned no graph-backed targets"
        })],
        next_actions: vec![next_action.into()],
        available_specialized_intents: vec![
            "behavior_localization_query",
            "usage_boundary_query",
        ],
        output_chars_estimate: 0,
        truncated: false,
        output_adapters: None,
        resolved_parameters: None,
        observability: Some(serde_json::json!({
            "readiness": {
                "status": "degraded",
                "reason": "optional_graph_unavailable"
            },
            "graph_store": {
                "status": status,
                "source_of_truth": "graph_fragments",
                "derived_query_artifact": "redb_graph_store",
                "reason": reason
            }
        })),
    })
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
    let started = std::time::Instant::now();
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

    let discovery_started = std::time::Instant::now();
    let canonical_repo = repo
        .canonicalize()
        .map_err(|e| ExploreError::EngineAnalyzer(format!("canonicalize repo: {e}")))?;
    let repository_discovery_elapsed_us = discovery_started.elapsed().as_micros();
    let store_started = std::time::Instant::now();
    let store = GraphStore::open_read_only(&canonical_repo).map_err(graph_store_explore_error)?;
    let graph_store_open_elapsed_us = store_started.elapsed().as_micros();
    let observability = graph_store_observability(&canonical_repo);
    let query_started = std::time::Instant::now();

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
    let surface_flow = surface_flow_evidence_redb(&store, request, &symbol_queries, &text_terms);

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

    let query_execution_elapsed_us = query_started.elapsed().as_micros();
    let mut response = build_response_with_surface_flow(
        request,
        intent,
        intent_source,
        &view,
        &symbol_matches,
        &text_items,
        &filename_items,
        &callsite_items,
        &surface_flow,
        params,
        observability,
    );
    if let Some(serde_json::Value::Object(observability)) = response.observability.as_mut() {
        observability.insert(
            "performance".into(),
            serde_json::json!({
                "repository_discovery_elapsed_us": repository_discovery_elapsed_us,
                "graph_store_open_elapsed_us": graph_store_open_elapsed_us,
                "query_execution_elapsed_us": query_execution_elapsed_us,
                "total_elapsed_us": started.elapsed().as_micros(),
                "store_bytes": std::fs::metadata(GraphStore::final_path(&canonical_repo))
                    .ok()
                    .map(|metadata| metadata.len()),
                "peak_memory_bytes": aethyme_graph_storage::peak_memory_bytes(),
            }),
        );
    }
    Ok(response)
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

fn surface_flow_evidence_redb(
    store: &ReadOnlyGraphStore,
    request: &str,
    symbol_queries: &[String],
    text_terms: &[String],
) -> SurfaceFlowExploreEvidence {
    let tokens = surface_flow_query_tokens(request, symbol_queries, text_terms);
    if tokens.is_empty() || !should_query_surface_flow(request, &tokens) {
        return SurfaceFlowExploreEvidence::default();
    }

    let mut evidence = SurfaceFlowExploreEvidence::default();
    if let Ok(entrypoints) = store.entrypoints_for_task(&tokens) {
        evidence.entrypoints = entrypoints;
    }
    if let Ok(surface_paths) = store.surface_paths_for_behavior(&tokens) {
        evidence.surface_paths = surface_paths;
    }
    if let Ok(credential_flows) = store.credential_flow_candidates(&tokens) {
        evidence.credential_flows = credential_flows;
    }
    if let Ok(subsystems) = store.subsystems_matching(&tokens) {
        evidence.subsystems = subsystems;
    }
    if let Ok(coverage) = store.coverage_for_task_class(request) {
        evidence.tests = coverage.tests;
        evidence.coverage_missing = coverage.missing;
    }
    evidence
}

fn surface_flow_query_tokens(
    request: &str,
    symbol_queries: &[String],
    text_terms: &[String],
) -> Vec<String> {
    let mut tokens = Vec::new();
    for token in symbol_queries.iter().chain(text_terms.iter()) {
        push_unique_token(&mut tokens, token);
    }
    if ranking::auth_token_focus_from_request(request) {
        for token in [
            "auth",
            "token",
            "credential",
            "middleware",
            "route",
            "proxy",
        ] {
            push_unique_token(&mut tokens, token);
        }
    }
    tokens.truncate(16);
    tokens
}

fn push_unique_token(tokens: &mut Vec<String>, token: &str) {
    let normalized = token.trim().to_ascii_lowercase();
    if normalized.len() < 3 {
        return;
    }
    if !tokens.iter().any(|existing| existing == &normalized) {
        tokens.push(normalized);
    }
}

fn should_query_surface_flow(request: &str, tokens: &[String]) -> bool {
    if ranking::auth_token_focus_from_request(request) {
        return true;
    }
    tokens.iter().any(|token| {
        contains_any_text(
            token,
            &[
                "auth",
                "credential",
                "entrypoint",
                "middleware",
                "proxy",
                "route",
                "surface",
                "token",
                "webhook",
                "worker",
            ],
        )
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

    let graph_store = serde_json::json!({
        "backend": "redb",
        "status": status,
        "exists": store_path.is_file(),
        "fragments_exist": fragments_path.is_dir(),
        "stale": stale,
        "store_modified_unix": store_modified,
        "newest_fragment_modified_unix": newest_fragment,
    });
    let surface_flow_graph = surface_flow_coverage(repo, &fragments_path);
    let completeness = surface_flow_graph
        .get("coverage")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let indexed_languages = surface_flow_graph
        .get("indexed_languages")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let indexed_frameworks = surface_flow_graph
        .get("indexed_frameworks")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let missing_expected_surfaces = surface_flow_graph
        .get("missing_expected_surfaces")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));

    serde_json::json!({
        "graph_store": graph_store,
        "graph_freshness": {
            "backend": "redb",
            "status": status,
            "fresh": status == "fresh",
            "exists": store_path.is_file(),
            "fragments_exist": fragments_path.is_dir(),
            "stale": stale,
            "store_modified_unix": store_modified,
            "newest_fragment_modified_unix": newest_fragment,
            "source_of_truth": "graph_fragments",
            "derived_query_artifact": "redb_graph_store",
        },
        "surface_flow_graph": surface_flow_graph,
        "graph_completeness_by_surface_type": completeness,
        "indexed_languages": indexed_languages,
        "indexed_frameworks": indexed_frameworks,
        "missing_expected_surfaces": missing_expected_surfaces,
    })
}

fn surface_flow_coverage(repo: &Path, fragments_path: &Path) -> serde_json::Value {
    let source_paths = collect_surface_flow_paths(repo, false);
    let indexed_paths = collect_surface_flow_paths(fragments_path, true);
    let semantic_hits = collect_surface_flow_semantic_hits(fragments_path, &indexed_paths);
    let indexed_languages = indexed_languages_from_graph_paths(&indexed_paths);
    let indexed_frameworks = indexed_frameworks_from_graph_paths(&indexed_paths, &semantic_hits);
    let source_truncated = source_paths.len() >= SURFACE_FLOW_MAX_SCANNED_PATHS;
    let indexed_truncated = indexed_paths.len() >= SURFACE_FLOW_MAX_SCANNED_PATHS;
    let mut coverage = serde_json::Map::new();
    let mut missing_expected_surfaces = Vec::new();
    let mut covered_count = 0usize;
    let mut source_present_count = 0usize;

    for spec in SURFACE_COVERAGE_SPECS {
        let source_matches = matching_paths(&source_paths, spec.patterns);
        let path_indexed_matches = matching_paths(&indexed_paths, spec.patterns);
        let semantic_matches = semantic_path_matches(&semantic_hits, spec.semantic_terms);
        let source_present = !source_matches.is_empty();
        let path_indexed = !path_indexed_matches.is_empty();
        let semantic_required = !spec.semantic_terms.is_empty();
        let semantic_indexed = !semantic_matches.is_empty();
        let indexed = if semantic_required {
            semantic_indexed
        } else {
            path_indexed
        };
        let indexed_matches = if semantic_required {
            &semantic_matches
        } else {
            &path_indexed_matches
        };
        let unindexed_source_matches = if source_present && semantic_required && !semantic_indexed {
            source_matches.clone()
        } else {
            unindexed_source_paths(&source_matches, indexed_matches)
        };
        let source_hints = capped_path_hints(&source_matches);
        let indexed_hints = capped_path_hints(indexed_matches);
        let path_indexed_hints = capped_path_hints(&path_indexed_matches);
        let semantic_hints = capped_path_hints(&semantic_matches);
        let unindexed_source_hints = capped_path_hints(&unindexed_source_matches);
        let status = match (source_present, indexed) {
            (true, true) if unindexed_source_hints.is_empty() => {
                covered_count += 1;
                "covered"
            }
            (true, true) => {
                missing_expected_surfaces.push(serde_json::json!({
                    "surface_type": spec.key,
                    "label": spec.label,
                    "reason": if semantic_required {
                        "some source paths suggest this Surface/Flow family and some semantic graph evidence exists, but other source paths have no matching semantic Surface/Flow node/edge evidence"
                    } else {
                        "some source paths suggest this family, but matching graph fragments/index shards were not found for those source paths"
                    },
                    "source_path_hints": unindexed_source_hints.clone(),
                }));
                "partially_indexed"
            }
            (true, false) => {
                missing_expected_surfaces.push(serde_json::json!({
                    "surface_type": spec.key,
                    "label": spec.label,
                    "reason": if semantic_required && path_indexed {
                        "source paths and path fragments exist for this Surface/Flow family, but explicit semantic Surface/Flow node/edge evidence is missing"
                    } else if semantic_required {
                        "source paths suggest this Surface/Flow family, but graph fragments/index shards do not contain explicit semantic Surface/Flow node/edge evidence"
                    } else {
                        "source paths suggest this family, but graph fragments/index shards do not contain matching paths"
                    },
                    "source_path_hints": source_hints.clone(),
                }));
                "source_present_not_indexed"
            }
            (false, true) => "indexed_without_source_hint",
            (false, false) => "not_detected",
        };
        if source_present {
            source_present_count += 1;
        }
        coverage.insert(
            spec.key.to_string(),
            serde_json::json!({
                "label": spec.label,
                "source_present": source_present,
                "indexed": indexed,
                "path_indexed": path_indexed,
                "semantic_required": semantic_required,
                "semantic_indexed": semantic_indexed,
                "status": status,
                "source_path_hints": source_hints,
                "indexed_path_hints": indexed_hints,
                "path_indexed_hints": path_indexed_hints,
                "semantic_path_hints": semantic_hints,
                "unindexed_source_path_hints": unindexed_source_hints,
            }),
        );
    }

    let status = if !fragments_path.is_dir() {
        "unknown"
    } else if !missing_expected_surfaces.is_empty() {
        "partial"
    } else if covered_count > 0 {
        "covered"
    } else {
        "no_surface_signals"
    };

    serde_json::json!({
        "schema_version": SURFACE_FLOW_COVERAGE_SCHEMA_VERSION,
        "status": status,
        "source_of_truth": "graph_fragments",
        "derived_query_artifact": "redb_graph_store",
        "source_path_count_scanned": source_paths.len(),
        "indexed_path_count_scanned": indexed_paths.len(),
        "semantic_fragment_hit_count": semantic_hits.len(),
        "source_scan_truncated": source_truncated,
        "indexed_scan_truncated": indexed_truncated,
        "indexed_languages": indexed_languages,
        "indexed_frameworks": indexed_frameworks,
        "surface_type_count": SURFACE_COVERAGE_SPECS.len(),
        "source_present_surface_count": source_present_count,
        "covered_surface_count": covered_count,
        "coverage": coverage,
        "missing_expected_surfaces": missing_expected_surfaces,
    })
}

fn collect_surface_flow_paths(root: &Path, include_hidden: bool) -> Vec<String> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        if out.len() >= SURFACE_FLOW_MAX_SCANNED_PATHS {
            break;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if include_hidden || !SURFACE_FLOW_IGNORED_DIRS.contains(&file_name.as_str()) {
                    stack.push(path);
                }
                continue;
            }
            if let Ok(relative) = path.strip_prefix(root) {
                out.push(normalized_path(relative));
            }
            if out.len() >= SURFACE_FLOW_MAX_SCANNED_PATHS {
                break;
            }
        }
    }
    out.sort();
    out
}

fn collect_surface_flow_semantic_hits(
    root: &Path,
    relative_paths: &[String],
) -> Vec<SurfaceSemanticHit> {
    let mut hits = Vec::new();
    if !root.is_dir() {
        return hits;
    }
    for relative in relative_paths {
        let path = root.join(&relative);
        let Some(bytes) = read_file_prefix(&path, SURFACE_FLOW_MAX_FRAGMENT_BYTES) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        let terms: Vec<&'static str> = SURFACE_FLOW_SEMANTIC_TERMS
            .iter()
            .copied()
            .filter(|term| text.contains(term))
            .collect();
        if !terms.is_empty() {
            hits.push(SurfaceSemanticHit {
                path: relative.clone(),
                terms,
            });
        }
    }
    hits.sort_by(|a, b| a.path.cmp(&b.path));
    hits
}

fn read_file_prefix(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut limited = file.by_ref().take(max_bytes);
    let mut bytes = Vec::new();
    limited.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn semantic_path_matches(
    hits: &[SurfaceSemanticHit],
    semantic_terms: &[&'static str],
) -> Vec<String> {
    if semantic_terms.is_empty() {
        return Vec::new();
    }
    let mut matches: Vec<String> = hits
        .iter()
        .filter(|hit| hit.terms.iter().any(|term| semantic_terms.contains(term)))
        .map(|hit| hit.path.clone())
        .collect();
    matches.sort_by(
        |a, b| match surface_hint_priority(a).cmp(&surface_hint_priority(b)) {
            std::cmp::Ordering::Equal => a.cmp(b),
            other => other,
        },
    );
    matches
}

fn matching_paths(paths: &[String], patterns: &[&str]) -> Vec<String> {
    let mut matches: Vec<String> = paths
        .iter()
        .filter(|path| {
            let match_text = surface_path_match_text(path);
            patterns.iter().any(|pattern| match_text.contains(pattern))
        })
        .cloned()
        .collect();
    matches.sort_by(
        |a, b| match surface_hint_priority(a).cmp(&surface_hint_priority(b)) {
            std::cmp::Ordering::Equal => a.cmp(b),
            other => other,
        },
    );
    matches
}

fn capped_path_hints(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .take(SURFACE_FLOW_MAX_PATH_HINTS)
        .cloned()
        .collect()
}

fn surface_path_match_text(path: &str) -> String {
    let lowered = path.to_ascii_lowercase();
    let logical = surface_logical_path(path);
    if logical != lowered {
        return format!("{lowered}\n{logical}");
    }
    lowered
}

fn unindexed_source_paths(source_paths: &[String], indexed_paths: &[String]) -> Vec<String> {
    let indexed_keys: std::collections::BTreeSet<String> = indexed_paths
        .iter()
        .flat_map(|path| surface_path_coverage_keys(path))
        .collect();
    source_paths
        .iter()
        .filter(|path| {
            let source_keys = surface_path_coverage_keys(path);
            !source_keys.iter().any(|key| indexed_keys.contains(key))
        })
        .cloned()
        .collect()
}

fn surface_path_coverage_keys(path: &str) -> Vec<String> {
    let logical = surface_logical_path(path);
    let mut keys = vec![logical.clone()];
    if let Some(stem) = strip_known_source_extension(&logical) {
        keys.push(stem);
    }
    keys.sort();
    keys.dedup();
    keys
}

fn surface_logical_path(path: &str) -> String {
    let lowered = path.to_ascii_lowercase();
    if let Some(indexed_module) = lowered.strip_prefix("_index/") {
        indexed_module.trim_end_matches(".ndjson").replace('.', "/")
    } else {
        lowered.trim_end_matches(".bin").to_string()
    }
}

fn strip_known_source_extension(path: &str) -> Option<String> {
    const SOURCE_EXTENSIONS: &[&str] = &[
        ".cs", ".go", ".java", ".js", ".jsx", ".mjs", ".php", ".py", ".rb", ".rs", ".swift", ".ts",
        ".tsx",
    ];
    SOURCE_EXTENSIONS
        .iter()
        .find_map(|extension| path.strip_suffix(extension).map(str::to_string))
}

fn indexed_languages_from_graph_paths(paths: &[String]) -> Vec<String> {
    let mut languages = std::collections::BTreeSet::new();
    for path in paths {
        let logical = surface_logical_path(path);
        let extension = Path::new(&logical)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("");
        if let Some(language) = language_for_extension(extension) {
            languages.insert(language);
        }
    }
    languages.into_iter().map(str::to_string).collect()
}

fn language_for_extension(extension: &str) -> Option<&'static str> {
    match extension {
        "cs" => Some("csharp"),
        "go" => Some("go"),
        "java" => Some("java"),
        "js" | "jsx" | "mjs" => Some("javascript"),
        "php" => Some("php"),
        "py" => Some("python"),
        "rb" => Some("ruby"),
        "rs" => Some("rust"),
        "swift" => Some("swift"),
        "ts" | "tsx" => Some("typescript"),
        _ => None,
    }
}

fn indexed_frameworks_from_graph_paths(
    paths: &[String],
    semantic_hits: &[SurfaceSemanticHit],
) -> Vec<String> {
    let mut match_text = paths
        .iter()
        .map(|path| surface_path_match_text(path))
        .collect::<Vec<_>>()
        .join("\n");
    for hit in semantic_hits {
        match_text.push('\n');
        match_text.push_str(&surface_path_match_text(&hit.path));
        match_text.push('\n');
        match_text.push_str(&hit.terms.join("\n"));
    }

    let mut frameworks = std::collections::BTreeSet::new();
    if contains_any_text(
        &match_text,
        &["manage.py", "settings.py", "urls.py", "django"],
    ) {
        frameworks.insert("django");
    }
    if contains_any_text(
        &match_text,
        &[
            "rest_framework",
            "viewset",
            "serializers.py",
            "api_views.py",
        ],
    ) {
        frameworks.insert("django-rest-framework");
    }
    if contains_any_text(&match_text, &["fastapi", "apirouter"]) {
        frameworks.insert("fastapi");
    }
    if contains_any_text(&match_text, &["flask", "blueprint"]) {
        frameworks.insert("flask");
    }
    if contains_any_text(&match_text, &["next.config", "app/api/", "pages/api/"]) {
        frameworks.insert("nextjs");
    }
    if contains_any_text(
        &match_text,
        &["functions/_middleware", "middleware.ts", "middleware.js"],
    ) {
        frameworks.insert("edge-middleware");
    }
    if contains_any_text(
        &match_text,
        &[
            "cloudflare",
            "wrangler",
            "worker.js",
            "worker.mjs",
            "worker.ts",
        ],
    ) {
        frameworks.insert("cloudflare-workers");
    }
    if contains_any_text(&match_text, &["gcp-run-proxy", "proxy_surface"]) {
        frameworks.insert("edge-proxy");
    }
    if contains_any_text(&match_text, &["vercel.json", "vercel/"]) {
        frameworks.insert("vercel");
    }
    if contains_any_text(&match_text, &["netlify.toml", "netlify/functions"]) {
        frameworks.insert("netlify");
    }
    if contains_any_text(&match_text, &["config/routes.rb", "rails"]) {
        frameworks.insert("rails");
    }
    if contains_any_text(&match_text, &["axum", "actix_web", "rocket"]) {
        frameworks.insert("rust-web");
    }
    frameworks.into_iter().map(str::to_string).collect()
}

fn surface_hint_priority(path: &str) -> u8 {
    let logical = surface_logical_path(path);
    let hidden = logical.split('/').any(|part| part.starts_with('.'));
    let implementation_file = strip_known_source_extension(&logical).is_some();
    match (hidden, implementation_file) {
        (false, true) => 0,
        (true, true) => 1,
        (false, false) => 2,
        (true, false) => 3,
    }
}

fn normalized_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
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

#[derive(Debug, Default)]
struct SurfaceFlowExploreEvidence {
    entrypoints: Vec<SurfaceFlowCandidate>,
    surface_paths: Vec<SurfacePathCandidate>,
    credential_flows: Vec<SurfaceFlowCandidate>,
    subsystems: Vec<SubsystemCandidate>,
    tests: Vec<NodeDisplay>,
    coverage_missing: Vec<String>,
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
    if lower.contains("webhook")
        && contains_any_text(lower, &["token", "secret", "signature", "hmac"])
    {
        matches.push(TokenSubsystemMatch {
            id: "webhook_tokens",
            label: "webhook tokens",
            base_score: 75,
            signal: "webhook_token_surface",
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

fn request_names_provider_or_secondary_token_subsystem(request: &str) -> bool {
    let lower = request.to_ascii_lowercase();
    token_subsystem_matches(&lower).iter().any(|matched| {
        matches!(
            matched.id,
            "oidc"
                | "audit_jws"
                | "auth0_management"
                | "webhook_tokens"
                | "profile_integrity"
                | "domain_verification"
        )
    })
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

#[derive(Debug)]
struct ExploreSubsystemBuilder {
    id: &'static str,
    label: &'static str,
    role: &'static str,
    priority: u8,
    score: i32,
    paths: std::collections::BTreeSet<String>,
    targets: Vec<ExploreSubsystemTarget>,
    target_keys: std::collections::BTreeSet<String>,
    signals: std::collections::BTreeSet<String>,
    warnings: std::collections::BTreeSet<String>,
}

impl ExploreSubsystemBuilder {
    fn new(id: &'static str) -> Self {
        let (label, role, priority) = explore_subsystem_identity(id);
        Self {
            id,
            label,
            role,
            priority,
            score: 0,
            paths: std::collections::BTreeSet::new(),
            targets: Vec::new(),
            target_keys: std::collections::BTreeSet::new(),
            signals: std::collections::BTreeSet::new(),
            warnings: std::collections::BTreeSet::new(),
        }
    }

    fn add_path(&mut self, path: impl Into<String>) {
        let path = path.into();
        if !path.trim().is_empty() {
            self.paths.insert(path);
        }
    }

    fn add_signal(&mut self, signal: impl Into<String>) {
        let signal = signal.into();
        if !signal.trim().is_empty() {
            self.signals.insert(signal);
        }
    }

    fn add_warning(&mut self, warning: impl Into<String>) {
        let warning = warning.into();
        if !warning.trim().is_empty() {
            self.warnings.insert(warning);
        }
    }

    fn add_target(&mut self, target: ExploreSubsystemTarget) {
        let key = format!(
            "{}\u{1f}{}\u{1f}{}",
            target.kind,
            target.target,
            target.path.as_deref().unwrap_or("")
        );
        if !self.target_keys.insert(key) {
            return;
        }
        self.targets.push(target);
    }
}

fn explore_subsystem_identity(id: &'static str) -> (&'static str, &'static str, u8) {
    match id {
        "ingress_proxy" => ("ingress/proxy", "ingress_proxy", 0),
        "backend_validator" => ("backend API-key validator", "backend_validator", 1),
        "provider_oidc_audit" => (
            "provider/OIDC/audit/webhook",
            "provider_or_secondary_token",
            2,
        ),
        _ => ("related subsystem", "related", 9),
    }
}

fn subsystem_target_search_text(target: &ExploreSubsystemTarget) -> String {
    format!(
        "{} {} {} {}",
        target.kind,
        target.target,
        target.path.as_deref().unwrap_or(""),
        target.reason
    )
    .to_ascii_lowercase()
}

fn target_path_component_count(target: &ExploreSubsystemTarget) -> usize {
    target
        .path
        .as_deref()
        .unwrap_or(target.target.as_str())
        .trim_matches('/')
        .split('/')
        .filter(|component| !component.is_empty())
        .count()
}

fn broad_subsystem_path_penalty(target: &ExploreSubsystemTarget) -> i32 {
    if target.kind != "subsystem_path" {
        return 0;
    }
    let lower = target
        .path
        .as_deref()
        .unwrap_or(target.target.as_str())
        .to_ascii_lowercase();
    if target_path_component_count(target) <= 1
        || matches!(
            lower.as_str(),
            "api" | "backend" | "server" | "services" | "src"
        )
    {
        -220
    } else {
        0
    }
}

fn subsystem_target_priority(subsystem_id: &str, target: &ExploreSubsystemTarget) -> i32 {
    let text = subsystem_target_search_text(target);
    let mut priority = match target.kind.as_str() {
        "entrypoint" => 320,
        "credential_flow" => 300,
        "surface_path" => 240,
        "token_subsystem" => 160,
        "behavior_test" => 80,
        "subsystem_path" => 60,
        _ => 100,
    } + broad_subsystem_path_penalty(target);

    match subsystem_id {
        "ingress_proxy" => {
            if text.contains("gcp-run-proxy") {
                priority += 520;
            }
            if path_looks_ingress_proxy(&text) {
                priority += 220;
            }
            if contains_any_text(
                &text,
                &[
                    "forwards_to",
                    "rewrites_header",
                    "entrypoint_for",
                    "exposes",
                ],
            ) {
                priority += 140;
            }
            if target.kind == "behavior_test" {
                priority -= 80;
            }
        }
        "backend_validator" => {
            if contains_any_text(&text, &["backend/api_keys", "backend.api_keys"]) {
                priority += 520;
            }
            if contains_any_text(
                &text,
                &[
                    "validates_credential",
                    "authorizes",
                    "publishablekey",
                    "validate_publishable_key",
                    "authenticate_api_key",
                ],
            ) {
                priority += 180;
            }
            if target.kind == "behavior_test" {
                priority -= 70;
            }
        }
        "provider_oidc_audit" => {
            if !token_subsystem_matches(&text).is_empty() {
                priority += 160;
            }
            if contains_any_text(
                &text,
                &[
                    "oidc",
                    "audit_jws",
                    "auth0_management",
                    "webhook_token",
                    "profile_integrity",
                    "domain_verification",
                ],
            ) {
                priority += 120;
            }
        }
        _ => {}
    }

    priority
}

fn text_has_edge_proxy_evidence(text: &str) -> bool {
    path_looks_ingress_proxy(text)
        || contains_any_text(
            text,
            &[
                "proxy_surface",
                "worker_surface",
                "forwards_to",
                "rewrites_header",
            ],
        )
}

fn ingress_lane_lacks_edge_proxy_evidence(builder: &ExploreSubsystemBuilder) -> bool {
    builder.id == "ingress_proxy"
        && !builder
            .paths
            .iter()
            .any(|path| text_has_edge_proxy_evidence(&path.to_ascii_lowercase()))
        && !builder.signals.iter().any(|signal| {
            matches!(
                signal.as_str(),
                "forwards_to" | "rewrites_header" | "proxy_surface" | "worker_surface"
            )
        })
        && !builder
            .targets
            .iter()
            .any(|target| text_has_edge_proxy_evidence(&subsystem_target_search_text(target)))
}

fn explore_subsystem_rankings(
    request: &str,
    token_subsystems: &[TokenSubsystemSummary],
    surface_flow: &SurfaceFlowExploreEvidence,
) -> Vec<ExploreSubsystem> {
    let surface_focused = ranking::auth_token_focus_from_request(request)
        || !surface_flow.entrypoints.is_empty()
        || !surface_flow.surface_paths.is_empty()
        || !surface_flow.credential_flows.is_empty();
    if !surface_focused || request_names_provider_or_secondary_token_subsystem(request) {
        return Vec::new();
    }

    let mut builders: std::collections::BTreeMap<&'static str, ExploreSubsystemBuilder> =
        std::collections::BTreeMap::new();

    for summary in token_subsystems {
        let subsystem_id = match summary.id {
            "api_keys" if !summary_paths_are_only_ingress_proxy(summary) => "backend_validator",
            "api_keys" => continue,
            "oidc"
            | "audit_jws"
            | "auth0_management"
            | "webhook_tokens"
            | "profile_integrity"
            | "domain_verification" => "provider_oidc_audit",
            _ => continue,
        };
        let builder = builders
            .entry(subsystem_id)
            .or_insert_with(|| ExploreSubsystemBuilder::new(subsystem_id));
        builder.score += summary.score.max(1);
        builder.add_signal(format!("token_subsystem:{}", summary.id));
        for signal in &summary.signals {
            builder.add_signal(*signal);
        }
        for path in &summary.paths {
            builder.add_path(path.clone());
        }
        builder.add_target(ExploreSubsystemTarget {
            kind: "token_subsystem".into(),
            target: summary.label.to_string(),
            path: summary.paths.iter().next().cloned(),
            reason: format!(
                "Matched token/auth subsystem signals: {}",
                summary
                    .signals
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            confidence: confidence_from_subsystem_score(summary.score),
        });
    }

    for candidate in &surface_flow.entrypoints {
        add_surface_flow_candidate_subsystems(
            &mut builders,
            candidate,
            "entrypoint",
            190,
            "Surface/Flow graph matched an ingress candidate for this task.",
        );
    }
    for path in &surface_flow.surface_paths {
        add_surface_path_subsystems(&mut builders, path);
    }
    for candidate in &surface_flow.credential_flows {
        add_surface_flow_candidate_subsystems(
            &mut builders,
            candidate,
            "credential_flow",
            180,
            "Surface/Flow graph matched credential issue/store/use/validation behavior.",
        );
    }
    for subsystem in &surface_flow.subsystems {
        add_redb_subsystem_candidate(&mut builders, subsystem);
    }
    for test in &surface_flow.tests {
        add_behavior_test_target(&mut builders, test);
    }
    apply_surface_flow_missing_warnings(&mut builders, &surface_flow.coverage_missing);

    let mut ranked: Vec<ExploreSubsystemBuilder> = builders.into_values().collect();
    ranked.retain(|builder| !builder.paths.is_empty() || !builder.targets.is_empty());
    ranked.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| right.score.cmp(&left.score))
            .then_with(|| left.label.cmp(right.label))
    });
    ranked
        .into_iter()
        .enumerate()
        .map(|(index, mut builder)| {
            let subsystem_id = builder.id;
            let route_only_ingress = ingress_lane_lacks_edge_proxy_evidence(&builder);
            if route_only_ingress {
                builder.add_warning(
                    "No indexed edge/proxy surface matched this task; ingress evidence is backend route-only.",
                );
            }
            builder.targets.sort_by(|left, right| {
                subsystem_target_priority(subsystem_id, right)
                    .cmp(&subsystem_target_priority(subsystem_id, left))
                    .then_with(|| {
                        right
                            .confidence
                            .partial_cmp(&left.confidence)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .then_with(|| left.target.cmp(&right.target))
                    .then_with(|| left.path.cmp(&right.path))
            });
            let top_verification_targets = builder.targets.into_iter().take(3).collect::<Vec<_>>();
            let signals = builder.signals.iter().take(10).cloned().collect::<Vec<_>>();
            let token_subsystems =
                token_subsystem_labels_from_signals_targets(&signals, &top_verification_targets);
            let mut confidence = confidence_from_subsystem_score(builder.score);
            if route_only_ingress {
                confidence = confidence.min(0.68);
            }
            ExploreSubsystem {
                rank: index + 1,
                id: builder.id.to_string(),
                label: builder.label.to_string(),
                role: builder.role.to_string(),
                confidence,
                paths: builder.paths.iter().take(6).cloned().collect(),
                token_subsystems,
                top_verification_targets,
                signals,
                missing_coverage_warnings: builder.warnings.iter().take(4).cloned().collect(),
            }
        })
        .take(4)
        .collect()
}

fn summary_paths_are_only_ingress_proxy(summary: &TokenSubsystemSummary) -> bool {
    !summary.paths.is_empty()
        && summary
            .paths
            .iter()
            .all(|path| path_looks_ingress_proxy(path))
}

fn path_looks_ingress_proxy(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    contains_any_text(
        &lower,
        &[
            "gcp-run-proxy",
            "proxy/",
            "proxy.",
            "worker.",
            "workers/",
            "edge/",
            "gateway/",
        ],
    )
}

fn add_surface_flow_candidate_subsystems(
    builders: &mut std::collections::BTreeMap<&'static str, ExploreSubsystemBuilder>,
    candidate: &SurfaceFlowCandidate,
    target_kind: &str,
    base_score: i32,
    reason: &str,
) {
    let text = surface_candidate_text(
        candidate.node.kind,
        candidate.node.id.as_str(),
        candidate.node.display.as_str(),
        candidate.node.name.as_str(),
        candidate.node.path.as_deref(),
        &candidate.relation_kinds,
        &[],
    );
    for subsystem_id in
        classify_surface_subsystems(candidate.node.kind, &text, &candidate.relation_kinds)
    {
        let builder = builders
            .entry(subsystem_id)
            .or_insert_with(|| ExploreSubsystemBuilder::new(subsystem_id));
        builder.score += base_score + candidate.rank.max(0);
        if let Some(path) = candidate.node.path.as_deref() {
            builder.add_path(path);
        }
        for relation in &candidate.relation_kinds {
            builder.add_signal(edge_kind_label_for_explore(relation));
        }
        for token in &candidate.matched_tokens {
            builder.add_signal(format!("matched_token:{token}"));
        }
        for matched in token_subsystem_matches(&text) {
            builder.add_signal(matched.signal);
        }
        builder.add_target(ExploreSubsystemTarget {
            kind: target_kind.to_string(),
            target: node_target_label(&candidate.node),
            path: candidate.node.path.clone(),
            reason: relation_reason(reason, &candidate.relation_kinds),
            confidence: confidence_from_subsystem_score(base_score + candidate.rank.max(0)),
        });
    }
}

fn add_surface_path_subsystems(
    builders: &mut std::collections::BTreeMap<&'static str, ExploreSubsystemBuilder>,
    path: &SurfacePathCandidate,
) {
    let node_kind = path
        .surfaces
        .first()
        .map(|node| node.kind)
        .unwrap_or(StoredNodeKind::File);
    let text = surface_candidate_text(
        node_kind,
        path.path.as_str(),
        path.path.as_str(),
        path.path.as_str(),
        Some(path.path.as_str()),
        &path.relation_kinds,
        &[],
    );
    for subsystem_id in classify_surface_subsystems(node_kind, &text, &path.relation_kinds) {
        let builder = builders
            .entry(subsystem_id)
            .or_insert_with(|| ExploreSubsystemBuilder::new(subsystem_id));
        builder.score += 120 + path.rank.max(0);
        builder.add_path(path.path.clone());
        for relation in &path.relation_kinds {
            builder.add_signal(edge_kind_label_for_explore(relation));
        }
        for token in &path.matched_tokens {
            builder.add_signal(format!("matched_token:{token}"));
        }
        for matched in token_subsystem_matches(&text) {
            builder.add_signal(matched.signal);
        }
        builder.add_target(ExploreSubsystemTarget {
            kind: "surface_path".into(),
            target: path.path.clone(),
            path: Some(path.path.clone()),
            reason: relation_reason(
                "Surface/Flow graph matched behavior on this repo path.",
                &path.relation_kinds,
            ),
            confidence: confidence_from_subsystem_score(120 + path.rank.max(0)),
        });
    }
}

fn add_redb_subsystem_candidate(
    builders: &mut std::collections::BTreeMap<&'static str, ExploreSubsystemBuilder>,
    subsystem: &SubsystemCandidate,
) {
    let text = format!(
        "{} {} {}",
        subsystem.id.as_deref().unwrap_or(""),
        subsystem.path_prefix,
        subsystem.matched_tokens.join(" ")
    )
    .to_ascii_lowercase();
    let relation_kinds = Vec::new();
    let mut matched = classify_surface_subsystems(StoredNodeKind::File, &text, &relation_kinds)
        .into_iter()
        .filter(|id| *id != "backend_validator")
        .collect::<Vec<_>>();
    for node in &subsystem.nodes {
        let node_text = surface_candidate_text(
            node.kind,
            node.id.as_str(),
            node.display.as_str(),
            node.name.as_str(),
            node.path.as_deref(),
            &relation_kinds,
            &[],
        );
        for id in classify_surface_subsystems(node.kind, &node_text, &relation_kinds) {
            if !matched.contains(&id) {
                matched.push(id);
            }
        }
    }
    for subsystem_id in matched {
        let builder = builders
            .entry(subsystem_id)
            .or_insert_with(|| ExploreSubsystemBuilder::new(subsystem_id));
        builder.score += 100 + subsystem.rank.max(0);
        builder.add_path(subsystem.path_prefix.clone());
        for token in &subsystem.matched_tokens {
            builder.add_signal(format!("matched_token:{token}"));
        }
        builder.add_target(ExploreSubsystemTarget {
            kind: "subsystem_path".into(),
            target: subsystem.path_prefix.clone(),
            path: Some(subsystem.path_prefix.clone()),
            reason: "redb grouped matching Surface/Flow nodes under this path prefix.".into(),
            confidence: confidence_from_subsystem_score(100 + subsystem.rank.max(0)),
        });
    }
}

fn add_behavior_test_target(
    builders: &mut std::collections::BTreeMap<&'static str, ExploreSubsystemBuilder>,
    node: &NodeDisplay,
) {
    let text = surface_candidate_text(
        node.kind,
        node.id.as_str(),
        node.display.as_str(),
        node.name.as_str(),
        node.path.as_deref(),
        &[],
        &[],
    );
    let matched = classify_surface_subsystems(node.kind, &text, &[]);
    for subsystem_id in matched {
        let builder = builders
            .entry(subsystem_id)
            .or_insert_with(|| ExploreSubsystemBuilder::new(subsystem_id));
        builder.score += 45;
        if let Some(path) = node.path.as_deref() {
            builder.add_path(path);
        }
        builder.add_signal("tested_by");
        builder.add_target(ExploreSubsystemTarget {
            kind: "behavior_test".into(),
            target: node_target_label(node),
            path: node.path.clone(),
            reason: "Behavior test linked to a matching Surface/Flow candidate.".into(),
            confidence: 0.62,
        });
    }
}

fn apply_surface_flow_missing_warnings(
    builders: &mut std::collections::BTreeMap<&'static str, ExploreSubsystemBuilder>,
    missing: &[String],
) {
    for item in missing {
        match item.as_str() {
            "entrypoints" => {
                if let Some(builder) = builders.get_mut("ingress_proxy") {
                    builder.add_warning(
                        "No indexed ingress entrypoint matched this task; verify route/proxy discovery from source.",
                    );
                }
            }
            "surface_paths" => {
                for builder in builders.values_mut() {
                    builder.add_warning(
                        "Surface path coverage is partial for this task class; treat subsystem ordering as provisional.",
                    );
                }
            }
            "credential_flows" => {
                if let Some(builder) = builders.get_mut("backend_validator") {
                    builder.add_warning(
                        "No indexed credential-flow edge matched this task; verify credential issue/use/validation in source.",
                    );
                }
            }
            "behavior_tests" => {
                for builder in builders.values_mut() {
                    builder.add_warning(
                        "No linked live behavior test was indexed for this task class.",
                    );
                }
            }
            _ => {}
        }
    }
}

fn classify_surface_subsystems(
    kind: StoredNodeKind,
    lower_text: &str,
    relation_kinds: &[EdgeKind],
) -> Vec<&'static str> {
    let mut out = Vec::new();
    let provider_or_secondary = token_subsystem_matches(lower_text).iter().any(|matched| {
        matches!(
            matched.id,
            "oidc"
                | "audit_jws"
                | "auth0_management"
                | "webhook_tokens"
                | "profile_integrity"
                | "domain_verification"
        )
    }) || contains_any_text(
        lower_text,
        &["oauth", "provider", "management_token", "management token"],
    );
    let api_key = contains_any_text(
        lower_text,
        &["api keys", "api-key", "api_key", "api_keys", "apikey"],
    );
    let ingress_kind = matches!(
        kind,
        StoredNodeKind::RouteSurface
            | StoredNodeKind::WorkerSurface
            | StoredNodeKind::ProxySurface
            | StoredNodeKind::WebhookSurface
            | StoredNodeKind::CliSurface
            | StoredNodeKind::JobSurface
            | StoredNodeKind::QueueSurface
    );
    let ingress_relation = relation_kinds.iter().any(|kind| {
        matches!(
            kind,
            EdgeKind::EntrypointFor
                | EdgeKind::Exposes
                | EdgeKind::ForwardsTo
                | EdgeKind::RewritesHeader
        )
    });
    if ingress_kind
        || ingress_relation
        || contains_any_text(
            lower_text,
            &[
                "gcp-run-proxy",
                "proxy/",
                "proxy.",
                "worker.",
                "workers/",
                "route_surface",
                "webhook_surface",
                "middleware.ts",
                "middleware.js",
            ],
        )
    {
        out.push("ingress_proxy");
    }

    let credential_relation = relation_kinds.iter().any(|kind| {
        matches!(
            kind,
            EdgeKind::Authorizes
                | EdgeKind::IssuesCredential
                | EdgeKind::StoresCredential
                | EdgeKind::UsesCredential
                | EdgeKind::ValidatesCredential
        )
    });
    let backend_text = contains_any_text(lower_text, &["backend/", "backend.", "server/", "api/"]);
    let backend_validation_text = api_key
        || contains_any_text(
            lower_text,
            &[
                "bearer",
                "credential",
                "pk_",
                "authenticate",
                "authentication",
                "authorize",
                "permission",
                "validate",
                "validator",
            ],
        );
    let ingress_proxy_text = path_looks_ingress_proxy(lower_text);
    let backend_implementation_text = contains_any_text(
        lower_text,
        &[
            "backend/api_keys",
            "backend.api_keys",
            "/api_keys",
            "api_keys/",
        ],
    );
    let backend_candidate_kind = matches!(
        kind,
        StoredNodeKind::File
            | StoredNodeKind::Function
            | StoredNodeKind::Class
            | StoredNodeKind::CredentialOperation
            | StoredNodeKind::MiddlewareInstallation
            | StoredNodeKind::RouteSurface
    );
    if !provider_or_secondary
        && backend_text
        && backend_validation_text
        && (!ingress_proxy_text || backend_implementation_text)
        && (api_key || credential_relation || backend_candidate_kind)
    {
        out.push("backend_validator");
    }

    if provider_or_secondary && !out.contains(&"provider_oidc_audit") {
        out.push("provider_oidc_audit");
    }
    out
}

fn surface_candidate_text(
    kind: StoredNodeKind,
    id: &str,
    display: &str,
    name: &str,
    path: Option<&str>,
    relation_kinds: &[EdgeKind],
    matched_tokens: &[String],
) -> String {
    let relation_labels = relation_kinds
        .iter()
        .map(edge_kind_label_for_explore)
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{} {} {} {} {} {} {}",
        stored_node_kind_label(kind),
        id,
        display,
        name,
        path.unwrap_or(""),
        relation_labels,
        matched_tokens.join(" ")
    )
    .to_ascii_lowercase()
}

fn relation_reason(base: &str, relation_kinds: &[EdgeKind]) -> String {
    if relation_kinds.is_empty() {
        return base.to_string();
    }
    format!(
        "{} Relations: {}.",
        base,
        relation_kinds
            .iter()
            .map(edge_kind_label_for_explore)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn node_target_label(node: &NodeDisplay) -> String {
    if !node.display.trim().is_empty() {
        node.display.clone()
    } else if !node.name.trim().is_empty() {
        node.name.clone()
    } else {
        node.id.clone()
    }
}

fn confidence_from_subsystem_score(score: i32) -> f64 {
    let clamped = score.max(0).min(500) as f64;
    ((0.48 + (clamped / 500.0) * 0.44) * 100.0).round() / 100.0
}

fn subsystem_crossing_hint(subsystems: &[ExploreSubsystem]) -> Option<String> {
    let ingress = subsystems
        .iter()
        .find(|subsystem| subsystem.role == "ingress_proxy")
        .and_then(|subsystem| {
            preferred_matching_path(&subsystem.paths, &["gcp-run-proxy", "proxy"])
        });
    let backend = subsystems
        .iter()
        .find(|subsystem| subsystem.role == "backend_validator")
        .and_then(|subsystem| {
            preferred_matching_path(&subsystem.paths, &["backend/api_keys", "api_keys"])
        });
    match (ingress, backend) {
        (Some(ingress), Some(backend)) => Some(format!(
            "The likely inbound API-key path crosses {ingress} and {backend}. Verify proxy classification first, then backend validation."
        )),
        _ => None,
    }
}

fn preferred_matching_path(paths: &[String], preferred_needles: &[&str]) -> Option<String> {
    paths
        .iter()
        .find(|path| {
            let lower = path.to_ascii_lowercase();
            preferred_needles
                .iter()
                .any(|needle| lower.contains(needle))
        })
        .cloned()
}

fn subsystem_lane_ambiguity_value(subsystems: &[ExploreSubsystem]) -> serde_json::Value {
    serde_json::json!({
        "kind": "subsystem_lane_ambiguity",
        "status": "needs_verification",
        "reason": "Multiple subsystem lanes matched this request; verify the top lanes before committing to one implementation.",
        "verify_top_n": 2,
        "subsystems": subsystems
            .iter()
            .take(4)
            .map(|subsystem| serde_json::json!({
                "rank": subsystem.rank,
                "id": subsystem.id,
                "label": subsystem.label,
                "role": subsystem.role,
                "confidence": subsystem.confidence,
                "paths": subsystem.paths,
                "token_subsystems": subsystem.token_subsystems,
                "signals": subsystem.signals,
                "top_verification_targets": subsystem.top_verification_targets,
                "missing_coverage_warnings": subsystem.missing_coverage_warnings,
            }))
            .collect::<Vec<_>>(),
    })
}

fn token_subsystem_labels_from_signals_targets(
    signals: &[String],
    targets: &[ExploreSubsystemTarget],
) -> Vec<&'static str> {
    let mut labels = std::collections::BTreeSet::new();
    for signal in signals {
        match signal.as_str() {
            "api_key_surface" | "token_subsystem:api_keys" => {
                labels.insert("API keys");
            }
            "oidc_surface" | "token_subsystem:oidc" => {
                labels.insert("OIDC");
            }
            "audit_jws_surface" | "token_subsystem:audit_jws" => {
                labels.insert("audit JWS");
            }
            "auth0_management_surface" | "token_subsystem:auth0_management" => {
                labels.insert("Auth0 management");
            }
            "webhook_token_surface" | "token_subsystem:webhook_tokens" => {
                labels.insert("webhook tokens");
            }
            "profile_integrity_surface" | "token_subsystem:profile_integrity" => {
                labels.insert("profile-integrity");
            }
            "domain_verification_surface" | "token_subsystem:domain_verification" => {
                labels.insert("domain verification");
            }
            _ => {}
        }
    }
    for target in targets {
        let text = format!(
            "{} {} {}",
            target.target,
            target.path.as_deref().unwrap_or(""),
            target.reason
        )
        .to_ascii_lowercase();
        for matched in token_subsystem_matches(&text) {
            labels.insert(matched.label);
        }
    }
    labels.into_iter().collect()
}

fn edge_kind_label_for_explore(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Contains => "contains",
        EdgeKind::BelongsTo => "belongs_to",
        EdgeKind::Defines => "defines",
        EdgeKind::Imports => "imports",
        EdgeKind::Calls => "calls",
        EdgeKind::References => "references",
        EdgeKind::Documents => "documents",
        EdgeKind::Configures => "configures",
        EdgeKind::EntrypointFor => "entrypoint_for",
        EdgeKind::Authorizes => "authorizes",
        EdgeKind::Exposes => "exposes",
        EdgeKind::ForwardsTo => "forwards_to",
        EdgeKind::InstallsMiddleware => "installs_middleware",
        EdgeKind::IssuesCredential => "issues_credential",
        EdgeKind::StoresCredential => "stores_credential",
        EdgeKind::UsesCredential => "uses_credential",
        EdgeKind::ValidatesCredential => "validates_credential",
        EdgeKind::RewritesHeader => "rewrites_header",
        EdgeKind::TestedBy => "tested_by",
    }
}

fn stored_node_kind_label(kind: StoredNodeKind) -> &'static str {
    match kind {
        StoredNodeKind::Repository => "repository",
        StoredNodeKind::Directory => "directory",
        StoredNodeKind::File => "file",
        StoredNodeKind::Area => "area",
        StoredNodeKind::Function => "function",
        StoredNodeKind::Class => "class",
        StoredNodeKind::Doc => "doc",
        StoredNodeKind::Config => "config",
        StoredNodeKind::BehaviorTestSurface => "behavior_test_surface",
        StoredNodeKind::CliSurface => "cli_surface",
        StoredNodeKind::CredentialOperation => "credential_operation",
        StoredNodeKind::JobSurface => "job_surface",
        StoredNodeKind::MiddlewareInstallation => "middleware_installation",
        StoredNodeKind::ProxySurface => "proxy_surface",
        StoredNodeKind::QueueSurface => "queue_surface",
        StoredNodeKind::RouteSurface => "route_surface",
        StoredNodeKind::WebhookSurface => "webhook_surface",
        StoredNodeKind::WorkerSurface => "worker_surface",
        StoredNodeKind::Unresolved => "unresolved",
    }
}

fn contains_any_text(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// Translate the redb task-localize view into the answer-json
/// envelope the agent contract expects.
#[cfg(test)]
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
    build_response_with_surface_flow(
        request,
        intent,
        intent_source,
        view,
        symbol_matches,
        text_items,
        filename_items,
        callsite_items,
        &SurfaceFlowExploreEvidence::default(),
        params,
        observability,
    )
}

/// Translate the redb task-localize view into the answer-json
/// envelope the agent contract expects, with optional Surface/Flow
/// evidence projected into subsystem-ranked verification lanes.
fn build_response_with_surface_flow(
    request: &str,
    intent: Intent,
    intent_source: IntentSource,
    view: &serde_json::Value,
    symbol_matches: &SymbolBatchResults,
    text_items: &[AnswerItem],
    filename_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    surface_flow: &SurfaceFlowExploreEvidence,
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
    let mut subsystems = explore_subsystem_rankings(request, &token_subsystems, surface_flow);
    let token_subsystem_ambiguous =
        token_subsystems.len() >= 2 && !request_names_specific_token_subsystem(request);
    let subsystem_lane_ambiguous = subsystems.len() >= 2
        && ranking::auth_token_focus_from_request(request)
        && !request_names_provider_or_secondary_token_subsystem(request);
    let subsystem_ambiguous = token_subsystem_ambiguous || subsystem_lane_ambiguous;
    let mut ambiguous = Vec::new();
    if token_subsystem_ambiguous {
        ambiguous.push(token_subsystem_ambiguity_value(&token_subsystems));
    }
    if subsystem_lane_ambiguous {
        ambiguous.push(subsystem_lane_ambiguity_value(&subsystems));
    }

    let mut output_budget = OutputBudgetReport::default();
    if agent_output_profile(params) {
        apply_agent_output_budget(
            &mut answers,
            &mut nav_hints,
            &mut ambiguous,
            &mut subsystems,
            &mut output_budget,
        );
    }

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
    if subsystem_ambiguous && policy_kind == "answer_candidate" {
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
        _ if subsystem_ambiguous => {
            if let Some(hint) = subsystem_crossing_hint(&subsystems) {
                format!("Multiple token/auth subsystems matched. {hint}")
            } else {
                format!(
                    "Multiple token/auth subsystems matched ({}); verify the top 2 \
                     before relying on one implementation.",
                    top_token_subsystem_labels(&token_subsystems, 6)
                )
            }
        }
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
    let mut trust_policy = TrustPolicy {
        safe_to_use_as_answer,
        safe_to_use_as_navigation: !answers.is_empty() || !nav_hints.is_empty(),
        evidence_level: evidence_level.to_string(),
        authoritative_answer_count: high_confidence_count,
        navigation_hint_count,
        degraded: false,
        trust_policy: policy_kind,
        reason: trust_reason,
    };
    let mut degraded_reasons = explore_degraded_ranking_reasons(
        request,
        &trust_policy,
        &answers,
        &nav_hints,
        &symbol_items,
        text_items,
        callsite_items,
        surface_flow,
        subsystem_ambiguous,
        &observability,
    );
    trust_policy.degraded = !degraded_reasons.is_empty();

    let status = if answers.is_empty() && nav_hints.is_empty() {
        "degraded"
    } else {
        "complete"
    };

    let mut next_actions = if answers.is_empty() && nav_hints.is_empty() {
        vec![
            "Refine the request — graph navigation found no anchors.".into(),
            "Try a more specific keyword from the codebase domain.".into(),
        ]
    } else if subsystem_ambiguous {
        let mut actions = vec![
            "There are multiple token systems; verify the top 2 subsystem lanes before committing to one implementation."
                .into(),
        ];
        if let Some(hint) = subsystem_crossing_hint(&subsystems) {
            actions.push(hint);
        } else {
            actions.push(
                "Then read the top answer[] item and confirm it matches the intended inbound or provider-management path."
                    .into(),
            );
        }
        actions
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
    let mut verification_steps = build_verification_steps(
        &answers,
        &nav_hints,
        &trust_policy,
        text_items,
        if subsystem_ambiguous {
            token_subsystems.as_slice()
        } else {
            &[]
        },
        if subsystem_ambiguous {
            subsystems.as_slice()
        } else {
            &[]
        },
    );

    // Build output_adapters and resolved_parameters only when the
    // caller has asked for the explicit full profile. `--show-observability`
    // in compact/standard now emits a compact trust/coverage block instead
    // of the full debug envelope; agents need safety, lanes, and warnings on
    // the first call, not adapters and long path-hint arrays.
    let full_profile = matches!(params.detail, Detail::Full);

    // At the agent-facing profile, truncate tail arrays to keep the first
    // Explore call under the command-output budget. Full detail remains the
    // deliberate escape hatch for exhaustive debugging.
    if agent_output_profile(params) {
        apply_agent_tail_budget(
            &mut degraded_reasons,
            &mut verification_steps,
            &mut next_actions,
            &mut output_budget,
        );
    }

    let output_adapters = if full_profile {
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
    let resolved_parameters = if full_profile {
        Some(params.to_json())
    } else {
        None
    };
    let observability = if full_profile {
        Some(enrich_explore_observability(
            observability,
            request,
            &trust_policy,
            &degraded_reasons,
            &answers,
            &nav_hints,
            &symbol_items,
            text_items,
            callsite_items,
            &subsystems,
            surface_flow,
            subsystem_ambiguous,
        ))
    } else if params.show_observability {
        Some(compact_explore_observability(
            observability,
            request,
            &trust_policy,
            &degraded_reasons,
            &answers,
            &nav_hints,
            &symbol_items,
            text_items,
            callsite_items,
            &subsystems,
            surface_flow,
            subsystem_ambiguous,
        ))
    } else {
        None
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

    let response = ExploreResponse {
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
        subsystems,
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
        degraded_reasons,
        verification_steps,
        next_actions,
        available_specialized_intents: vec!["behavior_localization_query", "usage_boundary_query"],
        output_chars_estimate: 0,
        truncated: output_budget.truncated,
        output_adapters,
        resolved_parameters,
        observability,
    };
    response_with_output_estimate(response)
}

fn enrich_explore_observability(
    mut observability: serde_json::Value,
    request: &str,
    trust_policy: &TrustPolicy,
    degraded_reasons: &[String],
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    symbol_items: &[AnswerItem],
    text_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    subsystems: &[ExploreSubsystem],
    surface_flow: &SurfaceFlowExploreEvidence,
    subsystem_ambiguous: bool,
) -> serde_json::Value {
    let top_signals_used = explore_top_signals_used(
        answers,
        nav_hints,
        symbol_items,
        text_items,
        callsite_items,
        subsystems,
        surface_flow,
    );
    let top_signals_absent = explore_top_signals_absent(
        request,
        symbol_items,
        text_items,
        callsite_items,
        subsystems,
        surface_flow,
        &observability,
    );
    let readiness = explore_observability_readiness(
        request,
        trust_policy,
        &top_signals_used,
        degraded_reasons,
        surface_flow,
        &observability,
    );
    let answer_safe_after_observability = readiness
        .get("answer_safe_after_observability")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let navigation_only_after_observability = readiness
        .get("navigation_only_after_observability")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let mode = if answer_safe_after_observability {
        "answer_safe"
    } else if navigation_only_after_observability {
        "navigation_only"
    } else {
        "failed"
    };

    if let Some(obj) = observability.as_object_mut() {
        obj.insert(
            "ranking_explainability".into(),
            serde_json::json!({
                "degraded_ranking_reasons": degraded_reasons,
                "top_signals_used": top_signals_used,
                "top_signals_absent": top_signals_absent,
                "subsystem_ambiguous": subsystem_ambiguous,
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
        obj.insert("readiness".into(), readiness);
    }

    observability
}

fn compact_explore_observability(
    observability: serde_json::Value,
    request: &str,
    trust_policy: &TrustPolicy,
    degraded_reasons: &[String],
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    symbol_items: &[AnswerItem],
    text_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    subsystems: &[ExploreSubsystem],
    surface_flow: &SurfaceFlowExploreEvidence,
    subsystem_ambiguous: bool,
) -> serde_json::Value {
    let enriched = enrich_explore_observability(
        observability,
        request,
        trust_policy,
        degraded_reasons,
        answers,
        nav_hints,
        symbol_items,
        text_items,
        callsite_items,
        subsystems,
        surface_flow,
        subsystem_ambiguous,
    );
    let mut compact = serde_json::Map::new();
    if let Some(value) = enriched.get("graph_store").cloned() {
        compact.insert("graph_store".into(), value);
    }
    if let Some(value) = enriched.get("surface_flow_graph") {
        compact.insert(
            "surface_flow_graph".into(),
            compact_surface_flow_graph(value),
        );
    }
    if let Some(value) = enriched.get("answer_safety").cloned() {
        compact.insert("answer_safety".into(), value);
    }
    if let Some(value) = enriched.get("readiness").cloned() {
        compact.insert("readiness".into(), value);
    }
    if let Some(value) = enriched.get("ranking_explainability") {
        compact.insert(
            "ranking_explainability".into(),
            compact_ranking_explainability(value),
        );
    }
    compact.insert("output_profile".into(), serde_json::json!("agent_compact"));
    compact.insert(
        "full_observability_hint".into(),
        serde_json::json!("rerun with --detail full --show-observability"),
    );
    serde_json::Value::Object(compact)
}

fn compact_surface_flow_graph(value: &serde_json::Value) -> serde_json::Value {
    let mut compact = serde_json::Map::new();
    for key in [
        "schema_version",
        "status",
        "source_of_truth",
        "derived_query_artifact",
        "source_path_count_scanned",
        "indexed_path_count_scanned",
        "semantic_fragment_hit_count",
        "source_scan_truncated",
        "indexed_scan_truncated",
        "indexed_languages",
        "indexed_frameworks",
        "surface_type_count",
        "source_present_surface_count",
        "covered_surface_count",
    ] {
        if let Some(child) = value.get(key).cloned() {
            compact.insert(key.to_string(), child);
        }
    }
    if let Some(coverage) = value.get("coverage") {
        compact.insert("coverage".into(), compact_surface_flow_coverage(coverage));
    }
    if let Some(missing) = value.get("missing_expected_surfaces") {
        compact.insert(
            "missing_expected_surfaces".into(),
            compact_missing_expected_surfaces(missing),
        );
    }
    serde_json::Value::Object(compact)
}

fn compact_surface_flow_coverage(value: &serde_json::Value) -> serde_json::Value {
    let mut compact = serde_json::Map::new();
    if let Some(entries) = value.as_object() {
        for (surface_type, entry) in entries {
            let mut surface = serde_json::Map::new();
            for key in ["label", "source_present", "indexed", "status"] {
                if let Some(child) = entry.get(key).cloned() {
                    surface.insert(key.to_string(), child);
                }
            }
            compact.insert(surface_type.clone(), serde_json::Value::Object(surface));
        }
    }
    serde_json::Value::Object(compact)
}

fn compact_missing_expected_surfaces(value: &serde_json::Value) -> serde_json::Value {
    let Some(items) = value.as_array() else {
        return serde_json::json!([]);
    };
    serde_json::Value::Array(
        items
            .iter()
            .take(AGENT_OUTPUT_MAX_MISSING_SURFACES)
            .map(|item| {
                let mut surface = serde_json::Map::new();
                for key in ["surface_type", "label"] {
                    if let Some(child) = item.get(key).cloned() {
                        surface.insert(key.to_string(), child);
                    }
                }
                serde_json::Value::Object(surface)
            })
            .collect(),
    )
}

fn compact_ranking_explainability(value: &serde_json::Value) -> serde_json::Value {
    let mut compact = serde_json::Map::new();
    for key in ["subsystem_ambiguous", "degraded_ranking_reasons"] {
        if let Some(child) = value.get(key).cloned() {
            compact.insert(key.to_string(), child);
        }
    }
    if let Some(items) = value
        .get("top_signals_used")
        .and_then(|value| value.as_array())
    {
        compact.insert(
            "top_signals_used".into(),
            compact_signal_items(items, &["signal", "count"]),
        );
    }
    if let Some(items) = value
        .get("top_signals_absent")
        .and_then(|value| value.as_array())
    {
        compact.insert(
            "top_signals_absent".into(),
            compact_signal_items(items, &["signal"]),
        );
    }
    serde_json::Value::Object(compact)
}

fn compact_signal_items(items: &[serde_json::Value], keys: &[&str]) -> serde_json::Value {
    serde_json::Value::Array(
        items
            .iter()
            .take(AGENT_OUTPUT_MAX_SYMBOLS)
            .map(|item| {
                let mut compact = serde_json::Map::new();
                for key in keys {
                    if let Some(child) = item.get(*key).cloned() {
                        compact.insert((*key).to_string(), child);
                    }
                }
                serde_json::Value::Object(compact)
            })
            .collect(),
    )
}

fn explore_top_signals_used(
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    symbol_items: &[AnswerItem],
    text_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    subsystems: &[ExploreSubsystem],
    surface_flow: &SurfaceFlowExploreEvidence,
) -> Vec<serde_json::Value> {
    let mut counts = std::collections::BTreeMap::new();
    if !answers.is_empty() {
        add_signal_count(&mut counts, "ranked_answer_candidates");
    }
    if !nav_hints.is_empty() {
        add_signal_count(&mut counts, "navigation_hints");
    }
    for item in answers
        .iter()
        .chain(nav_hints.iter())
        .chain(symbol_items.iter())
        .chain(text_items.iter())
        .chain(callsite_items.iter())
    {
        collect_answer_item_signals(item, &mut counts);
    }
    if !subsystems.is_empty() {
        add_signal_count(&mut counts, "subsystem_lane_ranking");
    }
    for subsystem in subsystems {
        add_signal_count(&mut counts, format!("subsystem:{}", subsystem.role));
        for signal in &subsystem.signals {
            add_signal_count(&mut counts, format!("subsystem_signal:{signal}"));
        }
    }
    if !surface_flow.entrypoints.is_empty() {
        add_signal_count(&mut counts, "surface_flow_entrypoints");
    }
    if !surface_flow.surface_paths.is_empty() {
        add_signal_count(&mut counts, "surface_flow_paths");
    }
    if !surface_flow.credential_flows.is_empty() {
        add_signal_count(&mut counts, "surface_flow_credential_flows");
    }
    if !surface_flow.tests.is_empty() {
        add_signal_count(&mut counts, "surface_flow_behavior_tests");
    }
    signal_count_values(counts, 14)
}

fn collect_answer_item_signals(
    item: &AnswerItem,
    counts: &mut std::collections::BTreeMap<String, usize>,
) {
    match item.kind.as_str() {
        "anchor" | "anchor_area" => add_signal_count(counts, "graph_anchor"),
        "in_scope_file" | "in_scope_area" => add_signal_count(counts, "graph_scope"),
        "source_text_file" => add_signal_count(counts, "source_text_match"),
        "symbol_search_file" | "symbol_search" => add_signal_count(counts, "symbol_search_match"),
        "call_site_file" => add_signal_count(counts, "callsite_adjacency"),
        "filesystem_file" => add_signal_count(counts, "filesystem_filename_match"),
        _ => {}
    }
    collect_evidence_signals(&item.evidence, counts);
}

fn collect_evidence_signals(
    evidence: &serde_json::Value,
    counts: &mut std::collections::BTreeMap<String, usize>,
) {
    if let Some(source) = evidence.get("source").and_then(|value| value.as_str()) {
        match source {
            "source-text-search" => add_signal_count(counts, "source_text_match"),
            "task-localize.anchors" => add_signal_count(counts, "graph_anchor"),
            "task-localize.scope" => add_signal_count(counts, "graph_scope"),
            other => add_signal_count(counts, format!("evidence_source:{other}")),
        }
    }
    if evidence
        .get("line_refs")
        .and_then(|value| value.as_array())
        .is_some_and(|items| !items.is_empty())
    {
        add_signal_count(counts, "source_line_refs");
    }
    if evidence
        .get("matched_terms")
        .and_then(|value| value.as_array())
        .is_some_and(|items| items.len() >= 2)
    {
        add_signal_count(counts, "multi_term_source_text");
    }
    if evidence
        .get("matched_queries")
        .and_then(|value| value.as_array())
        .is_some_and(|items| !items.is_empty())
    {
        add_signal_count(counts, "symbol_name_match");
    }
    if evidence
        .get("matched_queries")
        .and_then(|value| value.as_array())
        .is_some_and(|items| items.len() >= 2)
    {
        add_signal_count(counts, "multi_query_symbol_match");
    }
    if evidence
        .get("symbols")
        .and_then(|value| value.as_array())
        .is_some_and(|items| !items.is_empty())
    {
        add_signal_count(counts, "callsite_or_symbol_rows");
    }
    if let Some(signals) = evidence
        .get("ranking_signals")
        .and_then(|value| value.as_array())
    {
        for signal in signals.iter().filter_map(|value| value.as_str()) {
            add_signal_count(counts, format!("ranking_signal:{signal}"));
        }
    }
    if let Some(symbol_evidence) = evidence.get("also_symbol_search") {
        collect_evidence_signals(symbol_evidence, counts);
    }
}

fn explore_top_signals_absent(
    request: &str,
    symbol_items: &[AnswerItem],
    text_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    subsystems: &[ExploreSubsystem],
    surface_flow: &SurfaceFlowExploreEvidence,
    observability: &serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut absent = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    if graph_freshness_status(observability) != Some("fresh") {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "fresh_graph_store",
            "The redb graph store is missing, stale, or freshness could not be proven.",
        );
    }
    if surface_flow_relevant_for_request(request, surface_flow)
        && !surface_flow_complete_for_request(observability)
    {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "complete_surface_flow_coverage",
            "The request depends on ingress/middleware/credential surfaces, but coverage is partial or unknown.",
        );
    }
    if symbol_items.is_empty() {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "symbol_search_match",
            "No redb symbol candidates matched the bounded query set.",
        );
    } else if !has_multi_query_symbol_file(symbol_items) {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "multi_query_symbol_match",
            "Symbol evidence matched only single request terms per file.",
        );
    }
    if text_items.is_empty() {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "source_text_match",
            "No bounded source-text candidate matched enough request terms.",
        );
    }
    if !has_symbol_text_corroboration(symbol_items, text_items) {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "symbol_text_corroboration",
            "No candidate file was confirmed by both multi-query symbol search and multi-term source text.",
        );
    }
    if callsite_items.is_empty() {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "callsite_adjacency",
            "No caller/callee expansion evidence was emitted for the ranked symbols.",
        );
    }
    let auth_focus = ranking::auth_token_focus_from_request(request);
    if auth_focus && surface_flow.entrypoints.is_empty() {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "surface_flow_entrypoints",
            "No indexed route/proxy/worker entrypoint candidate matched this auth or token task.",
        );
    }
    if auth_focus && surface_flow.credential_flows.is_empty() {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "surface_flow_credential_flows",
            "No persisted credential issue/store/use/validation edge matched this task.",
        );
    }
    if auth_focus && surface_flow.tests.is_empty() {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "linked_behavior_tests",
            "No indexed integration or behavior test was linked to the matching surface.",
        );
    }
    if auth_focus && subsystems.len() < 2 {
        push_absent_signal(
            &mut absent,
            &mut seen,
            "multi_lane_subsystem_ranking",
            "Explore did not have enough Surface/Flow evidence to rank competing subsystem lanes.",
        );
    }
    absent.into_iter().take(14).collect()
}

fn explore_degraded_ranking_reasons(
    request: &str,
    trust_policy: &TrustPolicy,
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    symbol_items: &[AnswerItem],
    text_items: &[AnswerItem],
    callsite_items: &[AnswerItem],
    surface_flow: &SurfaceFlowExploreEvidence,
    subsystem_ambiguous: bool,
    observability: &serde_json::Value,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if answers.is_empty() && nav_hints.is_empty() {
        push_unique_reason(&mut reasons, "no_ranked_candidates");
    } else if !trust_policy.safe_to_use_as_answer {
        push_unique_reason(
            &mut reasons,
            "navigation_only_without_authoritative_evidence",
        );
    }
    if subsystem_ambiguous {
        push_unique_reason(&mut reasons, "ambiguous_token_or_surface_subsystems");
    }
    if !trust_policy.safe_to_use_as_answer
        && !has_symbol_text_corroboration(symbol_items, text_items)
    {
        push_unique_reason(&mut reasons, "missing_symbol_text_corroboration");
    }
    if !trust_policy.safe_to_use_as_answer && symbol_items.is_empty() {
        push_unique_reason(&mut reasons, "missing_symbol_search_evidence");
    }
    if !trust_policy.safe_to_use_as_answer && text_items.is_empty() {
        push_unique_reason(&mut reasons, "missing_source_text_evidence");
    }
    if !trust_policy.safe_to_use_as_answer && callsite_items.is_empty() {
        push_unique_reason(&mut reasons, "missing_callsite_evidence");
    }
    if ranking::auth_token_focus_from_request(request) {
        if surface_flow.entrypoints.is_empty() {
            push_unique_reason(&mut reasons, "missing_ingress_surface_flow_candidates");
        }
        if surface_flow.credential_flows.is_empty() {
            push_unique_reason(&mut reasons, "missing_credential_flow_edges");
        }
    }
    if !surface_flow.coverage_missing.is_empty() {
        push_unique_reason(&mut reasons, "surface_flow_task_coverage_missing");
    }
    match graph_freshness_status(observability) {
        Some("fresh") => {}
        Some(status) => push_unique_reason(&mut reasons, format!("graph_store_{status}")),
        None => push_unique_reason(&mut reasons, "graph_store_status_unknown"),
    }
    if surface_flow_relevant_for_request(request, surface_flow)
        && !surface_flow_complete_for_request(observability)
    {
        push_unique_reason(&mut reasons, "surface_flow_coverage_not_complete_enough");
    }
    reasons
}

fn explore_observability_readiness(
    request: &str,
    trust_policy: &TrustPolicy,
    top_signals_used: &[serde_json::Value],
    degraded_reasons: &[String],
    surface_flow: &SurfaceFlowExploreEvidence,
    observability: &serde_json::Value,
) -> serde_json::Value {
    let graph_status = graph_freshness_status(observability).unwrap_or("unknown");
    let graph_fresh = graph_status == "fresh";
    let surface_status = surface_flow_status(observability).unwrap_or("unknown");
    let surface_relevant = surface_flow_relevant_for_request(request, surface_flow);
    let surface_complete = surface_flow_complete_for_request(observability);
    let complete_enough = if surface_relevant {
        surface_complete
    } else {
        graph_status != "missing"
    };
    let explainable = !top_signals_used.is_empty();
    let answer_safe_after_observability =
        trust_policy.safe_to_use_as_answer && graph_fresh && complete_enough && explainable;
    let navigation_only_after_observability =
        trust_policy.safe_to_use_as_navigation && !answer_safe_after_observability;
    let status = if answer_safe_after_observability {
        "answer_safe"
    } else if navigation_only_after_observability {
        "navigation_only"
    } else {
        "degraded"
    };

    serde_json::json!({
        "status": status,
        "fresh_enough": graph_fresh,
        "complete_enough": complete_enough,
        "surface_flow_relevant": surface_relevant,
        "surface_flow_complete": surface_complete,
        "explainable": explainable,
        "answer_safe_by_evidence": trust_policy.safe_to_use_as_answer,
        "answer_safe_after_observability": answer_safe_after_observability,
        "navigation_only_after_observability": navigation_only_after_observability,
        "graph_freshness_status": graph_status,
        "surface_flow_graph_status": surface_status,
        "degraded_reasons": degraded_reasons,
    })
}

fn surface_flow_relevant_for_request(
    request: &str,
    surface_flow: &SurfaceFlowExploreEvidence,
) -> bool {
    ranking::auth_token_focus_from_request(request)
        || !surface_flow.entrypoints.is_empty()
        || !surface_flow.surface_paths.is_empty()
        || !surface_flow.credential_flows.is_empty()
        || contains_any_text(
            &request.to_ascii_lowercase(),
            &[
                "credential",
                "entrypoint",
                "middleware",
                "proxy",
                "route",
                "surface",
                "webhook",
                "worker",
            ],
        )
}

fn surface_flow_complete_for_request(observability: &serde_json::Value) -> bool {
    let missing_count = observability
        .get("missing_expected_surfaces")
        .and_then(|value| value.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    matches!(
        surface_flow_status(observability),
        Some("covered") | Some("no_surface_signals")
    ) && missing_count == 0
}

fn graph_freshness_status(observability: &serde_json::Value) -> Option<&str> {
    observability
        .get("graph_freshness")
        .or_else(|| observability.get("graph_store"))
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
}

fn surface_flow_status(observability: &serde_json::Value) -> Option<&str> {
    observability
        .get("surface_flow_graph")
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
}

fn has_multi_query_symbol_file(symbol_items: &[AnswerItem]) -> bool {
    symbol_items.iter().any(|item| {
        item.evidence
            .get("matched_queries")
            .and_then(|value| value.as_array())
            .is_some_and(|items| items.len() >= 2)
    })
}

fn has_symbol_text_corroboration(symbol_items: &[AnswerItem], text_items: &[AnswerItem]) -> bool {
    let symbol_paths: std::collections::BTreeSet<&str> = symbol_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_queries")
                .and_then(|value| value.as_array())
                .is_some_and(|items| items.len() >= 2)
        })
        .filter_map(|item| item.path.as_deref())
        .collect();
    text_items
        .iter()
        .filter(|item| {
            item.evidence
                .get("matched_terms")
                .and_then(|value| value.as_array())
                .is_some_and(|items| items.len() >= 2)
        })
        .filter_map(|item| item.path.as_deref())
        .any(|path| symbol_paths.contains(path))
}

fn add_signal_count(
    counts: &mut std::collections::BTreeMap<String, usize>,
    signal: impl Into<String>,
) {
    let signal = signal.into();
    if !signal.trim().is_empty() {
        *counts.entry(signal).or_insert(0) += 1;
    }
}

fn signal_count_values(
    counts: std::collections::BTreeMap<String, usize>,
    limit: usize,
) -> Vec<serde_json::Value> {
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|(left_signal, left_count), (right_signal, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_signal.cmp(right_signal))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|(signal, count)| serde_json::json!({"signal": signal, "count": count}))
        .collect()
}

fn push_absent_signal(
    absent: &mut Vec<serde_json::Value>,
    seen: &mut std::collections::BTreeSet<String>,
    signal: &str,
    reason: &str,
) {
    if seen.insert(signal.to_string()) {
        absent.push(serde_json::json!({
            "signal": signal,
            "reason": reason,
        }));
    }
}

fn push_unique_reason(reasons: &mut Vec<String>, reason: impl Into<String>) {
    let reason = reason.into();
    if !reasons.iter().any(|existing| existing == &reason) {
        reasons.push(reason);
    }
}

#[derive(Debug, Default)]
struct OutputBudgetReport {
    truncated: bool,
}

fn agent_output_profile(params: &ExploreParams) -> bool {
    !matches!(params.detail, Detail::Full) && (params.show_observability || params.depth.is_some())
}

fn cap_vec<T>(items: &mut Vec<T>, max: usize, report: &mut OutputBudgetReport) {
    if items.len() > max {
        items.truncate(max);
        report.truncated = true;
    }
}

fn apply_agent_output_budget(
    answers: &mut Vec<AnswerItem>,
    nav_hints: &mut Vec<AnswerItem>,
    ambiguous: &mut [serde_json::Value],
    subsystems: &mut Vec<ExploreSubsystem>,
    report: &mut OutputBudgetReport,
) {
    cap_vec(answers, AGENT_OUTPUT_MAX_ANSWER_ITEMS, report);
    cap_vec(nav_hints, AGENT_OUTPUT_MAX_NAVIGATION_HINTS, report);
    cap_vec(subsystems, AGENT_OUTPUT_MAX_SUBSYSTEMS, report);

    for item in answers.iter_mut().chain(nav_hints.iter_mut()) {
        budget_answer_item(item, report);
    }
    for value in ambiguous {
        budget_ambiguity_value(value, report);
    }
    for subsystem in subsystems {
        cap_vec(
            &mut subsystem.paths,
            AGENT_OUTPUT_MAX_SUBSYSTEM_PATHS,
            report,
        );
        cap_vec(
            &mut subsystem.top_verification_targets,
            AGENT_OUTPUT_MAX_SUBSYSTEM_TARGETS,
            report,
        );
        for target in &mut subsystem.top_verification_targets {
            shorten_string(
                &mut target.reason,
                AGENT_OUTPUT_MAX_TARGET_REASON_CHARS,
                report,
            );
        }
        cap_vec(
            &mut subsystem.signals,
            AGENT_OUTPUT_MAX_SUBSYSTEM_SIGNALS,
            report,
        );
        cap_vec(
            &mut subsystem.missing_coverage_warnings,
            AGENT_OUTPUT_MAX_WARNINGS,
            report,
        );
    }
}

fn apply_agent_tail_budget(
    degraded_reasons: &mut Vec<String>,
    verification_steps: &mut Vec<serde_json::Value>,
    next_actions: &mut Vec<String>,
    report: &mut OutputBudgetReport,
) {
    cap_vec(degraded_reasons, AGENT_OUTPUT_MAX_DEGRADED_REASONS, report);
    cap_vec(
        verification_steps,
        AGENT_OUTPUT_MAX_VERIFICATION_STEPS,
        report,
    );
    cap_vec(next_actions, AGENT_OUTPUT_MAX_NEXT_ACTIONS, report);
}

fn budget_answer_item(item: &mut AnswerItem, report: &mut OutputBudgetReport) {
    budget_evidence_value(&mut item.evidence, None, report);
}

fn shorten_string(value: &mut String, max_chars: usize, report: &mut OutputBudgetReport) {
    if value.chars().count() <= max_chars {
        return;
    }
    let shortened = value.chars().take(max_chars).collect::<String>();
    *value = format!("{shortened}...");
    report.truncated = true;
}

fn budget_evidence_value(
    value: &mut serde_json::Value,
    key: Option<&str>,
    report: &mut OutputBudgetReport,
) {
    match value {
        serde_json::Value::Array(items) => {
            let max = match key {
                Some("ranking_signals") => AGENT_OUTPUT_MAX_RANKING_SIGNALS,
                Some("matched_terms") => AGENT_OUTPUT_MAX_MATCHED_TERMS,
                Some("matched_queries") => AGENT_OUTPUT_MAX_MATCHED_QUERIES,
                Some("symbols") => AGENT_OUTPUT_MAX_SYMBOLS,
                Some("line_refs") => AGENT_OUTPUT_MAX_LINE_REFS,
                _ => AGENT_OUTPUT_MAX_EVIDENCE_ARRAY_ITEMS,
            };
            cap_vec(items, max, report);
            for item in items {
                budget_evidence_value(item, None, report);
            }
        }
        serde_json::Value::Object(map) => {
            for (child_key, child_value) in map {
                budget_evidence_value(child_value, Some(child_key.as_str()), report);
            }
        }
        _ => {}
    }
}

fn budget_ambiguity_value(value: &mut serde_json::Value, report: &mut OutputBudgetReport) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let Some(subsystems) = obj
        .get_mut("subsystems")
        .and_then(|value| value.as_array_mut())
    else {
        return;
    };
    cap_vec(subsystems, AGENT_OUTPUT_MAX_AMBIGUITY_ARRAY_ITEMS, report);
    for subsystem in subsystems {
        let Some(subsystem_obj) = subsystem.as_object_mut() else {
            continue;
        };
        let keep: std::collections::BTreeSet<&str> = [
            "id",
            "label",
            "rank",
            "role",
            "score",
            "confidence",
            "token_subsystems",
            "missing_coverage_warnings",
        ]
        .into_iter()
        .collect();
        let keys_to_remove = subsystem_obj
            .keys()
            .filter(|key| !keep.contains(key.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !keys_to_remove.is_empty() {
            report.truncated = true;
        }
        for key in keys_to_remove {
            subsystem_obj.remove(&key);
        }
        if let Some(token_subsystems) = subsystem_obj
            .get_mut("token_subsystems")
            .and_then(|value| value.as_array_mut())
        {
            cap_vec(
                token_subsystems,
                AGENT_OUTPUT_MAX_AMBIGUITY_ARRAY_ITEMS,
                report,
            );
        }
        if let Some(warnings) = subsystem_obj
            .get_mut("missing_coverage_warnings")
            .and_then(|value| value.as_array_mut())
        {
            cap_vec(warnings, AGENT_OUTPUT_MAX_WARNINGS, report);
        }
    }
}

pub(super) fn response_with_output_estimate(mut response: ExploreResponse) -> ExploreResponse {
    response.output_chars_estimate = serde_json::to_string_pretty(&response)
        .map(|json| json.len())
        .unwrap_or(0);
    response
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
///   4. If failed/no answers → suggest broadening the request or increasing
///      native detail for richer evidence
///   5. Generic "open top answer and confirm" as a final fallback
fn build_verification_steps(
    answers: &[AnswerItem],
    nav_hints: &[AnswerItem],
    trust_policy: &TrustPolicy,
    text_items: &[AnswerItem],
    token_subsystems: &[TokenSubsystemSummary],
    subsystems: &[ExploreSubsystem],
) -> Vec<serde_json::Value> {
    let mut steps: Vec<serde_json::Value> = Vec::new();

    if token_subsystems.len() >= 2 || subsystems.len() >= 2 {
        let label_list = if !subsystems.is_empty() {
            subsystems
                .iter()
                .take(4)
                .map(|subsystem| subsystem.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            top_token_subsystem_labels(token_subsystems, 6)
        };
        steps.push(serde_json::json!({
            "step": format!(
                "There are multiple token systems: Aethyme found multiple \
                 subsystem lanes ({}). Verify the top 2 before committing \
                 to one subsystem.",
                label_list
            ),
            "rationale": "Broad token requests can match API keys, OIDC, audit \
                          JWS, provider-management, profile-integrity, and \
                          domain-verification code. Checking the top two \
                          prevents anchoring on the first plausible token hit.",
        }));
    }

    if let Some(hint) = subsystem_crossing_hint(subsystems) {
        steps.push(serde_json::json!({
            "step": hint,
            "rationale": "Inbound credential behavior often crosses an edge/proxy \
                          layer before backend validation. Verifying the boundary \
                          order prevents confusing provider-token helpers with \
                          request authentication.",
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
                         `--detail standard`; use `--detail full \
                         --show-observability` only when the wider native \
                         evidence still needs diagnosis.",
                "rationale": "The compact native path prioritizes bounded \
                              output; higher native detail levels expose \
                              wider evidence and diagnostics.",
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
        let verification = serde_json::to_string(&response.verification_steps).unwrap();
        assert!(verification.contains("--detail standard"));
        assert!(verification.contains("--detail full"));
        assert!(!verification.contains("Python"));
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
                    name: "verify_webhook_token_signature".into(),
                    kind: "function".into(),
                    file: "backend/accounts/webhook_tokens.py".into(),
                    line: 44,
                    score: 780,
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
            "webhook tokens",
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
        assert!(
            !response.subsystems.is_empty(),
            "broad token ambiguity should emit top-level subsystem lanes"
        );
        assert_eq!(
            response.subsystems[0].role, "backend_validator",
            "without ingress/proxy graph evidence, API-key validation should be the first subsystem lane"
        );
    }

    #[test]
    fn build_response_auth_token_subsystems_include_proxy_backend_and_provider_lanes() {
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
        let proxy = NodeDisplay {
            id: "surface:proxy:gcp-run-proxy:token".into(),
            kind: StoredNodeKind::ProxySurface,
            display: "gcp-run-proxy token ingress".into(),
            name: "gcp-run-proxy".into(),
            path: Some("gcp-run-proxy/src/index.ts".into()),
            language: Some("typescript".into()),
            area_id: None,
        };
        let backend = NodeDisplay {
            id: "surface:credential:backend/api_keys/models.py:api-key".into(),
            kind: StoredNodeKind::CredentialOperation,
            display: "backend API-key validation".into(),
            name: "authenticate_api_key".into(),
            path: Some("backend/api_keys/models.py".into()),
            language: Some("python".into()),
            area_id: None,
        };
        let surface_flow = SurfaceFlowExploreEvidence {
            entrypoints: vec![SurfaceFlowCandidate {
                node: proxy,
                signals: SymbolMatchSignals::default(),
                matched_tokens: vec!["token".into(), "auth".into()],
                relation_kinds: vec![EdgeKind::ForwardsTo, EdgeKind::RewritesHeader],
                rank: 260,
            }],
            surface_paths: vec![SurfacePathCandidate {
                path: "backend/api_keys/models.py".into(),
                surfaces: vec![backend.clone()],
                matched_tokens: vec!["token".into(), "credential".into()],
                relation_kinds: vec![EdgeKind::ValidatesCredential, EdgeKind::Authorizes],
                rank: 240,
            }],
            credential_flows: vec![SurfaceFlowCandidate {
                node: backend,
                signals: SymbolMatchSignals::default(),
                matched_tokens: vec!["token".into(), "credential".into()],
                relation_kinds: vec![
                    EdgeKind::IssuesCredential,
                    EdgeKind::UsesCredential,
                    EdgeKind::ValidatesCredential,
                ],
                rank: 240,
            }],
            coverage_missing: vec!["behavior_tests".into()],
            ..SurfaceFlowExploreEvidence::default()
        };

        let response = build_response_with_surface_flow(
            "trace API key token issuing and authentication behavior",
            Intent::TaskLocalization,
            IntentSource::Default,
            &sample_view(),
            &symbols,
            &[],
            &[],
            &[],
            &surface_flow,
            &ExploreParams::default(),
            test_observability(),
        );

        let roles: Vec<&str> = response
            .subsystems
            .iter()
            .map(|subsystem| subsystem.role.as_str())
            .collect();
        assert_eq!(
            roles.iter().take(3).copied().collect::<Vec<_>>(),
            vec![
                "ingress_proxy",
                "backend_validator",
                "provider_or_secondary_token"
            ]
        );
        let all_paths = response
            .subsystems
            .iter()
            .flat_map(|subsystem| subsystem.paths.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            all_paths.contains("gcp-run-proxy"),
            "proxy lane should point at gcp-run-proxy evidence: {all_paths}"
        );
        assert!(
            all_paths.contains("backend/api_keys"),
            "backend validator lane should point at API-key evidence: {all_paths}"
        );
        let steps = response
            .verification_steps
            .iter()
            .filter_map(|step| step.get("step").and_then(|value| value.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            steps.contains("Verify proxy classification first, then backend validation"),
            "verification should name the proxy-first order: {steps}"
        );
        assert!(
            response.subsystems.iter().any(|subsystem| subsystem
                .missing_coverage_warnings
                .iter()
                .any(|warning| warning.contains("No linked live behavior test"))),
            "subsystems should surface missing live-test coverage warnings"
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
        assert!(
            response.subsystems.is_empty(),
            "provider-specific token requests should not emit competing subsystem lanes"
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
    //       gate correctly on `Detail::Full`.
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
        "subsystems",
        "evidence",
        "confidence",
        "safe_to_use_as_answer",
        "safe_to_use_as_navigation",
        "trust_policy",
        "degraded_reasons",
        "verification_steps",
        "next_actions",
        "available_specialized_intents",
        "output_chars_estimate",
        "truncated",
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
        assert!(
            response.output_chars_estimate > 0,
            "compact response should report an output char estimate"
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
    fn response_show_observability_uses_agent_compact_profile() {
        // `--show-observability` at compact emits the agent-facing trust
        // summary, not the full debug envelope.
        let response = build_minimal_response(Detail::Compact, true);
        let json = serde_json::to_value(&response).unwrap();
        let obj = json.as_object().unwrap();
        assert!(
            !obj.contains_key("output_adapters"),
            "agent compact observability must not emit output_adapters"
        );
        assert!(
            !obj.contains_key("resolved_parameters"),
            "agent compact observability must not emit resolved_parameters"
        );
        assert!(
            obj.contains_key("observability"),
            "show_observability=true must emit compact observability"
        );
        assert_eq!(
            obj["observability"]["output_profile"], "agent_compact",
            "compact observability should identify its profile"
        );
    }

    #[test]
    fn observability_reports_surface_flow_coverage_gaps() {
        let tmp = tempfile::tempdir().unwrap();
        write_test_file(
            tmp.path(),
            "backend/api_keys/middleware.py",
            "class APIKeyAuthenticationMiddleware: pass\n",
        );
        write_test_file(
            tmp.path(),
            "gcp-run-proxy/src/worker.mjs",
            "export default { fetch(request) { return fetch(request) } }\n",
        );
        write_test_file(
            tmp.path(),
            ".aethyme/graph/_index/backend.api_keys.middleware.ndjson",
            r#"{"module":"backend.api_keys.middleware","symbol":"APIKeyAuthenticationMiddleware","kind":"class","node_id":"class:demo:abc","file":"backend/api_keys/middleware.py"}"#,
        );
        write_test_file(
            tmp.path(),
            ".aethyme/graph/backend/api_keys/middleware.py.bin",
            "middleware_installation validates_credential\n",
        );
        write_test_file(
            tmp.path(),
            ".aethyme/graph/backend/api_keys/urls.py.bin",
            "route_surface exposes\n",
        );
        write_test_file(tmp.path(), ".aethyme/graph_store.redb", "placeholder");

        let observability = graph_store_observability(tmp.path());
        assert_eq!(
            observability
                .get("graph_freshness")
                .and_then(|value| value.get("status"))
                .and_then(|value| value.as_str()),
            Some("fresh")
        );
        assert_eq!(
            observability
                .get("graph_freshness")
                .and_then(|value| value.get("fresh"))
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        let surface_flow = observability
            .get("surface_flow_graph")
            .and_then(|value| value.as_object())
            .expect("surface_flow_graph observability object");
        assert_eq!(
            surface_flow.get("status").and_then(|value| value.as_str()),
            Some("partial")
        );

        let coverage = surface_flow
            .get("coverage")
            .and_then(|value| value.as_object())
            .expect("coverage object");
        let backend = coverage
            .get("backend")
            .and_then(|value| value.as_object())
            .expect("backend coverage");
        assert_eq!(
            backend.get("source_present").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(backend.get("indexed").and_then(|v| v.as_bool()), Some(true));

        let edge_proxy = coverage
            .get("edge_proxy")
            .and_then(|value| value.as_object())
            .expect("edge proxy coverage");
        assert_eq!(
            edge_proxy.get("source_present").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            edge_proxy.get("indexed").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            edge_proxy.get("status").and_then(|v| v.as_str()),
            Some("source_present_not_indexed")
        );

        let missing = surface_flow
            .get("missing_expected_surfaces")
            .and_then(|value| value.as_array())
            .expect("missing surface list");
        assert!(missing.iter().any(|item| {
            item.get("surface_type").and_then(|value| value.as_str()) == Some("edge_proxy")
        }));
        let top_level_missing = observability
            .get("missing_expected_surfaces")
            .and_then(|value| value.as_array())
            .expect("top-level missing surface list");
        assert_eq!(top_level_missing.len(), missing.len());
        assert_eq!(
            observability
                .get("graph_completeness_by_surface_type")
                .and_then(|value| value.get("edge_proxy"))
                .and_then(|value| value.get("status"))
                .and_then(|value| value.as_str()),
            Some("source_present_not_indexed")
        );
        let languages = observability
            .get("indexed_languages")
            .and_then(|value| value.as_array())
            .expect("indexed languages");
        assert!(
            languages
                .iter()
                .any(|value| value.as_str() == Some("python")),
            "expected python indexed language in {languages:?}"
        );
        let frameworks = observability
            .get("indexed_frameworks")
            .and_then(|value| value.as_array())
            .expect("indexed frameworks");
        assert!(
            frameworks
                .iter()
                .any(|value| value.as_str() == Some("django")),
            "expected django indexed framework in {frameworks:?}"
        );
    }

    #[test]
    fn observability_reports_partially_indexed_surface_families() {
        let tmp = tempfile::tempdir().unwrap();
        write_test_file(
            tmp.path(),
            "functions/_middleware.ts",
            "export function onRequest() {}\n",
        );
        write_test_file(
            tmp.path(),
            "gcp-run-proxy/src/worker.mjs",
            "export default { fetch(request) { return fetch(request) } }\n",
        );
        write_test_file(
            tmp.path(),
            ".aethyme/graph/_index/functions._middleware.ndjson",
            r#"{"module":"functions._middleware","symbol":"onRequest","kind":"function","node_id":"function:demo:def","file":"functions/_middleware.ts"}"#,
        );
        write_test_file(
            tmp.path(),
            ".aethyme/graph/functions/_middleware.ts.bin",
            r#"{"kind":"worker_surface","path":"functions/_middleware.ts","trigger":"request"}"#,
        );
        write_test_file(
            tmp.path(),
            ".aethyme/graph/gcp-run-proxy/FOLDER.gcp-run-proxy.md.bin",
            "folder summary",
        );
        write_test_file(
            tmp.path(),
            ".aethyme/graph/gcp-run-proxy/package.json.bin",
            "{\"scripts\":{\"deploy\":\"wrangler deploy\"}}\n",
        );

        let observability = graph_store_observability(tmp.path());
        let edge_proxy = observability
            .get("surface_flow_graph")
            .and_then(|value| value.get("coverage"))
            .and_then(|value| value.get("edge_proxy"))
            .and_then(|value| value.as_object())
            .expect("edge proxy coverage");

        assert_eq!(
            edge_proxy.get("status").and_then(|v| v.as_str()),
            Some("partially_indexed")
        );
        assert_eq!(
            edge_proxy.get("path_indexed").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            edge_proxy.get("semantic_indexed").and_then(|v| v.as_bool()),
            Some(true)
        );
        let unindexed = edge_proxy
            .get("unindexed_source_path_hints")
            .and_then(|value| value.as_array())
            .expect("unindexed source hints");
        assert!(unindexed.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|path| path.starts_with("gcp-run-proxy/"))
        }));
        let languages = observability
            .get("indexed_languages")
            .and_then(|value| value.as_array())
            .expect("indexed languages");
        assert!(
            languages
                .iter()
                .any(|value| value.as_str() == Some("typescript")),
            "expected typescript indexed language in {languages:?}"
        );
        let frameworks = observability
            .get("indexed_frameworks")
            .and_then(|value| value.as_array())
            .expect("indexed frameworks");
        assert!(
            frameworks
                .iter()
                .any(|value| value.as_str() == Some("edge-middleware")),
            "expected edge-middleware indexed framework in {frameworks:?}"
        );
    }

    #[test]
    fn full_response_observability_reports_safety_and_ranking_signals() {
        let text_match = AnswerItem {
            kind: "source_text_file".into(),
            target: "includes/Watchlist/WatchedItemStore.php".into(),
            path: Some("includes/Watchlist/WatchedItemStore.php".into()),
            status: "candidate".into(),
            confidence: 0.87,
            reason: "source text evidence".into(),
            role: "candidate".into(),
            evidence: serde_json::json!({
                "source": "source-text-search",
                "matched_terms": ["watchlist", "revision"],
                "line_refs": [{"line": 12, "text": "watchlist revision", "matched_terms": ["watchlist", "revision"]}],
            }),
        };
        let symbols = symbols_for(
            "includes/Watchlist/WatchedItemStore.php",
            &[("watchlist", 200), ("revision", 300)],
        );
        let params = ExploreParams {
            detail: Detail::Full,
            ..ExploreParams::default()
        };

        let response = build_response(
            "find watchlist revision handlers",
            Intent::TaskLocalization,
            IntentSource::Default,
            &sample_view(),
            &symbols,
            &[text_match],
            &[],
            &[],
            &params,
            test_observability(),
        );

        let observability = response
            .observability
            .as_ref()
            .expect("full detail emits observability");
        assert_eq!(
            observability
                .get("answer_safety")
                .and_then(|value| value.get("mode"))
                .and_then(|value| value.as_str()),
            Some("answer_safe")
        );
        assert_eq!(
            observability
                .get("readiness")
                .and_then(|value| value.get("fresh_enough"))
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            observability
                .get("readiness")
                .and_then(|value| value.get("complete_enough"))
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        let signals = observability
            .get("ranking_explainability")
            .and_then(|value| value.get("top_signals_used"))
            .and_then(|value| value.as_array())
            .expect("used signals");
        assert!(
            signals.iter().any(|value| {
                value.get("signal").and_then(|signal| signal.as_str())
                    == Some("multi_query_symbol_match")
            }),
            "expected multi-query symbol signal in {signals:?}"
        );
        assert!(
            signals.iter().any(|value| {
                value.get("signal").and_then(|signal| signal.as_str()) == Some("source_line_refs")
            }),
            "expected source line-ref signal in {signals:?}"
        );
        let absent = observability
            .get("ranking_explainability")
            .and_then(|value| value.get("top_signals_absent"))
            .and_then(|value| value.as_array())
            .expect("absent signals");
        assert!(
            absent.iter().any(|value| {
                value.get("signal").and_then(|signal| signal.as_str()) == Some("callsite_adjacency")
            }),
            "expected missing callsite signal in {absent:?}"
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

    fn write_test_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}
