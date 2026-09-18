//! Advisory graph-impact provider boundary.
//!
//! The broker owns the safety contract around semantic gate advice, but not
//! the graph engine or its storage. Providers therefore return degraded
//! outcomes as data instead of errors: a cold, stale, or broken graph must not
//! prevent path-selected gates from being reported or run.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use aethyme_engine::model::edge::EdgeKind;
use aethyme_engine::store::redb::graph_store::{
    GraphStore, GraphStoreError, NeighborDirection, ReadOnlyGraphStore, StoredNodeKind,
};
use aethyme_graph_storage::{GraphCoverage, read_coverage};
use sha2::{Digest, Sha256};

/// Maximum number of provider-ranked impact paths admitted to one report.
pub const GRAPH_IMPACT_RESULT_LIMIT: usize = 64;
/// Maximum incoming relationship hops from a changed graph node.
pub const GRAPH_IMPACT_MAX_DEPTH: usize = 2;
/// Maximum distinct graph nodes admitted to the bounded traversal.
pub const GRAPH_IMPACT_MAX_NODES: usize = 128;
/// Version of the machine-readable, revision-bound impact projection.
pub const GRAPH_IMPACT_CONTRACT_SCHEMA_VERSION: u32 = 1;
/// Default caller budget for the contract projection.
pub const GRAPH_IMPACT_DEFAULT_BUDGET: usize = GRAPH_IMPACT_MAX_NODES;
/// Hard upper bound for a caller-provided impact budget.
pub const GRAPH_IMPACT_MAX_BUDGET: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum GraphImpactContractError {
    #[error("graph impact budget must be between 1 and {GRAPH_IMPACT_MAX_BUDGET}")]
    InvalidBudget,
    #[error("graph impact diff contains unsafe repository path {path:?}")]
    UnsafePath { path: String },
    #[error(
        "graph impact diff JSON must be an array of paths or an object with a changed_files/files array"
    )]
    InvalidDiffShape,
    #[error("graph impact diff JSON path entries must be strings")]
    InvalidDiffEntry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphImpactContractStatus {
    Complete,
    Partial,
    Unavailable,
    Stale,
}

impl GraphImpactContractStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
            Self::Stale => "stale",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphImpactConfidence {
    High,
    Medium,
    Low,
    Unknown,
}

impl GraphImpactConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphImpactRepository {
    pub root: String,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphImpactRequestSummary {
    pub changed_files: usize,
    pub diff_digest: String,
    pub mode: GraphImpactMode,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphImpactCoverage {
    pub mode: String,
    pub languages: Vec<String>,
    pub parsed_files: u64,
    pub excluded_files: u64,
    pub unsupported_files: u64,
    pub missing_edge_kinds: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct GraphImpactSet {
    pub direct: Vec<String>,
    pub transitive: Vec<String>,
    pub callers: Vec<String>,
    pub importers: Vec<String>,
    pub tests: Vec<String>,
    pub configs: Vec<String>,
    pub manifests: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphImpactRiskHints {
    pub security_surface: bool,
    pub runtime_surface: bool,
    pub workspace_surface: bool,
    pub global_config_surface: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphImpactLimits {
    pub budget: usize,
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_results: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphImpactProvenance {
    pub graph_revision: Option<String>,
    pub engine_version: String,
    pub request_digest: String,
    pub result_digest: String,
}

/// Stable, conservative impact information for an orchestrator. A report with
/// status `complete` is the only result eligible for an adaptive fast path;
/// all other statuses remain advisory and require the consumer to fall back.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphImpactReport {
    pub schema_version: u32,
    pub repository: GraphImpactRepository,
    pub request: GraphImpactRequestSummary,
    pub status: GraphImpactContractStatus,
    pub confidence: GraphImpactConfidence,
    pub coverage: GraphImpactCoverage,
    pub impact: GraphImpactSet,
    pub risk_hints: GraphImpactRiskHints,
    pub limits: GraphImpactLimits,
    pub provenance: GraphImpactProvenance,
    pub explanations: Vec<String>,
}

/// Relationship used to derive a bounded graph-impact frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphImpactMode {
    Calls,
    Imports,
}

impl GraphImpactMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "calls" => Some(Self::Calls),
            "imports" => Some(Self::Imports),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Imports => "imports",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Calls => "Calls",
            Self::Imports => "Imports",
        }
    }
}

/// Parse the deliberately small diff input accepted by the impact contract.
/// JSON is preferred for callers that already have structured paths; the
/// line-oriented form keeps `git diff --name-only` and `--name-status` useful
/// without making the broker parse a patch.
pub fn parse_diff_text(input: &str) -> Result<Vec<String>, GraphImpactContractError> {
    let trimmed = input.trim();
    let raw_paths = if trimmed.starts_with('[') || trimmed.starts_with('{') {
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|_| GraphImpactContractError::InvalidDiffShape)?;
        match value {
            serde_json::Value::Array(entries) => entries,
            serde_json::Value::Object(mut object) => {
                let Some(entries) = object
                    .remove("changed_files")
                    .or_else(|| object.remove("files"))
                else {
                    return Err(GraphImpactContractError::InvalidDiffShape);
                };
                match entries {
                    serde_json::Value::Array(entries) => entries,
                    _ => return Err(GraphImpactContractError::InvalidDiffShape),
                }
            }
            _ => return Err(GraphImpactContractError::InvalidDiffShape),
        }
        .into_iter()
        .map(|entry| match entry {
            serde_json::Value::String(path) => Ok(path),
            _ => Err(GraphImpactContractError::InvalidDiffEntry),
        })
        .collect::<Result<Vec<_>, _>>()?
    } else {
        trimmed
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                let path = line
                    .rsplit_once('\t')
                    .map(|(_, path)| path)
                    .or_else(|| {
                        let mut fields = line.splitn(2, char::is_whitespace);
                        let first = fields.next()?;
                        let Some(remainder) = fields.next().map(str::trim) else {
                            return Some(line);
                        };
                        if first.len() <= 2
                            && first.chars().all(|character| {
                                matches!(character, 'A' | 'C' | 'D' | 'M' | 'R' | 'T' | 'U' | 'X')
                            })
                        {
                            Some(remainder)
                        } else {
                            Some(line)
                        }
                    })
                    .map(str::trim)
                    .filter(|path| !path.is_empty())?;
                Some(Ok(path.to_string()))
            })
            .collect::<Result<Vec<_>, GraphImpactContractError>>()?
    };

    let mut paths = BTreeSet::new();
    for path in raw_paths {
        if !is_safe_repo_relative(&path) {
            return Err(GraphImpactContractError::UnsafePath { path });
        }
        paths.insert(path);
    }
    Ok(paths.into_iter().collect())
}

/// Build a deterministic, path-only digest for the evaluated diff.
pub fn diff_digest(changed_files: &[String]) -> String {
    let mut paths = changed_files.to_vec();
    paths.sort();
    paths.dedup();
    digest_json(&paths)
}

/// Evaluate a bounded graph provider against an exact repository revision and
/// diff. The graph is advisory: only an exact, covered, untruncated result is
/// `complete`; every other outcome carries no impact paths so consumers cannot
/// mistake uncertainty for permission to reduce mandatory checks.
pub fn revision_bound_impact_report(
    repo_root: &Path,
    requested_revision: &str,
    resolved_revision: &str,
    changed_files: &[String],
    mode: GraphImpactMode,
    budget: usize,
    provider: &dyn GraphImpactProvider,
) -> Result<GraphImpactReport, GraphImpactContractError> {
    if !(1..=GRAPH_IMPACT_MAX_BUDGET).contains(&budget) {
        return Err(GraphImpactContractError::InvalidBudget);
    }

    let mut canonical_files = BTreeSet::new();
    for path in changed_files {
        if !is_safe_repo_relative(path) {
            return Err(GraphImpactContractError::UnsafePath { path: path.clone() });
        }
        canonical_files.insert(path.clone());
    }
    let changed_files = canonical_files.into_iter().collect::<Vec<_>>();
    let diff_digest = diff_digest(&changed_files);
    let max_nodes = budget.min(GRAPH_IMPACT_MAX_NODES);
    let max_results = budget.min(GRAPH_IMPACT_RESULT_LIMIT);

    let coverage = read_coverage(repo_root).unwrap_or_else(|error| {
        GraphCoverage::unavailable_with_gap(
            env!("CARGO_PKG_VERSION"),
            format!("coverage_artifact_invalid:{error}"),
        )
    });
    let coverage_available = coverage.available;
    let mut explanations = vec![format!(
        "evaluated requested revision {requested_revision:?} as resolved revision {resolved_revision}"
    )];

    let mut graph_revision = None;
    let mut graph_metadata_explanation = None;
    let store_path = repo_root.join(".aethyme/graph_store.redb");
    if store_path.is_file() {
        match GraphStore::open_read_only(repo_root) {
            Ok(store) => match store.repo_metadata() {
                Ok(metadata) => {
                    graph_revision = metadata.and_then(|metadata| metadata.commit_hash);
                }
                Err(error) => {
                    graph_metadata_explanation = Some(format!(
                        "graph repository metadata could not be read: {error}"
                    ));
                }
            },
            Err(error) => {
                graph_metadata_explanation = Some(format!(
                    "graph repository metadata could not be opened: {error}"
                ));
            }
        }
    }
    if graph_revision.is_none() {
        graph_revision = coverage.indexed_revision.clone();
    }

    let lookup = provider
        .lookup(&GraphImpactQuery {
            repo_root,
            changed_files: &changed_files,
            mode,
            max_results,
            max_depth: GRAPH_IMPACT_MAX_DEPTH,
            max_nodes,
        })
        .bounded(max_results);

    let coverage_missing_edge_kinds = missing_edge_kinds(&coverage, mode);
    let graph_revision_matches = graph_revision.as_deref() == Some(resolved_revision)
        && coverage.source_revision.as_deref() == Some(resolved_revision)
        && coverage.indexed_revision.as_deref() == Some(resolved_revision);
    let provider_ready = lookup.status == GraphImpactStatus::Ready;
    let mut status = match lookup.status {
        GraphImpactStatus::GraphStale => GraphImpactContractStatus::Stale,
        GraphImpactStatus::GraphMissing | GraphImpactStatus::ProviderError => {
            GraphImpactContractStatus::Unavailable
        }
        GraphImpactStatus::Ready if !coverage_available => GraphImpactContractStatus::Unavailable,
        GraphImpactStatus::Ready if !graph_revision_matches => GraphImpactContractStatus::Stale,
        GraphImpactStatus::Ready
            if lookup.truncated
                || !coverage.safe_to_use
                || !coverage_missing_edge_kinds.is_empty() =>
        {
            GraphImpactContractStatus::Partial
        }
        GraphImpactStatus::Ready => GraphImpactContractStatus::Complete,
    };

    if let Some(explanation) = graph_metadata_explanation {
        explanations.push(explanation);
        if provider_ready {
            status = GraphImpactContractStatus::Unavailable;
        }
    }
    explanations.push(format!(
        "provider {} returned {}: {}",
        provider.name(),
        lookup.status.as_str(),
        lookup.explanation
    ));
    if !coverage.available {
        explanations.push(
            "coverage is unavailable; an empty impact result is not evidence of zero impact".into(),
        );
    }
    if !coverage.gaps.is_empty() {
        explanations.push(format!("coverage gaps: {}", coverage.gaps.join(", ")));
    }
    if !coverage_missing_edge_kinds.is_empty() {
        explanations.push(format!(
            "requested {} edges are absent from coverage: {}",
            mode.as_str(),
            coverage_missing_edge_kinds.join(", ")
        ));
    }
    if !graph_revision_matches {
        explanations.push(format!(
            "graph and coverage are not both bound to resolved revision {resolved_revision}; impact paths are withheld"
        ));
    }
    if lookup.truncated {
        explanations.push(format!(
            "impact traversal reached a configured limit (budget {budget}, nodes {max_nodes}, results {max_results})"
        ));
    }

    let expose_impact = status == GraphImpactContractStatus::Complete
        || (status == GraphImpactContractStatus::Partial
            && provider_ready
            && graph_revision_matches
            && coverage_available);
    let impact = if expose_impact {
        impact_set(&lookup, &changed_files, mode)
    } else {
        GraphImpactSet::default()
    };
    let truncated = lookup.truncated || !coverage.gaps.is_empty();
    let coverage_projection = GraphImpactCoverage {
        mode: coverage.coverage_mode.clone(),
        languages: coverage.by_language.keys().cloned().collect(),
        parsed_files: coverage.files.parsed,
        excluded_files: coverage.files.excluded,
        unsupported_files: coverage.files.unsupported,
        missing_edge_kinds: coverage_missing_edge_kinds.clone(),
        truncated,
    };
    let risk_hints = risk_hints(&changed_files, &impact);
    let confidence = match status {
        GraphImpactContractStatus::Complete => GraphImpactConfidence::High,
        GraphImpactContractStatus::Partial
            if lookup.truncated
                || !coverage.gaps.is_empty()
                || !coverage_missing_edge_kinds.is_empty() =>
        {
            GraphImpactConfidence::Low
        }
        GraphImpactContractStatus::Partial => GraphImpactConfidence::Medium,
        GraphImpactContractStatus::Unavailable | GraphImpactContractStatus::Stale => {
            GraphImpactConfidence::Unknown
        }
    };
    match status {
        GraphImpactContractStatus::Complete => explanations.push(
            "the result is revision-bound, covered, and untruncated; empty impact is a complete empty result".into(),
        ),
        GraphImpactContractStatus::Partial => explanations.push(
            "the result is advisory only because coverage gaps or bounded traversal prevent a complete answer".into(),
        ),
        GraphImpactContractStatus::Unavailable => explanations.push(
            "the graph cannot answer this request; consumers must retain the conservative full verification path".into(),
        ),
        GraphImpactContractStatus::Stale => explanations.push(
            "the graph does not match the requested revision; consumers must retain the conservative full verification path".into(),
        ),
    }

    let repository = GraphImpactRepository {
        root: repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.to_path_buf())
            .display()
            .to_string(),
        revision: resolved_revision.to_string(),
    };
    let request = GraphImpactRequestSummary {
        changed_files: changed_files.len(),
        diff_digest,
        mode,
    };
    let limits = GraphImpactLimits {
        budget,
        max_nodes,
        max_depth: GRAPH_IMPACT_MAX_DEPTH,
        max_results,
        truncated,
    };
    let request_digest = digest_json(&serde_json::json!({
        "schema_version": GRAPH_IMPACT_CONTRACT_SCHEMA_VERSION,
        "revision": resolved_revision,
        "changed_files": changed_files,
        "diff_digest": request.diff_digest,
        "mode": mode.as_str(),
        "limits": limits,
    }));
    let provenance = GraphImpactProvenance {
        graph_revision,
        engine_version: coverage.engine_version.clone(),
        request_digest,
        result_digest: String::new(),
    };
    let mut report = GraphImpactReport {
        schema_version: GRAPH_IMPACT_CONTRACT_SCHEMA_VERSION,
        repository,
        request,
        status,
        confidence,
        coverage: coverage_projection,
        impact,
        risk_hints,
        limits,
        provenance,
        explanations,
    };
    report.provenance.result_digest = digest_json(&serde_json::json!({
        "schema_version": report.schema_version,
        "revision": report.repository.revision,
        "request_digest": report.provenance.request_digest,
        "status": report.status,
        "confidence": report.confidence,
        "coverage": report.coverage,
        "impact": report.impact,
        "risk_hints": report.risk_hints,
        "limits": report.limits,
        "explanations": report.explanations,
    }));
    Ok(report)
}

fn impact_set(
    lookup: &GraphImpactLookup,
    changed_files: &[String],
    mode: GraphImpactMode,
) -> GraphImpactSet {
    let mut direct = BTreeSet::new();
    let mut transitive = BTreeSet::new();
    for chain in &lookup.chains {
        if chain.depth <= 1 {
            direct.insert(chain.caller_file.clone());
        } else {
            transitive.insert(chain.caller_file.clone());
        }
    }
    let mut impact = GraphImpactSet {
        direct: direct.into_iter().collect(),
        transitive: transitive.into_iter().collect(),
        ..GraphImpactSet::default()
    };
    let all = lookup
        .impacted_paths
        .iter()
        .cloned()
        .chain(changed_files.iter().cloned())
        .collect::<BTreeSet<_>>();
    let impacted_paths = lookup
        .impacted_paths
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    match mode {
        GraphImpactMode::Calls => impact.callers = impacted_paths,
        GraphImpactMode::Imports => impact.importers = impacted_paths,
    }
    for path in all {
        if is_test_path(&path) {
            impact.tests.push(path.clone());
        }
        if is_config_path(&path) {
            impact.configs.push(path.clone());
        }
        if is_manifest_path(&path) {
            impact.manifests.push(path);
        }
    }
    impact.tests.sort();
    impact.configs.sort();
    impact.manifests.sort();
    impact
}

fn missing_edge_kinds(coverage: &GraphCoverage, mode: GraphImpactMode) -> Vec<String> {
    let wanted = mode.as_str();
    if coverage
        .edge_counts_by_kind
        .keys()
        .any(|kind| kind.eq_ignore_ascii_case(wanted))
    {
        Vec::new()
    } else {
        vec![wanted.to_string()]
    }
}

fn risk_hints(changed_files: &[String], impact: &GraphImpactSet) -> GraphImpactRiskHints {
    let paths = changed_files
        .iter()
        .chain(impact.direct.iter())
        .chain(impact.transitive.iter())
        .chain(impact.callers.iter())
        .chain(impact.importers.iter())
        .chain(impact.tests.iter())
        .chain(impact.configs.iter())
        .chain(impact.manifests.iter())
        .map(|path| path.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let security_surface = paths.iter().any(|path| {
        [
            "security",
            "auth",
            "crypto",
            "secret",
            "credential",
            "permission",
            "identity",
        ]
        .iter()
        .any(|token| path.contains(token))
    });
    let runtime_surface = paths.iter().any(|path| {
        matches!(
            Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str()),
            Some(
                "rs" | "py"
                    | "js"
                    | "jsx"
                    | "ts"
                    | "tsx"
                    | "go"
                    | "java"
                    | "kt"
                    | "c"
                    | "cc"
                    | "cpp"
                    | "h"
                    | "hpp"
            )
        )
    });
    let workspace_surface = paths.iter().any(|path| {
        path.contains("/workspace/")
            || path.starts_with("workspace/")
            || path.ends_with("workspace.toml")
            || is_manifest_path(path)
    });
    let global_config_surface = paths.iter().any(|path| {
        matches!(
            Path::new(path).file_name().and_then(|name| name.to_str()),
            Some(".env" | ".envrc" | "Makefile" | "Dockerfile" | "build.gradle" | "pom.xml")
        ) || path.starts_with(".github/")
            || path.starts_with(".aethyme/")
            || path.contains("/config/")
    });
    GraphImpactRiskHints {
        security_surface,
        runtime_surface,
        workspace_surface,
        global_config_surface,
    }
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower
        .split('/')
        .any(|part| matches!(part, "test" | "tests" | "spec" | "specs" | "__tests__"))
        || lower.ends_with("_test.rs")
        || lower.ends_with("_test.py")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".test.js")
        || lower.ends_with(".spec.ts")
        || lower.ends_with(".spec.js")
}

fn is_config_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with(".github/")
        || lower.starts_with(".aethyme/")
        || lower.contains("/config/")
        || lower
            .split('/')
            .any(|part| part == "config" || part == "configs")
        || Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(extension, "toml" | "yaml" | "yml" | "json" | "ini" | "env")
            })
}

fn is_manifest_path(path: &str) -> bool {
    matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()),
        Some(
            "Cargo.toml"
                | "Cargo.lock"
                | "package.json"
                | "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
                | "pyproject.toml"
                | "poetry.lock"
                | "go.mod"
                | "go.sum"
                | "pom.xml"
                | "build.gradle"
                | "settings.gradle"
        )
    ) || path.to_ascii_lowercase().contains("workspace")
}

fn digest_json<T: serde::Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("impact contract digest inputs serialize");
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Inputs available to an advisory graph-impact provider.
#[derive(Debug, Clone, Copy)]
pub struct GraphImpactQuery<'a> {
    pub repo_root: &'a Path,
    pub changed_files: &'a [String],
    pub mode: GraphImpactMode,
    pub max_results: usize,
    pub max_depth: usize,
    pub max_nodes: usize,
}

/// One provider-proven path from a changed file to an external caller file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphImpactChain {
    pub changed_file: String,
    pub caller_file: String,
    pub depth: usize,
}

/// Availability/result state returned by a graph-impact provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphImpactStatus {
    Ready,
    GraphMissing,
    GraphStale,
    ProviderError,
}

impl GraphImpactStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::GraphMissing => "graph_missing",
            Self::GraphStale => "graph_stale",
            Self::ProviderError => "provider_error",
        }
    }
}

/// Provider output before the broker derives advisory gate suggestions.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphImpactLookup {
    /// Relationship that actually served this lookup, including degraded
    /// outcomes so callers can explain which mode was requested.
    pub mode: GraphImpactMode,
    pub status: GraphImpactStatus,
    pub impacted_paths: Vec<String>,
    pub chains: Vec<GraphImpactChain>,
    pub visited_nodes: usize,
    pub truncated: bool,
    pub explanation: String,
}

impl GraphImpactLookup {
    pub fn ready(
        impacted_paths: Vec<String>,
        truncated: bool,
        explanation: impl Into<String>,
    ) -> Self {
        Self {
            mode: GraphImpactMode::Calls,
            status: GraphImpactStatus::Ready,
            impacted_paths,
            chains: Vec::new(),
            visited_nodes: 0,
            truncated,
            explanation: explanation.into(),
        }
    }

    pub fn ready_with_chains(
        chains: Vec<GraphImpactChain>,
        visited_nodes: usize,
        truncated: bool,
        explanation: impl Into<String>,
    ) -> Self {
        let impacted_paths = chains
            .iter()
            .map(|chain| chain.caller_file.clone())
            .collect();
        Self {
            mode: GraphImpactMode::Calls,
            status: GraphImpactStatus::Ready,
            impacted_paths,
            chains,
            visited_nodes,
            truncated,
            explanation: explanation.into(),
        }
    }

    pub fn graph_missing(explanation: impl Into<String>) -> Self {
        Self::degraded(GraphImpactStatus::GraphMissing, explanation)
    }

    pub fn graph_stale(explanation: impl Into<String>) -> Self {
        Self::degraded(GraphImpactStatus::GraphStale, explanation)
    }

    pub fn provider_error(explanation: impl Into<String>) -> Self {
        Self::degraded(GraphImpactStatus::ProviderError, explanation)
    }

    fn degraded(status: GraphImpactStatus, explanation: impl Into<String>) -> Self {
        Self {
            mode: GraphImpactMode::Calls,
            status,
            impacted_paths: Vec::new(),
            chains: Vec::new(),
            visited_nodes: 0,
            truncated: false,
            explanation: explanation.into(),
        }
    }

    /// Record the relationship mode requested by the caller. The basic
    /// constructors retain their historical Calls default for custom test
    /// providers and other API users that return a synthetic lookup.
    pub fn with_mode(mut self, mode: GraphImpactMode) -> Self {
        self.mode = mode;
        self
    }

    /// Enforce the broker-side result contract even when a provider returns
    /// duplicates, unsafe paths, or more entries than requested. First-seen
    /// order is provider ranking and is preserved.
    pub(crate) fn bounded(mut self, limit: usize) -> Self {
        if self.status != GraphImpactStatus::Ready {
            self.impacted_paths.clear();
            self.chains.clear();
            self.visited_nodes = 0;
            self.truncated = false;
            return self;
        }

        let original_len = self.impacted_paths.len();
        let mut seen = HashSet::new();
        self.impacted_paths
            .retain(|path| is_safe_repo_relative(path) && seen.insert(path.clone()));
        if self.impacted_paths.len() > limit {
            self.impacted_paths.truncate(limit);
        }
        self.truncated |= original_len > self.impacted_paths.len();

        let retained_paths = self.impacted_paths.iter().cloned().collect::<HashSet<_>>();
        let original_chain_len = self.chains.len();
        let mut seen_callers = HashSet::new();
        self.chains.retain(|chain| {
            is_safe_repo_relative(&chain.changed_file)
                && retained_paths.contains(&chain.caller_file)
                && seen_callers.insert(chain.caller_file.clone())
        });
        self.truncated |= original_chain_len > self.chains.len();
        self
    }
}

/// Read-only source of semantic impact paths. Implementations must not mutate
/// graph or broker state and must translate expected graph failures into a
/// [`GraphImpactLookup`] outcome.
pub trait GraphImpactProvider: Send + Sync {
    fn name(&self) -> &str;
    fn lookup(&self, query: &GraphImpactQuery<'_>) -> GraphImpactLookup;
}

/// Read-only redb provider for bounded incoming graph frontiers.
#[derive(Debug, Default)]
pub struct GraphStoreImpactProvider;

impl GraphImpactProvider for GraphStoreImpactProvider {
    fn name(&self) -> &str {
        "caller_frontier"
    }

    fn lookup(&self, query: &GraphImpactQuery<'_>) -> GraphImpactLookup {
        let store_path = query.repo_root.join(".aethyme/graph_store.redb");
        let fragments_path = query.repo_root.join(".aethyme/graph");
        if !store_path.is_file() {
            return GraphImpactLookup::graph_missing(
                "no .aethyme/graph_store.redb found; semantic suggestions are unavailable",
            )
            .with_mode(query.mode);
        }

        let store_modified = match modified(&store_path) {
            Ok(modified) => modified,
            Err(error) => {
                return GraphImpactLookup::provider_error(format!(
                    "could not inspect .aethyme/graph_store.redb freshness: {error}"
                ))
                .with_mode(query.mode);
            }
        };
        let newest_fragment = match newest_modified(&fragments_path) {
            Ok(modified) => modified,
            Err(error) => {
                return GraphImpactLookup::provider_error(format!(
                    "could not inspect .aethyme/graph fragments: {error}"
                ))
                .with_mode(query.mode);
            }
        };
        if newest_fragment.is_some_and(|fragment| fragment > store_modified) {
            return GraphImpactLookup::graph_stale(
                ".aethyme/graph contains fragments newer than graph_store.redb; rebuild the graph before using semantic suggestions",
            )
            .with_mode(query.mode);
        }

        let store = match GraphStore::open_read_only(query.repo_root) {
            Ok(store) => store,
            Err(error) => {
                return GraphImpactLookup::provider_error(format!(
                    "could not open graph_store.redb for impact lookup: {error}"
                ))
                .with_mode(query.mode);
            }
        };
        let lookup = match query.mode {
            GraphImpactMode::Calls => caller_frontier(&store, query),
            GraphImpactMode::Imports => import_frontier(&store, query),
        };
        match lookup {
            Ok(lookup) => lookup,
            Err(error) => GraphImpactLookup::provider_error(format!(
                "{} frontier lookup failed: {error}",
                query.mode.as_str()
            ))
            .with_mode(query.mode),
        }
    }
}

#[derive(Debug)]
struct FrontierNode {
    changed_file: String,
    node_id: String,
    depth: usize,
}

fn caller_frontier(
    store: &ReadOnlyGraphStore,
    query: &GraphImpactQuery<'_>,
) -> Result<GraphImpactLookup, GraphStoreError> {
    let mut changed_files = query.changed_files.to_vec();
    changed_files.sort();
    changed_files.dedup();
    let changed_set = changed_files.iter().cloned().collect::<BTreeSet<_>>();

    let mut queue = VecDeque::new();
    let mut visited = BTreeSet::new();
    let mut truncated = false;
    for changed_file in &changed_files {
        let remaining = query.max_nodes.saturating_sub(visited.len());
        let seeds = store.function_ids_for_path(changed_file, remaining)?;
        truncated |= seeds.truncated;
        for node_id in seeds.ids {
            if visited.insert(node_id.clone()) {
                queue.push_back(FrontierNode {
                    changed_file: changed_file.clone(),
                    node_id,
                    depth: 0,
                });
            }
        }
    }
    let seed_count = visited.len();

    let mut chains = Vec::new();
    let mut caller_files = HashSet::new();
    'walk: while let Some(current) = queue.pop_front() {
        let callers = callable_callers(store, &current.node_id)?;
        if current.depth >= query.max_depth {
            if callers
                .iter()
                .any(|(caller_id, _)| !visited.contains(caller_id))
            {
                truncated = true;
            }
            continue;
        }

        for (caller_id, caller_file) in callers {
            if visited.contains(&caller_id) {
                continue;
            }
            if visited.len() == query.max_nodes {
                truncated = true;
                break 'walk;
            }
            visited.insert(caller_id.clone());
            let depth = current.depth + 1;
            queue.push_back(FrontierNode {
                changed_file: current.changed_file.clone(),
                node_id: caller_id,
                depth,
            });

            if changed_set.contains(&caller_file) || !caller_files.insert(caller_file.clone()) {
                continue;
            }
            if chains.len() == query.max_results {
                truncated = true;
                break 'walk;
            }
            chains.push(GraphImpactChain {
                changed_file: current.changed_file.clone(),
                caller_file,
                depth,
            });
        }
    }

    let explanation = format!(
        "walked incoming Calls edges from {seed_count} changed-file callable(s); returned {} caller file(s) with depth <= {} and nodes <= {}",
        chains.len(),
        query.max_depth,
        query.max_nodes
    );
    Ok(
        GraphImpactLookup::ready_with_chains(chains, visited.len(), truncated, explanation)
            .with_mode(GraphImpactMode::Calls),
    )
}

fn import_frontier(
    store: &ReadOnlyGraphStore,
    query: &GraphImpactQuery<'_>,
) -> Result<GraphImpactLookup, GraphStoreError> {
    let mut changed_files = query.changed_files.to_vec();
    changed_files.sort();
    changed_files.dedup();
    let changed_set = changed_files.iter().cloned().collect::<BTreeSet<_>>();

    let mut queue = VecDeque::new();
    let mut visited = BTreeSet::new();
    let mut truncated = false;
    for changed_file in &changed_files {
        if visited.len() == query.max_nodes {
            truncated = true;
            break;
        }
        let Some(file) = store.resolve_file_path(changed_file)? else {
            continue;
        };
        if visited.insert(file.id.clone()) {
            queue.push_back(FrontierNode {
                changed_file: changed_file.clone(),
                node_id: file.id,
                depth: 0,
            });
        }
    }
    let seed_count = visited.len();

    let mut chains = Vec::new();
    let mut importer_files = HashSet::new();
    'walk: while let Some(current) = queue.pop_front() {
        let importers = file_importers(store, &current.node_id)?;
        if current.depth >= query.max_depth {
            if importers
                .iter()
                .any(|(importer_id, _)| !visited.contains(importer_id))
            {
                truncated = true;
            }
            continue;
        }

        for (importer_id, importer_file) in importers {
            if visited.contains(&importer_id) {
                continue;
            }
            if visited.len() == query.max_nodes {
                truncated = true;
                break 'walk;
            }
            visited.insert(importer_id.clone());
            let depth = current.depth + 1;
            queue.push_back(FrontierNode {
                changed_file: current.changed_file.clone(),
                node_id: importer_id,
                depth,
            });

            if changed_set.contains(&importer_file) || !importer_files.insert(importer_file.clone())
            {
                continue;
            }
            if chains.len() == query.max_results {
                truncated = true;
                break 'walk;
            }
            chains.push(GraphImpactChain {
                changed_file: current.changed_file.clone(),
                caller_file: importer_file,
                depth,
            });
        }
    }

    let explanation = format!(
        "walked incoming Imports edges from {seed_count} changed-file file(s); returned {} importer file(s) with depth <= {} and nodes <= {}",
        chains.len(),
        query.max_depth,
        query.max_nodes
    );
    Ok(
        GraphImpactLookup::ready_with_chains(chains, visited.len(), truncated, explanation)
            .with_mode(GraphImpactMode::Imports),
    )
}

fn callable_callers(
    store: &ReadOnlyGraphStore,
    node_id: &str,
) -> Result<Vec<(String, String)>, GraphStoreError> {
    let mut adjacency =
        store.neighbors(node_id, NeighborDirection::Incoming, Some(EdgeKind::Calls))?;
    adjacency.sort_by(|left, right| {
        left.other
            .as_str()
            .cmp(right.other.as_str())
            .then_with(|| left.source.as_str().cmp(right.source.as_str()))
            .then_with(|| left.confidence.cmp(&right.confidence))
    });

    let mut callers = Vec::new();
    for edge in adjacency {
        let caller_id = edge.other.as_str();
        let Some(display) = store.node_display(caller_id)? else {
            continue;
        };
        if display.kind != StoredNodeKind::Function {
            continue;
        }
        let Some(path) = display.path else {
            continue;
        };
        callers.push((caller_id.to_string(), path));
    }
    callers.dedup();
    Ok(callers)
}

fn file_importers(
    store: &ReadOnlyGraphStore,
    node_id: &str,
) -> Result<Vec<(String, String)>, GraphStoreError> {
    let mut adjacency = store.neighbors(
        node_id,
        NeighborDirection::Incoming,
        Some(EdgeKind::Imports),
    )?;
    adjacency.sort_by(|left, right| {
        left.other
            .as_str()
            .cmp(right.other.as_str())
            .then_with(|| left.source.as_str().cmp(right.source.as_str()))
            .then_with(|| left.confidence.cmp(&right.confidence))
    });

    let mut importers = Vec::new();
    for edge in adjacency {
        let importer_id = edge.other.as_str();
        let Some(display) = store.node_display(importer_id)? else {
            continue;
        };
        if display.kind != StoredNodeKind::File {
            continue;
        }
        let Some(path) = display.path else {
            continue;
        };
        importers.push((importer_id.to_string(), path));
    }
    importers.dedup();
    Ok(importers)
}

fn modified(path: &Path) -> std::io::Result<SystemTime> {
    std::fs::metadata(path)?.modified()
}

fn newest_modified(root: &Path) -> std::io::Result<Option<SystemTime>> {
    if !root.exists() {
        return Ok(None);
    }

    let mut newest = Some(modified(root)?);
    let mut stack = vec![PathBuf::from(root)];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                newest = newest.max(Some(modified(&path)?));
            }
        }
    }
    Ok(newest)
}

fn is_safe_repo_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(root: &Path) -> GraphImpactQuery<'_> {
        GraphImpactQuery {
            repo_root: root,
            changed_files: &[],
            mode: GraphImpactMode::Calls,
            max_results: GRAPH_IMPACT_RESULT_LIMIT,
            max_depth: GRAPH_IMPACT_MAX_DEPTH,
            max_nodes: GRAPH_IMPACT_MAX_NODES,
        }
    }

    #[test]
    fn bounded_results_preserve_first_seen_order_and_mark_omissions() {
        let lookup = GraphImpactLookup::ready(
            vec![
                "src/first.rs".into(),
                "../outside.rs".into(),
                "src/first.rs".into(),
                "src/second.rs".into(),
                "src/third.rs".into(),
            ],
            false,
            "fixture",
        )
        .bounded(2);

        assert_eq!(
            lookup.impacted_paths,
            vec!["src/first.rs".to_string(), "src/second.rs".to_string()]
        );
        assert!(lookup.truncated);
    }

    #[test]
    fn degraded_results_never_carry_provider_paths() {
        let mut lookup = GraphImpactLookup::graph_stale("stale fixture");
        lookup.impacted_paths.push("src/unsafe.rs".into());
        lookup.truncated = true;

        let bounded = lookup.bounded(GRAPH_IMPACT_RESULT_LIMIT);
        assert!(bounded.impacted_paths.is_empty());
        assert!(!bounded.truncated);
    }

    #[test]
    fn default_provider_reports_a_cold_graph() {
        let missing = tempfile::tempdir().unwrap();
        let provider = GraphStoreImpactProvider;
        assert_eq!(
            provider.lookup(&query(missing.path())).status,
            GraphImpactStatus::GraphMissing
        );
    }

    #[test]
    fn default_provider_reports_an_empty_warm_graph() {
        let root = tempfile::tempdir().unwrap();
        drop(GraphStore::open(root.path()).unwrap());
        let changed_files = vec!["src/empty.rs".to_string()];
        let lookup = GraphStoreImpactProvider.lookup(&GraphImpactQuery {
            changed_files: &changed_files,
            ..query(root.path())
        });

        assert_eq!(lookup.status, GraphImpactStatus::Ready);
        assert!(lookup.impacted_paths.is_empty());
        assert!(lookup.chains.is_empty());
        assert_eq!(lookup.visited_nodes, 0);
        assert!(!lookup.truncated);
    }

    #[test]
    fn default_provider_reports_a_stale_graph() {
        let stale = tempfile::tempdir().unwrap();
        let stale_aethyme = stale.path().join(".aethyme");
        std::fs::create_dir_all(stale_aethyme.join("graph")).unwrap();
        drop(GraphStore::open(stale.path()).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(stale_aethyme.join("graph/newer.bin"), "fixture").unwrap();
        assert_eq!(
            GraphStoreImpactProvider.lookup(&query(stale.path())).status,
            GraphImpactStatus::GraphStale
        );
    }

    #[test]
    fn default_provider_reports_a_corrupted_graph_without_failing_the_broker() {
        let corrupted = tempfile::tempdir().unwrap();
        let aethyme = corrupted.path().join(".aethyme");
        std::fs::create_dir_all(&aethyme).unwrap();
        std::fs::write(aethyme.join("graph_store.redb"), b"not a redb database").unwrap();

        let lookup = GraphStoreImpactProvider.lookup(&query(corrupted.path()));
        assert_eq!(lookup.status, GraphImpactStatus::ProviderError);
        assert!(lookup.impacted_paths.is_empty());
        assert!(lookup.chains.is_empty());
        assert!(
            lookup
                .explanation
                .contains("could not open graph_store.redb")
        );
    }

    #[test]
    fn warm_graph_walks_a_deterministic_bounded_incoming_calls_frontier() {
        use aethyme_engine::model::edge::Edge;
        use aethyme_engine::model::file::{FileNode, FileRole};
        use aethyme_engine::model::function::FunctionNode;
        use aethyme_engine::model::intern::InternedStr;
        use aethyme_engine::store::redb::graph_store::{insert_edge, insert_file, insert_function};

        fn file(path: &str) -> FileNode {
            FileNode::new(
                "Repo",
                path,
                Some("rust".into()),
                FileRole::Source,
                10,
                100,
                false,
                None,
            )
        }

        fn function(file: &FileNode, name: &str) -> FunctionNode {
            FunctionNode::new(
                "Repo",
                InternedStr::from(file.id.clone()),
                InternedStr::from(file.path.clone()),
                None,
                None,
                InternedStr::from("rust"),
                InternedStr::from(name),
                1,
                InternedStr::from(format!("fn {name}()")),
            )
        }

        let root = tempfile::tempdir().unwrap();
        let store = GraphStore::open(root.path()).unwrap();
        let changed_file = file("src/core.rs");
        let adapter_file = file("src/adapter.rs");
        let caller_file = file("src/service.rs");
        let outer_file = file("src/api.rs");
        let beyond_file = file("src/bin.rs");
        let changed = function(&changed_file, "changed");
        let adapter = function(&adapter_file, "adapter");
        let caller = function(&caller_file, "caller");
        let outer = function(&outer_file, "outer");
        let beyond = function(&beyond_file, "beyond");
        let mut session = store.begin_index().unwrap();
        for file in [
            &changed_file,
            &adapter_file,
            &caller_file,
            &outer_file,
            &beyond_file,
        ] {
            insert_file(&mut session, file).unwrap();
        }
        for function in [&changed, &caller, &outer, &beyond, &adapter] {
            insert_function(&mut session, function).unwrap();
        }
        for (from, to) in [
            (adapter.id.as_str(), changed.id.as_str()),
            (caller.id.as_str(), changed.id.as_str()),
            (outer.id.as_str(), caller.id.as_str()),
            (beyond.id.as_str(), outer.id.as_str()),
        ] {
            insert_edge(
                &mut session,
                &Edge::new(from, to, EdgeKind::Calls, 1000, "test"),
            )
            .unwrap();
        }
        session.commit().unwrap();
        drop(store);

        let changed_files = vec![changed_file.path.clone()];
        let query = GraphImpactQuery {
            repo_root: root.path(),
            changed_files: &changed_files,
            mode: GraphImpactMode::Calls,
            max_results: GRAPH_IMPACT_RESULT_LIMIT,
            max_depth: 2,
            max_nodes: GRAPH_IMPACT_MAX_NODES,
        };
        let provider = GraphStoreImpactProvider;
        let first = provider.lookup(&query);
        let second = provider.lookup(&query);

        assert_eq!(first, second, "caller ordering must be deterministic");
        assert_eq!(first.mode, GraphImpactMode::Calls);
        assert_eq!(first.status, GraphImpactStatus::Ready);
        assert_eq!(
            first.impacted_paths,
            vec![
                "src/adapter.rs".to_string(),
                "src/service.rs".to_string(),
                "src/api.rs".to_string()
            ]
        );
        assert_eq!(
            first.chains,
            vec![
                GraphImpactChain {
                    changed_file: "src/core.rs".into(),
                    caller_file: "src/adapter.rs".into(),
                    depth: 1,
                },
                GraphImpactChain {
                    changed_file: "src/core.rs".into(),
                    caller_file: "src/service.rs".into(),
                    depth: 1,
                },
                GraphImpactChain {
                    changed_file: "src/core.rs".into(),
                    caller_file: "src/api.rs".into(),
                    depth: 2,
                },
            ]
        );
        assert_eq!(first.visited_nodes, 4);
        assert!(
            first.truncated,
            "the depth-three caller must mark truncation"
        );

        let node_limited = provider.lookup(&GraphImpactQuery {
            max_nodes: 2,
            ..query
        });
        assert_eq!(node_limited.impacted_paths, vec!["src/adapter.rs"]);
        assert_eq!(node_limited.visited_nodes, 2);
        assert!(node_limited.truncated);
    }

    #[test]
    fn warm_graph_compares_calls_and_imports_frontiers_and_reports_mode() {
        use aethyme_engine::model::edge::Edge;
        use aethyme_engine::model::file::{FileNode, FileRole};
        use aethyme_engine::store::redb::graph_store::{insert_edge, insert_file};

        fn file(path: &str) -> FileNode {
            FileNode::new(
                "Repo",
                path,
                Some("rust".into()),
                FileRole::Source,
                10,
                100,
                false,
                None,
            )
        }

        let root = tempfile::tempdir().unwrap();
        let store = GraphStore::open(root.path()).unwrap();
        let changed_file = file("src/core.rs");
        let adapter_file = file("src/adapter.rs");
        let caller_file = file("src/service.rs");
        let outer_file = file("src/api.rs");
        let beyond_file = file("src/bin.rs");
        let mut session = store.begin_index().unwrap();
        for file in [
            &changed_file,
            &adapter_file,
            &caller_file,
            &outer_file,
            &beyond_file,
        ] {
            insert_file(&mut session, file).unwrap();
        }
        for (from, to) in [
            (adapter_file.id.as_str(), changed_file.id.as_str()),
            (caller_file.id.as_str(), changed_file.id.as_str()),
            (outer_file.id.as_str(), caller_file.id.as_str()),
            (beyond_file.id.as_str(), outer_file.id.as_str()),
        ] {
            insert_edge(
                &mut session,
                &Edge::new(from, to, EdgeKind::Imports, 1000, "test"),
            )
            .unwrap();
        }
        session.commit().unwrap();
        drop(store);

        let changed_files = vec![changed_file.path.clone()];
        let provider = GraphStoreImpactProvider;
        let calls = provider.lookup(&GraphImpactQuery {
            repo_root: root.path(),
            changed_files: &changed_files,
            mode: GraphImpactMode::Calls,
            max_results: GRAPH_IMPACT_RESULT_LIMIT,
            max_depth: 2,
            max_nodes: GRAPH_IMPACT_MAX_NODES,
        });
        let imports = provider.lookup(&GraphImpactQuery {
            mode: GraphImpactMode::Imports,
            ..GraphImpactQuery {
                repo_root: root.path(),
                changed_files: &changed_files,
                mode: GraphImpactMode::Calls,
                max_results: GRAPH_IMPACT_RESULT_LIMIT,
                max_depth: 2,
                max_nodes: GRAPH_IMPACT_MAX_NODES,
            }
        });

        assert_eq!(calls.status, GraphImpactStatus::Ready);
        assert_eq!(calls.mode, GraphImpactMode::Calls);
        assert!(calls.impacted_paths.is_empty());
        assert_eq!(imports.status, GraphImpactStatus::Ready);
        assert_eq!(imports.mode, GraphImpactMode::Imports);
        assert_eq!(
            imports.impacted_paths,
            vec![
                "src/adapter.rs".to_string(),
                "src/service.rs".to_string(),
                "src/api.rs".to_string()
            ]
        );
        assert_eq!(
            imports.chains,
            vec![
                GraphImpactChain {
                    changed_file: "src/core.rs".into(),
                    caller_file: "src/adapter.rs".into(),
                    depth: 1,
                },
                GraphImpactChain {
                    changed_file: "src/core.rs".into(),
                    caller_file: "src/service.rs".into(),
                    depth: 1,
                },
                GraphImpactChain {
                    changed_file: "src/core.rs".into(),
                    caller_file: "src/api.rs".into(),
                    depth: 2,
                },
            ]
        );
        assert!(
            imports.truncated,
            "the depth-three importer must mark truncation"
        );
        assert!(imports.explanation.contains("incoming Imports edges"));
    }

    #[test]
    fn diff_parser_accepts_name_status_and_structured_inputs() {
        assert_eq!(
            parse_diff_text("M\tsrc/app.rs\n\nsrc/app.rs\ntests/app.rs\n").unwrap(),
            vec!["src/app.rs".to_string(), "tests/app.rs".to_string()]
        );
        assert_eq!(
            parse_diff_text(r#"{"changed_files":["Cargo.toml","src/lib.rs"]}"#).unwrap(),
            vec!["Cargo.toml".to_string(), "src/lib.rs".to_string()]
        );
        assert!(matches!(
            parse_diff_text("../outside.rs"),
            Err(GraphImpactContractError::UnsafePath { .. })
        ));
    }

    #[derive(Clone)]
    struct FixedImpactProvider {
        lookup: GraphImpactLookup,
    }

    impl GraphImpactProvider for FixedImpactProvider {
        fn name(&self) -> &str {
            "fixed-fixture"
        }

        fn lookup(&self, _query: &GraphImpactQuery<'_>) -> GraphImpactLookup {
            self.lookup.clone()
        }
    }

    fn covered_root(revision: &str, edge_kind: &str) -> tempfile::TempDir {
        use aethyme_engine::store::redb::graph_store::{GraphStore, RepoMetadata};
        use aethyme_graph_storage::write_coverage_artifacts;

        let root = tempfile::tempdir().unwrap();
        let store = GraphStore::open(root.path()).unwrap();
        store
            .set_repo_metadata(&RepoMetadata {
                root_path: root.path().display().to_string(),
                commit_hash: Some(revision.into()),
                indexed_at_unix: 0,
                file_count: 1,
                languages: vec!["rust".into()],
            })
            .unwrap();
        let mut coverage = GraphCoverage::unavailable("fixture-engine");
        coverage.available = true;
        coverage.source_revision = Some(revision.into());
        coverage.indexed_revision = Some(revision.into());
        coverage.source_tree_sha256 = Some("tree".into());
        coverage.indexed_tree_sha256 = Some("tree".into());
        coverage.coverage_mode = "complete".into();
        coverage.safe_to_use = true;
        coverage.gaps.clear();
        coverage.files.parsed = 1;
        coverage
            .by_language
            .insert("rust".into(), Default::default());
        coverage.edge_counts_by_kind.insert(edge_kind.into(), 1);
        write_coverage_artifacts(root.path(), &coverage, &[]).unwrap();
        drop(store);
        root
    }

    #[test]
    fn revision_contract_distinguishes_complete_empty_from_unavailable() {
        let root = covered_root("abc", "Calls");
        let report = revision_bound_impact_report(
            root.path(),
            "abc",
            "abc",
            &["src/leaf.rs".into()],
            GraphImpactMode::Calls,
            8,
            &GraphStoreImpactProvider,
        )
        .unwrap();
        assert_eq!(report.status, GraphImpactContractStatus::Complete);
        assert_eq!(report.confidence, GraphImpactConfidence::High);
        assert!(report.impact.callers.is_empty());
        assert!(
            report
                .explanations
                .iter()
                .any(|line| line.contains("complete empty"))
        );

        let missing = tempfile::tempdir().unwrap();
        let unavailable = revision_bound_impact_report(
            missing.path(),
            "abc",
            "abc",
            &["src/leaf.rs".into()],
            GraphImpactMode::Calls,
            8,
            &GraphStoreImpactProvider,
        )
        .unwrap();
        assert_eq!(unavailable.status, GraphImpactContractStatus::Unavailable);
        assert!(unavailable.impact.callers.is_empty());
    }

    #[test]
    fn revision_contract_withholds_stale_paths_and_marks_truncation() {
        let stale_root = covered_root("old", "Calls");
        let stale = revision_bound_impact_report(
            stale_root.path(),
            "new",
            "new",
            &["src/core.rs".into()],
            GraphImpactMode::Calls,
            8,
            &FixedImpactProvider {
                lookup: GraphImpactLookup::ready(vec!["src/service.rs".into()], false, "fixture"),
            },
        )
        .unwrap();
        assert_eq!(stale.status, GraphImpactContractStatus::Stale);
        assert!(stale.impact.callers.is_empty());

        let root = covered_root("abc", "Calls");
        let truncated = revision_bound_impact_report(
            root.path(),
            "abc",
            "abc",
            &["src/core.rs".into()],
            GraphImpactMode::Calls,
            2,
            &FixedImpactProvider {
                lookup: GraphImpactLookup::ready(
                    vec![
                        "tests/core.rs".into(),
                        "src/service.rs".into(),
                        "src/third.rs".into(),
                    ],
                    false,
                    "fixture",
                ),
            },
        )
        .unwrap();
        assert_eq!(truncated.status, GraphImpactContractStatus::Partial);
        assert_eq!(truncated.confidence, GraphImpactConfidence::Low);
        assert!(truncated.limits.truncated);
        assert_eq!(
            truncated.impact.callers,
            vec!["src/service.rs", "tests/core.rs"]
        );
        assert_eq!(truncated.impact.tests, vec!["tests/core.rs"]);
    }
}
