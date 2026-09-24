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
//!
//! Module map
//! ----------
//!
//! - `params`: request parsing — intents, intent source, disclosure levels,
//!   parameters, symbol-query extraction.
//! - `graph_path`: the graph-backed entry point and its redb reads.
//! - `graph_free` + `source_fallback` (with `query_terms`, `path_role`,
//!   `symbol_index`): the bounded source search used without a graph.
//! - `text_search`, `filename_match`, `callsite`, `ranking`: evidence passes
//!   and ranking helpers shared by the graph path.
//! - `response`: answer-json synthesis, output budgets, verification steps.
//! - `observability` + `surface_coverage`: freshness, Surface/Flow coverage,
//!   ranking explainability and readiness.
//! - `usage_boundary`: the `usage_boundary_query` intent.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::graph::navigation::{task_anchors_view_redb, task_next_view_redb, task_scope_view_redb};
use crate::graph::search::{SearchHit, symbol_search_redb};
use crate::model::task::TaskInput;
use crate::store::redb::graph_store::{
    GraphStore, GraphStoreError, NodeDisplay, ReadOnlyGraphStore, SurfaceFlowCandidate,
    SurfacePathCandidate,
};

mod callsite;
mod filename_match;
mod graph_free;
mod graph_path;
mod observability;
mod params;
mod path_role;
mod query_terms;
mod ranking;
mod response;
mod source_fallback;
mod surface_coverage;
mod symbol_index;
mod text_search;
mod usage_boundary;

#[cfg(test)]
mod disclosure_tests;
#[cfg(test)]
mod tests;

pub use graph_free::{graph_unavailable_response, graph_unavailable_response_with};
pub use graph_path::explore_with_intent;
pub use params::{DISCLOSURE_LEVELS, Detail, DisclosureLevel, ExploreParams, Intent, IntentSource};
pub use source_fallback::{
    DEFAULT_SOURCE_SEARCH_BUDGET, DEFAULT_SOURCE_SEARCH_HITS, SourceSearchOptions, SymbolCache,
};
pub use usage_boundary::{UsageBoundaryParams, explore_usage_boundary};

use callsite::compute_callsite_files;
use filename_match::filename_token_matches;
use graph_free::*;
use graph_path::*;
use observability::*;
use params::*;
use response::*;
use surface_coverage::*;
pub(crate) use text_search::extract_text_search_terms;

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

impl std::error::Error for ExploreError {}
