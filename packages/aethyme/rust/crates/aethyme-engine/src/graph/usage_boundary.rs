use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// Phase 5 decision: usage-boundary stays hybrid V2. redb owns bounded seed
// discovery (public symbols plus candidate source/docs/config files), while
// query-time source text owns evidence strings. Persisting evidence spans in
// redb would require explicit freshness/invalidation semantics before it could
// be trusted for removal decisions.

use crate::graph::facts::classify_exposure;
use crate::model::analysis::{
    AnswerStatus, DeadCodeAnswer, DeadCodeCandidate, DeadCodeConfidenceSummary, DeadCodeFactCounts,
    DeadCodeGraphCounts, DeadCodeObservability, DeadCodeQuery, DeadCodeSummary, EvidencePacket,
    ExposureKind, FunctionFact,
};
use crate::model::class::ClassNode;
use crate::model::edge::EdgeKind;
use crate::model::file::{FileNode, FileRole};
use crate::model::function::FunctionNode;
use crate::store::redb::graph_store::{NeighborDirection, ReadOnlyGraphStore, StoredNode};

const SOURCE_EXTENSIONS: &[&str] = &["php"];
const DOC_EXTENSIONS: &[&str] = &["md", "markdown", "rst", "txt"];
const CONFIG_EXTENSIONS: &[&str] = &["json", "yaml", "yml", "toml", "ini", "xml"];
const DEFAULT_EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".aethyme",
    ".venv",
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    "tests",
    "test",
    "spec",
];

#[derive(Debug, Clone)]
struct ScopedSymbol {
    fact: FunctionFact,
    class_kind: Option<String>,
}

#[derive(Debug, Default, Clone)]
struct UsageEvidence {
    internal_callers: BTreeSet<String>,
    external_callers: BTreeSet<String>,
    docs_config_references: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct ScanStats {
    source_files: usize,
    docs: usize,
    configs: usize,
}

#[derive(Debug)]
struct ScanBudget {
    started_at: Instant,
    max_duration: Option<Duration>,
}

impl ScanBudget {
    fn new(budget_ms: Option<u64>) -> Self {
        Self {
            started_at: Instant::now(),
            max_duration: budget_ms.map(Duration::from_millis),
        }
    }

    fn expired(&self) -> bool {
        self.max_duration
            .is_some_and(|max_duration| self.started_at.elapsed() >= max_duration)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanZone {
    Internal,
    External,
}

#[derive(Debug, Default)]
struct UsageBoundarySeedPlan {
    symbols: Vec<ScopedSymbol>,
    internal_source_files: Vec<PathBuf>,
    external_source_files: Vec<PathBuf>,
    docs_config_files: Vec<PathBuf>,
    stats: ScanStats,
}

pub fn analyze_usage_boundary_scope_first(
    repo: &Path,
    scope: &str,
    searched_roots: &[String],
    include_methods: bool,
    budget_ms: Option<u64>,
    max_evidence_per_symbol: usize,
) -> Result<DeadCodeAnswer, String> {
    let canonical_repo = repo
        .canonicalize()
        .map_err(|error| format!("failed to resolve repo path {}: {error}", repo.display()))?;
    let scope = normalize_relative_path(scope);
    if scope.is_empty() {
        return Err("scope must be a non-empty repository-relative path".to_string());
    }
    let scope_path = canonical_repo.join(&scope);
    if !scope_path.exists() {
        return Err(format!("scope does not exist: {scope}"));
    }

    let roots = normalize_roots(searched_roots);
    let budget = ScanBudget::new(budget_ms);
    let evidence_limit = max_evidence_per_symbol.max(1);
    let mut degraded_reasons = vec![
        "scope_first_strategy".to_string(),
        "language_support_limited_to_php_for_scope_first_scan".to_string(),
    ];
    let mut stats = ScanStats::default();

    let scope_files = collect_files(
        &canonical_repo,
        &scope_path,
        FileSet::Source,
        Some(&scope),
        &budget,
        &mut stats,
        &mut degraded_reasons,
    )?;
    let symbols = collect_scope_symbols(
        &canonical_repo,
        &scope,
        &scope_files,
        include_methods,
        &budget,
        &mut degraded_reasons,
    );
    let mut usage = vec![UsageEvidence::default(); symbols.len()];

    if !symbols.is_empty() && !budget.expired() {
        scan_source_files(
            &canonical_repo,
            &scope,
            &scope_files,
            &symbols,
            &mut usage,
            ScanZone::Internal,
            evidence_limit,
            &budget,
            &mut degraded_reasons,
        );
    }

    if !symbols.is_empty() && !budget.expired() {
        let external_roots = root_paths(&canonical_repo, &roots, &mut degraded_reasons);
        for root in external_roots {
            if budget.expired() {
                degraded_reasons.push("budget_exceeded_before_external_scan_completed".to_string());
                break;
            }
            let files = collect_files(
                &canonical_repo,
                &root,
                FileSet::Source,
                None,
                &budget,
                &mut stats,
                &mut degraded_reasons,
            )?;
            let external_files = files
                .into_iter()
                .filter(|path| {
                    let relative = relative_path(&canonical_repo, path);
                    !is_inside_boundary(&relative, &scope) && !is_excluded_relative_path(&relative)
                })
                .collect::<Vec<_>>();
            scan_source_files(
                &canonical_repo,
                &scope,
                &external_files,
                &symbols,
                &mut usage,
                ScanZone::External,
                evidence_limit,
                &budget,
                &mut degraded_reasons,
            );
        }
    }

    if !symbols.is_empty() && !budget.expired() {
        let docs_roots = root_paths(&canonical_repo, &roots, &mut degraded_reasons);
        for root in docs_roots {
            if budget.expired() {
                degraded_reasons
                    .push("budget_exceeded_before_docs_config_scan_completed".to_string());
                break;
            }
            let files = collect_files(
                &canonical_repo,
                &root,
                FileSet::DocsConfig,
                None,
                &budget,
                &mut stats,
                &mut degraded_reasons,
            )?;
            let external_files = files
                .into_iter()
                .filter(|path| {
                    let relative = relative_path(&canonical_repo, path);
                    !is_inside_boundary(&relative, &scope) && !is_excluded_relative_path(&relative)
                })
                .collect::<Vec<_>>();
            scan_docs_config_files(
                &canonical_repo,
                &external_files,
                &symbols,
                &mut usage,
                evidence_limit,
                &budget,
                &mut degraded_reasons,
            );
        }
    }

    Ok(build_usage_boundary_answer(
        scope,
        roots,
        include_methods,
        symbols,
        usage,
        stats,
        degraded_reasons,
    ))
}

pub fn analyze_usage_boundary_scope_first_redb(
    repo: &Path,
    store: &ReadOnlyGraphStore,
    scope: &str,
    searched_roots: &[String],
    include_methods: bool,
    budget_ms: Option<u64>,
    max_evidence_per_symbol: usize,
) -> Result<DeadCodeAnswer, String> {
    analyze_usage_boundary_scope_first_redb_with_request(
        repo,
        store,
        scope,
        searched_roots,
        include_methods,
        None,
        budget_ms,
        max_evidence_per_symbol,
    )
}

pub fn analyze_usage_boundary_scope_first_redb_with_request(
    repo: &Path,
    store: &ReadOnlyGraphStore,
    scope: &str,
    searched_roots: &[String],
    include_methods: bool,
    request: Option<&str>,
    budget_ms: Option<u64>,
    max_evidence_per_symbol: usize,
) -> Result<DeadCodeAnswer, String> {
    let canonical_repo = repo
        .canonicalize()
        .map_err(|error| format!("failed to resolve repo path {}: {error}", repo.display()))?;
    let scope = normalize_relative_path(scope);
    if scope.is_empty() {
        return Err("scope must be a non-empty repository-relative path".to_string());
    }
    let scope_path = canonical_repo.join(&scope);
    if !scope_path.exists() {
        return Err(format!("scope does not exist: {scope}"));
    }

    let roots = normalize_roots(searched_roots);
    let budget = ScanBudget::new(budget_ms);
    let evidence_limit = max_evidence_per_symbol.max(1);
    let mut degraded_reasons = vec![
        "scope_first_strategy".to_string(),
        "redb_seed_discovery".to_string(),
        "language_support_limited_to_php_for_scope_first_scan".to_string(),
    ];

    let seed_plan = collect_redb_usage_seed_plan(
        &canonical_repo,
        store,
        &scope,
        &roots,
        include_methods,
        request,
        &budget,
        &mut degraded_reasons,
    )?;
    let mut usage = vec![UsageEvidence::default(); seed_plan.symbols.len()];

    if !seed_plan.symbols.is_empty() && !budget.expired() {
        scan_source_files(
            &canonical_repo,
            &scope,
            &seed_plan.internal_source_files,
            &seed_plan.symbols,
            &mut usage,
            ScanZone::Internal,
            evidence_limit,
            &budget,
            &mut degraded_reasons,
        );
    }

    if !seed_plan.symbols.is_empty() && !budget.expired() {
        scan_source_files(
            &canonical_repo,
            &scope,
            &seed_plan.external_source_files,
            &seed_plan.symbols,
            &mut usage,
            ScanZone::External,
            evidence_limit,
            &budget,
            &mut degraded_reasons,
        );
    }

    if !seed_plan.symbols.is_empty() && !budget.expired() {
        scan_docs_config_files(
            &canonical_repo,
            &seed_plan.docs_config_files,
            &seed_plan.symbols,
            &mut usage,
            evidence_limit,
            &budget,
            &mut degraded_reasons,
        );
    }

    Ok(build_usage_boundary_answer(
        scope,
        roots,
        include_methods,
        seed_plan.symbols,
        usage,
        seed_plan.stats,
        degraded_reasons,
    ))
}

fn build_usage_boundary_answer(
    scope: String,
    roots: Vec<String>,
    include_methods: bool,
    symbols: Vec<ScopedSymbol>,
    usage: Vec<UsageEvidence>,
    stats: ScanStats,
    mut degraded_reasons: Vec<String>,
) -> DeadCodeAnswer {
    let name_counts = symbol_name_counts(&symbols);
    let mut candidates = Vec::new();
    let mut excluded = Vec::new();
    let mut internal_caller_count = 0;
    let mut external_caller_count = 0;
    let mut docs_config_reference_count = 0;

    for (index, symbol) in symbols.iter().enumerate() {
        let usage = &usage[index];
        internal_caller_count += usage.internal_callers.len();
        external_caller_count += usage.external_callers.len();
        docs_config_reference_count += usage.docs_config_references.len();
        let ambiguity = ambiguity_for(symbol, usage, &name_counts);
        let status = if !usage.external_callers.is_empty() {
            AnswerStatus::Used
        } else if ambiguity.is_empty() {
            AnswerStatus::Unused
        } else {
            AnswerStatus::Ambiguous
        };
        let confidence = confidence_for(&status, usage);
        let rationale = rationale_for(&status, usage, &ambiguity, &roots);
        let candidate = DeadCodeCandidate {
            function: symbol.fact.clone(),
            status: status.clone(),
            confidence,
            evidence: EvidencePacket {
                searched_roots: roots.clone(),
                internal_callers: usage.internal_callers.iter().cloned().collect(),
                external_callers: usage.external_callers.iter().cloned().collect(),
                docs_config_references: usage.docs_config_references.iter().cloned().collect(),
            },
            ambiguity,
            rationale,
        };
        match status {
            AnswerStatus::Used => excluded.push(candidate),
            _ => candidates.push(candidate),
        }
    }

    candidates.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.function.defined_in.cmp(&right.function.defined_in))
            .then_with(|| left.function.line.cmp(&right.function.line))
            .then_with(|| left.function.name.cmp(&right.function.name))
    });
    excluded.sort_by(|left, right| {
        left.function
            .defined_in
            .cmp(&right.function.defined_in)
            .then_with(|| left.function.line.cmp(&right.function.line))
            .then_with(|| left.function.name.cmp(&right.function.name))
    });

    let summary = DeadCodeSummary {
        total_candidates: candidates.len() + excluded.len(),
        unused: candidates
            .iter()
            .filter(|candidate| matches!(candidate.status, AnswerStatus::Unused))
            .count(),
        ambiguous: candidates
            .iter()
            .filter(|candidate| matches!(candidate.status, AnswerStatus::Ambiguous))
            .count(),
        used: excluded.len(),
    };
    if summary.total_candidates == 0 {
        degraded_reasons.push("no_public_functions_found_in_scope".to_string());
    }
    if summary.ambiguous > 0 {
        degraded_reasons.push("ambiguous_no_external_caller_candidates_present".to_string());
    }
    dedupe(&mut degraded_reasons);

    let edge_count = internal_caller_count + external_caller_count + docs_config_reference_count;
    let observability = DeadCodeObservability {
        graph_counts: DeadCodeGraphCounts {
            functions: summary.total_candidates,
            docs: stats.docs,
            configs: stats.configs,
            edges: edge_count,
        },
        fact_counts: DeadCodeFactCounts {
            public_functions: summary.total_candidates,
            usage_facts: summary.total_candidates,
            internal_callers: internal_caller_count,
            external_callers: external_caller_count,
            docs_config_references: docs_config_reference_count,
        },
        confidence_summary: confidence_summary(candidates.iter().chain(excluded.iter())),
        degraded_reasons,
    };

    DeadCodeAnswer {
        analyzer: "usage-boundary".to_string(),
        version: "1".to_string(),
        query: DeadCodeQuery {
            scope,
            searched_roots: roots,
            include_methods,
        },
        candidates,
        excluded,
        summary,
        observability,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileSet {
    Source,
    DocsConfig,
}

fn collect_redb_usage_seed_plan(
    repo: &Path,
    store: &ReadOnlyGraphStore,
    scope: &str,
    roots: &[String],
    include_methods: bool,
    request: Option<&str>,
    budget: &ScanBudget,
    degraded_reasons: &mut Vec<String>,
) -> Result<UsageBoundarySeedPlan, String> {
    let mut source_files = BTreeSet::new();
    let mut internal_source_files = BTreeSet::new();
    let mut external_source_files = BTreeSet::new();
    let mut docs_config_files = BTreeSet::new();
    let mut class_by_id = BTreeMap::new();
    let mut seed_ids = BTreeSet::new();

    let scope_prefix = path_index_prefix(repo, scope);
    let scope_nodes = redb_nodes_under_path(store, &scope_prefix, degraded_reasons)?;
    for node in &scope_nodes {
        seed_ids.insert(node.id().to_string());
        if let StoredNode::Class(class) = node {
            class_by_id.insert(class.id.to_string(), class.clone());
        }
        if let Some(path) = source_scan_path(node) {
            if is_inside_boundary(&path, scope) {
                internal_source_files.insert(path.clone());
                source_files.insert(path);
            }
        }
    }

    let functions = store
        .functions_under_path(&scope_prefix)
        .map_err(|error| format!("redb functions_under_path failed for {scope}: {error}"))?;
    let mut symbols = Vec::new();
    for function in functions {
        if budget.expired() {
            degraded_reasons.push("budget_exceeded_while_extracting_scope_symbols".to_string());
            break;
        }
        if !is_inside_boundary(function.file_path.as_str(), scope)
            || !is_php_source_path(function.file_path.as_str())
        {
            continue;
        }
        let Some(exposure_kind) =
            classify_usage_boundary_exposure(repo, &function, include_methods, degraded_reasons)
        else {
            continue;
        };
        let parent_class = parent_class_for_function(store, &function, &mut class_by_id)?;
        let class_kind = parent_class.as_ref().and_then(class_kind);
        let parent_class_name = parent_class.map(|class| class.name.to_string());
        seed_ids.insert(function.id.to_string());
        internal_source_files.insert(function.file_path.to_string());
        source_files.insert(function.file_path.to_string());
        symbols.push(ScopedSymbol {
            fact: FunctionFact {
                id: function.id.to_string(),
                name: function.name.to_string(),
                qualified_name: qualified_name_for_usage_boundary(
                    &function,
                    parent_class_name.as_deref(),
                ),
                defined_in: function.file_path.to_string(),
                line: function.line,
                language: function.language.to_string(),
                parent_class: parent_class_name,
                exposure_kind,
            },
            class_kind,
        });
    }
    sort_usage_boundary_symbols(&mut symbols, request);

    collect_adjacency_candidate_paths(
        store,
        &seed_ids,
        scope,
        roots,
        &mut source_files,
        &mut internal_source_files,
        &mut external_source_files,
        &mut docs_config_files,
        degraded_reasons,
    )?;
    collect_root_candidate_paths(
        repo,
        store,
        scope,
        roots,
        &mut source_files,
        &mut internal_source_files,
        &mut external_source_files,
        &mut docs_config_files,
        degraded_reasons,
    )?;

    let mut stats = ScanStats {
        source_files: source_files.len(),
        docs: 0,
        configs: 0,
    };
    for path in &docs_config_files {
        if has_extension(Path::new(path), DOC_EXTENSIONS) {
            stats.docs += 1;
        } else {
            stats.configs += 1;
        }
    }

    let mut internal_source_files = relative_paths_to_full(repo, internal_source_files);
    let mut external_source_files = relative_paths_to_full(repo, external_source_files);
    let mut docs_config_files = relative_paths_to_full(repo, docs_config_files);
    rank_usage_boundary_scan_files(repo, &mut internal_source_files, request);
    rank_usage_boundary_scan_files(repo, &mut external_source_files, request);
    rank_usage_boundary_scan_files(repo, &mut docs_config_files, request);

    Ok(UsageBoundarySeedPlan {
        symbols,
        internal_source_files,
        external_source_files,
        docs_config_files,
        stats,
    })
}

fn collect_adjacency_candidate_paths(
    store: &ReadOnlyGraphStore,
    seed_ids: &BTreeSet<String>,
    scope: &str,
    roots: &[String],
    source_files: &mut BTreeSet<String>,
    internal_source_files: &mut BTreeSet<String>,
    external_source_files: &mut BTreeSet<String>,
    docs_config_files: &mut BTreeSet<String>,
    degraded_reasons: &mut Vec<String>,
) -> Result<(), String> {
    for seed_id in seed_ids {
        for direction in [NeighborDirection::Incoming, NeighborDirection::Outgoing] {
            let edges = store
                .neighbors(seed_id, direction, None)
                .map_err(|error| format!("redb neighbors failed for {seed_id}: {error}"))?;
            for edge in edges {
                if !matches!(
                    edge.kind,
                    EdgeKind::Calls
                        | EdgeKind::References
                        | EdgeKind::Imports
                        | EdgeKind::Documents
                        | EdgeKind::Configures
                        | EdgeKind::EntrypointFor
                ) {
                    continue;
                }
                let Some(node) = store
                    .get_node(edge.other.as_str())
                    .map_err(|error| format!("redb get_node failed for {}: {error}", edge.other))?
                else {
                    degraded_reasons.push(format!("redb_adjacency_missing_node:{}", edge.other));
                    continue;
                };
                add_candidate_path_from_node(
                    &node,
                    scope,
                    roots,
                    source_files,
                    internal_source_files,
                    external_source_files,
                    docs_config_files,
                );
            }
        }
    }
    Ok(())
}

fn collect_root_candidate_paths(
    repo: &Path,
    store: &ReadOnlyGraphStore,
    scope: &str,
    roots: &[String],
    source_files: &mut BTreeSet<String>,
    internal_source_files: &mut BTreeSet<String>,
    external_source_files: &mut BTreeSet<String>,
    docs_config_files: &mut BTreeSet<String>,
    degraded_reasons: &mut Vec<String>,
) -> Result<(), String> {
    for root in redb_root_prefixes(repo, roots, degraded_reasons) {
        let nodes = redb_nodes_under_path(store, &root, degraded_reasons)?;
        for node in nodes {
            add_candidate_path_from_node(
                &node,
                scope,
                roots,
                source_files,
                internal_source_files,
                external_source_files,
                docs_config_files,
            );
        }
    }
    Ok(())
}

fn add_candidate_path_from_node(
    node: &StoredNode,
    scope: &str,
    roots: &[String],
    source_files: &mut BTreeSet<String>,
    internal_source_files: &mut BTreeSet<String>,
    external_source_files: &mut BTreeSet<String>,
    docs_config_files: &mut BTreeSet<String>,
) {
    if let Some(path) = source_scan_path(node) {
        if is_inside_boundary(&path, scope) {
            internal_source_files.insert(path.clone());
            source_files.insert(path);
        } else if path_is_under_roots(&path, roots) && !is_excluded_relative_path(&path) {
            external_source_files.insert(path.clone());
            source_files.insert(path);
        }
    }
    if let Some(path) = docs_config_scan_path(node) {
        if !is_inside_boundary(&path, scope)
            && path_is_under_roots(&path, roots)
            && !is_excluded_relative_path(&path)
        {
            docs_config_files.insert(path);
        }
    }
}

fn parent_class_for_function(
    store: &ReadOnlyGraphStore,
    function: &FunctionNode,
    class_by_id: &mut BTreeMap<String, ClassNode>,
) -> Result<Option<ClassNode>, String> {
    let Some(class_id) = function.parent_class_id.as_ref() else {
        return Ok(None);
    };
    if let Some(class) = class_by_id.get(class_id.as_str()) {
        return Ok(Some(class.clone()));
    }
    let Some(node) = store
        .get_node(class_id.as_str())
        .map_err(|error| format!("redb get_node failed for {class_id}: {error}"))?
    else {
        return Ok(None);
    };
    let StoredNode::Class(class) = node else {
        return Ok(None);
    };
    class_by_id.insert(class.id.to_string(), class.clone());
    Ok(Some(class))
}

fn classify_usage_boundary_exposure(
    repo: &Path,
    function: &FunctionNode,
    include_methods: bool,
    degraded_reasons: &mut Vec<String>,
) -> Option<ExposureKind> {
    if is_ignored_symbol_name(function.name.as_str()) {
        return None;
    }
    if let Some(exposure) = classify_exposure(function, include_methods) {
        return Some(exposure);
    }
    if !include_methods
        || !function.language.eq_ignore_ascii_case("php")
        || function.parent_class_id.is_none()
    {
        return None;
    }

    let path = repo.join(function.file_path.as_str());
    let Ok(content) = fs::read_to_string(&path) else {
        degraded_reasons.push(format!(
            "unreadable_source_file_for_visibility:{}",
            function.file_path
        ));
        return None;
    };
    let Some(line) = content.lines().nth(function.line.saturating_sub(1)) else {
        degraded_reasons.push(format!(
            "missing_source_line_for_visibility:{}:{}",
            function.file_path, function.line
        ));
        return None;
    };
    if is_public_php_method(strip_line_comment(line).trim_start()) {
        Some(ExposureKind::PublicMethod)
    } else {
        None
    }
}

fn redb_nodes_under_path(
    store: &ReadOnlyGraphStore,
    prefix: &str,
    degraded_reasons: &mut Vec<String>,
) -> Result<Vec<StoredNode>, String> {
    let nodes = store
        .nodes_under_path(prefix)
        .map_err(|error| format!("redb nodes_under_path failed for {prefix}: {error}"))?;
    if nodes.is_empty() && !prefix.is_empty() {
        degraded_reasons.push(format!("redb_path_index_empty:{prefix}"));
    }
    Ok(nodes)
}

fn redb_root_prefixes(
    repo: &Path,
    roots: &[String],
    degraded_reasons: &mut Vec<String>,
) -> Vec<String> {
    if roots.is_empty() {
        return vec![String::new()];
    }
    roots
        .iter()
        .filter_map(|root| {
            let path = repo.join(root);
            if !path.exists() {
                degraded_reasons.push(format!(
                    "search_root_missing:{}",
                    relative_path(repo, &path)
                ));
                return None;
            }
            Some(path_index_prefix(repo, root))
        })
        .collect()
}

fn path_index_prefix(repo: &Path, relative: &str) -> String {
    let normalized = normalize_relative_path(relative);
    if normalized.is_empty() {
        return normalized;
    }
    if repo.join(&normalized).is_dir() {
        format!("{normalized}/")
    } else {
        normalized
    }
}

fn source_scan_path(node: &StoredNode) -> Option<String> {
    match node {
        StoredNode::File(file) if is_scannable_source_file(file) => Some(file.path.clone()),
        StoredNode::Function(function) if is_php_source_path(function.file_path.as_str()) => {
            Some(function.file_path.to_string())
        }
        StoredNode::Class(class) if is_php_source_path(class.file_path.as_str()) => {
            Some(class.file_path.to_string())
        }
        _ => None,
    }
}

fn docs_config_scan_path(node: &StoredNode) -> Option<String> {
    match node {
        StoredNode::Doc(doc) => Some(doc.path.clone()),
        StoredNode::Config(config) => Some(config.path.clone()),
        StoredNode::File(file) if matches!(file.role, FileRole::Doc | FileRole::Config) => {
            Some(file.path.clone())
        }
        _ => None,
    }
}

fn is_scannable_source_file(file: &FileNode) -> bool {
    matches!(file.role, FileRole::Source | FileRole::Test) && is_php_source_path(&file.path)
}

fn is_php_source_path(path: &str) -> bool {
    has_extension(Path::new(path), SOURCE_EXTENSIONS)
}

fn path_is_under_roots(path: &str, roots: &[String]) -> bool {
    roots.is_empty()
        || roots
            .iter()
            .any(|root| path == root || path.starts_with(&format!("{root}/")))
}

fn relative_paths_to_full(repo: &Path, paths: BTreeSet<String>) -> Vec<PathBuf> {
    paths.into_iter().map(|path| repo.join(path)).collect()
}

fn sort_usage_boundary_symbols(symbols: &mut [ScopedSymbol], request: Option<&str>) {
    symbols.sort_by(|left, right| {
        let left_score = usage_boundary_auth_seed_score(
            left.fact.defined_in.as_str(),
            Some(left.fact.name.as_str()),
            request,
        );
        let right_score = usage_boundary_auth_seed_score(
            right.fact.defined_in.as_str(),
            Some(right.fact.name.as_str()),
            request,
        );
        right_score
            .cmp(&left_score)
            .then_with(|| left.fact.defined_in.cmp(&right.fact.defined_in))
            .then_with(|| left.fact.line.cmp(&right.fact.line))
            .then_with(|| left.fact.name.cmp(&right.fact.name))
    });
}

fn rank_usage_boundary_scan_files(repo: &Path, files: &mut [PathBuf], request: Option<&str>) {
    files.sort_by(|left, right| {
        let left_relative = relative_path(repo, left);
        let right_relative = relative_path(repo, right);
        let left_score = usage_boundary_auth_seed_score(&left_relative, None, request);
        let right_score = usage_boundary_auth_seed_score(&right_relative, None, request);
        right_score
            .cmp(&left_score)
            .then_with(|| left_relative.cmp(&right_relative))
            .then_with(|| left.cmp(right))
    });
}

fn usage_boundary_auth_seed_score(
    relative_path: &str,
    symbol_name: Option<&str>,
    request: Option<&str>,
) -> i32 {
    if !usage_boundary_auth_focus(request) {
        return 0;
    }

    let lower_path = relative_path.to_ascii_lowercase();
    let basename = lower_path.rsplit('/').next().unwrap_or(&lower_path);
    let lower_symbol = symbol_name.unwrap_or("").to_ascii_lowercase();
    let combined = format!("{lower_path} {lower_symbol}");
    let mut score = 0;

    if contains_any_usage_boundary(
        &combined,
        &["api_keys", "api-key", "api_key", "apikey", "api key"],
    ) {
        score += 110;
    }
    if contains_any_usage_boundary(
        &combined,
        &[
            "/auth",
            "/accounts/",
            "authentication",
            "authenticate",
            "authorization",
            "bearer",
            "credential",
            "jwt",
            "oauth",
            "oidc",
            "session",
            "token",
        ],
    ) {
        score += 35;
    }
    if contains_any_usage_boundary(
        &combined,
        &[
            "middleware",
            "request_auth",
            "request_authentication",
            "authenticate_request",
            "verify_request",
        ],
    ) {
        score += 90;
    }
    if basename == "urls.py"
        || basename.contains("routes")
        || basename.contains("router")
        || basename.contains("controller")
        || basename.contains("views")
        || lower_path.contains("/api/")
    {
        score += 55;
    }
    if contains_any_usage_boundary(
        &combined,
        &[
            "create_key",
            "generate_api_key",
            "issue",
            "create_token",
            "generate_token",
            "mint",
            "validate",
            "verify",
            "check_token",
            "verify_id_token",
        ],
    ) {
        score += 45;
    }

    if usage_boundary_is_outbound_provider_helper(&combined) {
        if usage_boundary_request_targets_outbound_provider_helper(request, &combined) {
            score += 280;
        } else {
            score -= 90;
        }
    }

    score
}

fn usage_boundary_auth_focus(request: Option<&str>) -> bool {
    let Some(request) = request else {
        return false;
    };
    let lower = request.to_ascii_lowercase();
    contains_any_usage_boundary(
        &lower,
        &[
            "api key",
            "api-key",
            "api_key",
            "apikey",
            "authentication",
            "authorization",
            "bearer",
            "credential",
            "jwt",
            "oauth",
            "oidc",
            "session",
            "token",
        ],
    ) || lower
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| token == "auth")
}

fn usage_boundary_is_outbound_provider_helper(lower: &str) -> bool {
    contains_any_usage_boundary(
        lower,
        &[
            "/adapters/",
            "/integrations/",
            "/sdk",
            "auth0_management",
            "client.",
            "management",
            "provider",
        ],
    ) && !contains_any_usage_boundary(lower, &["middleware", "urls.py", "api_keys"])
}

fn usage_boundary_request_targets_outbound_provider_helper(
    request: Option<&str>,
    candidate_lower: &str,
) -> bool {
    let Some(request) = request else {
        return false;
    };
    let lower = request.to_ascii_lowercase();
    if lower.contains("auth0") {
        return candidate_lower.contains("auth0");
    }
    if lower.contains("provider management")
        || lower.contains("provider-management")
        || lower.contains("external provider")
    {
        return contains_any_usage_boundary(
            candidate_lower,
            &["provider", "management", "/integrations/", "/adapters/"],
        );
    }
    if lower.contains("management token")
        || lower.contains("management api")
        || lower.contains("oauth management")
    {
        return contains_any_usage_boundary(candidate_lower, &["management", "provider", "oauth"]);
    }
    false
}

fn contains_any_usage_boundary(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn qualified_name_for_usage_boundary(
    function: &FunctionNode,
    parent_class_name: Option<&str>,
) -> String {
    if let Some(class_name) = parent_class_name {
        format!("{class_name}::{}", function.name)
    } else {
        function.name.to_string()
    }
}

fn class_kind(class: &ClassNode) -> Option<String> {
    let lower = class.signature.trim_start().to_ascii_lowercase();
    if lower.starts_with("interface ") {
        Some("interface".to_string())
    } else if lower.starts_with("trait ") {
        Some("trait".to_string())
    } else if lower.starts_with("class ") {
        Some("class".to_string())
    } else {
        None
    }
}

fn collect_files(
    repo: &Path,
    root: &Path,
    file_set: FileSet,
    boundary: Option<&str>,
    budget: &ScanBudget,
    stats: &mut ScanStats,
    degraded_reasons: &mut Vec<String>,
) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_files_inner(
        repo,
        root,
        file_set,
        boundary,
        budget,
        stats,
        degraded_reasons,
        &mut files,
    )?;
    files.sort_by(|left, right| {
        relative_path(repo, left)
            .cmp(&relative_path(repo, right))
            .then_with(|| left.cmp(right))
    });
    Ok(files)
}

fn collect_files_inner(
    repo: &Path,
    root: &Path,
    file_set: FileSet,
    boundary: Option<&str>,
    budget: &ScanBudget,
    stats: &mut ScanStats,
    degraded_reasons: &mut Vec<String>,
    files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if budget.expired() {
        degraded_reasons.push("budget_exceeded_while_collecting_files".to_string());
        return Ok(());
    }
    if !root.exists() {
        degraded_reasons.push(format!("search_root_missing:{}", relative_path(repo, root)));
        return Ok(());
    }

    let entries = fs::read_dir(root)
        .map_err(|error| format!("failed to read directory {}: {error}", root.display()))?;
    for entry in entries {
        if budget.expired() {
            degraded_reasons.push("budget_exceeded_while_collecting_files".to_string());
            break;
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let relative = relative_path(repo, &path);
        if path.is_dir() {
            if boundary.is_none() && is_excluded_relative_path(&relative) {
                continue;
            }
            collect_files_inner(
                repo,
                &path,
                file_set,
                boundary,
                budget,
                stats,
                degraded_reasons,
                files,
            )?;
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if !matches_file_set(&path, file_set) {
            continue;
        }
        match file_set {
            FileSet::Source => stats.source_files += 1,
            FileSet::DocsConfig => {
                if has_extension(&path, DOC_EXTENSIONS) {
                    stats.docs += 1;
                } else {
                    stats.configs += 1;
                }
            }
        }
        files.push(path);
    }
    Ok(())
}

fn collect_scope_symbols(
    repo: &Path,
    scope: &str,
    scope_files: &[PathBuf],
    include_methods: bool,
    budget: &ScanBudget,
    degraded_reasons: &mut Vec<String>,
) -> Vec<ScopedSymbol> {
    let mut symbols = Vec::new();
    for path in scope_files {
        if budget.expired() {
            degraded_reasons.push("budget_exceeded_while_extracting_scope_symbols".to_string());
            break;
        }
        let Ok(content) = fs::read_to_string(path) else {
            degraded_reasons.push(format!(
                "unreadable_source_file:{}",
                relative_path(repo, path)
            ));
            continue;
        };
        symbols.extend(extract_php_symbols(
            repo,
            scope,
            path,
            &content,
            include_methods,
        ));
    }
    symbols.sort_by(|left, right| {
        left.fact
            .defined_in
            .cmp(&right.fact.defined_in)
            .then_with(|| left.fact.line.cmp(&right.fact.line))
            .then_with(|| left.fact.name.cmp(&right.fact.name))
    });
    symbols
}

fn extract_php_symbols(
    repo: &Path,
    scope: &str,
    path: &Path,
    content: &str,
    include_methods: bool,
) -> Vec<ScopedSymbol> {
    let relative = relative_path(repo, path);
    let mut symbols = Vec::new();
    let mut brace_depth = 0_i32;
    let mut current_class: Option<(String, String, i32)> = None;

    for (line_index, line) in content.lines().enumerate() {
        let line_number = line_index + 1;
        let code = strip_line_comment(line);
        let trimmed = code.trim_start();
        let depth_before = brace_depth;

        if let Some((kind, name)) = php_type_declaration(trimmed) {
            let class_depth = if trimmed.contains('{') {
                depth_before + 1
            } else {
                depth_before + 1
            };
            current_class = Some((kind, name, class_depth));
        }

        if let Some(name) = php_function_name(trimmed) {
            if is_ignored_symbol_name(&name) {
                brace_depth = update_brace_depth(brace_depth, trimmed);
                if current_class
                    .as_ref()
                    .is_some_and(|(_, _, class_depth)| brace_depth < *class_depth)
                {
                    current_class = None;
                }
                continue;
            }
            if let Some((class_kind, class_name, _class_depth)) = current_class.as_ref() {
                if include_methods && is_public_php_method(trimmed) {
                    symbols.push(ScopedSymbol {
                        fact: FunctionFact {
                            id: format!("fn:{relative}:{name}:{line_number}"),
                            name: name.clone(),
                            qualified_name: format!("{class_name}::{name}"),
                            defined_in: relative.clone(),
                            line: line_number,
                            language: "php".to_string(),
                            parent_class: Some(class_name.clone()),
                            exposure_kind: ExposureKind::PublicMethod,
                        },
                        class_kind: Some(class_kind.clone()),
                    });
                }
            } else if is_exported_top_level_function(trimmed) {
                symbols.push(ScopedSymbol {
                    fact: FunctionFact {
                        id: format!("fn:{relative}:{name}:{line_number}"),
                        name: name.clone(),
                        qualified_name: name,
                        defined_in: relative.clone(),
                        line: line_number,
                        language: "php".to_string(),
                        parent_class: None,
                        exposure_kind: ExposureKind::ExportedTopLevel,
                    },
                    class_kind: None,
                });
            }
        }

        brace_depth = update_brace_depth(brace_depth, trimmed);
        if current_class
            .as_ref()
            .is_some_and(|(_, _, class_depth)| brace_depth < *class_depth)
        {
            current_class = None;
        }
    }

    if symbols.is_empty() && relative.starts_with(scope) {
        // No-op branch kept explicit: this analyzer only emits public functions.
    }
    symbols
}

fn scan_source_files(
    repo: &Path,
    boundary: &str,
    files: &[PathBuf],
    symbols: &[ScopedSymbol],
    usage: &mut [UsageEvidence],
    zone: ScanZone,
    evidence_limit: usize,
    budget: &ScanBudget,
    degraded_reasons: &mut Vec<String>,
) {
    for path in files {
        if budget.expired() {
            degraded_reasons.push(match zone {
                ScanZone::Internal => "budget_exceeded_during_internal_scan".to_string(),
                ScanZone::External => "budget_exceeded_during_external_scan".to_string(),
            });
            break;
        }
        let relative = relative_path(repo, path);
        if zone == ScanZone::External && is_inside_boundary(&relative, boundary) {
            continue;
        }
        let Ok(content) = fs::read_to_string(path) else {
            degraded_reasons.push(format!("unreadable_source_file:{relative}"));
            continue;
        };
        scan_source_content(&relative, &content, symbols, usage, zone, evidence_limit);
    }
}

fn scan_source_content(
    relative: &str,
    content: &str,
    symbols: &[ScopedSymbol],
    usage: &mut [UsageEvidence],
    zone: ScanZone,
    evidence_limit: usize,
) {
    for (line_index, line) in content.lines().enumerate() {
        let line_number = line_index + 1;
        let trimmed = strip_line_comment(line).trim().to_string();
        if trimmed.is_empty() || !trimmed.contains('(') {
            continue;
        }
        for (index, symbol) in symbols.iter().enumerate() {
            if symbol.fact.defined_in == relative && symbol.fact.line == line_number {
                continue;
            }
            if !source_line_calls_symbol(&trimmed, symbol) {
                continue;
            }
            let evidence = format!("{relative}:{line_number}:{}", compact_snippet(&trimmed));
            match zone {
                ScanZone::Internal => {
                    if usage[index].internal_callers.len() < evidence_limit {
                        usage[index].internal_callers.insert(evidence);
                    }
                }
                ScanZone::External => {
                    if usage[index].external_callers.len() < evidence_limit {
                        usage[index].external_callers.insert(evidence);
                    }
                }
            }
        }
    }
}

fn scan_docs_config_files(
    repo: &Path,
    files: &[PathBuf],
    symbols: &[ScopedSymbol],
    usage: &mut [UsageEvidence],
    evidence_limit: usize,
    budget: &ScanBudget,
    degraded_reasons: &mut Vec<String>,
) {
    for path in files {
        if budget.expired() {
            degraded_reasons.push("budget_exceeded_during_docs_config_scan".to_string());
            break;
        }
        let relative = relative_path(repo, path);
        let Ok(content) = fs::read_to_string(path) else {
            degraded_reasons.push(format!("unreadable_docs_config_file:{relative}"));
            continue;
        };
        for (line_index, line) in content.lines().enumerate() {
            let line_number = line_index + 1;
            for (index, symbol) in symbols.iter().enumerate() {
                if usage[index].docs_config_references.len() >= evidence_limit {
                    continue;
                }
                if line.contains(&symbol.fact.name) || line.contains(&symbol.fact.qualified_name) {
                    usage[index].docs_config_references.insert(format!(
                        "doc_config:{relative}:{line_number}:{}",
                        compact_snippet(line)
                    ));
                }
            }
        }
    }
}

fn source_line_calls_symbol(line: &str, symbol: &ScopedSymbol) -> bool {
    if is_function_declaration_for(line, &symbol.fact.name) {
        return false;
    }
    if symbol.fact.parent_class.is_some() {
        return contains_method_call(line, &symbol.fact.name);
    }
    contains_top_level_call(line, &symbol.fact.name)
}

fn contains_method_call(line: &str, name: &str) -> bool {
    let compact = compact_code(line);
    compact.contains(&format!("->{name}("))
        || compact.contains(&format!("?->{name}("))
        || compact.contains(&format!("::{name}("))
}

fn contains_top_level_call(line: &str, name: &str) -> bool {
    let compact = compact_code(line);
    if compact.contains(&format!("->{name}("))
        || compact.contains(&format!("?->{name}("))
        || compact.contains(&format!("::{name}("))
    {
        return false;
    }
    let needle = format!("{name}(");
    let mut offset = 0;
    while let Some(position) = compact[offset..].find(&needle) {
        let absolute = offset + position;
        let previous = compact[..absolute].chars().next_back();
        if previous.is_none_or(|value| !is_identifier_char(value) && value != '$') {
            return true;
        }
        offset = absolute + needle.len();
    }
    false
}

fn is_function_declaration_for(line: &str, name: &str) -> bool {
    php_function_name(line).is_some_and(|declared| declared == name)
}

fn php_type_declaration(line: &str) -> Option<(String, String)> {
    let tokens = identifier_tokens(line);
    for (index, token) in tokens.iter().enumerate() {
        if matches!(*token, "class" | "interface" | "trait") {
            let name = tokens.get(index + 1)?;
            if name.is_empty() {
                return None;
            }
            return Some(((*token).to_string(), (*name).to_string()));
        }
    }
    None
}

fn php_function_name(line: &str) -> Option<String> {
    let function_start = line.find("function")?;
    let before = line[..function_start].chars().next_back();
    let after_keyword = function_start + "function".len();
    let after = line[after_keyword..].chars().next();
    if before.is_some_and(is_identifier_char) || after.is_some_and(is_identifier_char) {
        return None;
    }
    let mut chars = line[after_keyword..].trim_start().chars().peekable();
    if chars.peek().is_some_and(|value| *value == '&') {
        chars.next();
    }
    while chars.peek().is_some_and(|value| value.is_whitespace()) {
        chars.next();
    }
    let mut name = String::new();
    while chars.peek().is_some_and(|value| is_identifier_char(*value)) {
        name.push(chars.next().expect("peeked char"));
    }
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn is_public_php_method(line: &str) -> bool {
    let tokens = identifier_tokens(line);
    let Some(function_position) = tokens.iter().position(|token| *token == "function") else {
        return false;
    };
    let modifiers = &tokens[..function_position];
    modifiers.contains(&"public")
        && !modifiers.contains(&"private")
        && !modifiers.contains(&"protected")
}

fn is_exported_top_level_function(line: &str) -> bool {
    let tokens = identifier_tokens(line);
    let Some(function_position) = tokens.iter().position(|token| *token == "function") else {
        return false;
    };
    let modifiers = &tokens[..function_position];
    !modifiers.contains(&"private")
        && !modifiers.contains(&"protected")
        && !modifiers.contains(&"public")
}

fn identifier_tokens(line: &str) -> Vec<&str> {
    line.split(|character: char| !is_identifier_char(character))
        .filter(|token| !token.is_empty())
        .collect()
}

fn is_ignored_symbol_name(name: &str) -> bool {
    name.starts_with('_') || matches!(name, "__construct" | "__destruct")
}

fn strip_line_comment(line: &str) -> &str {
    line.split_once("//")
        .map(|(before, _)| before)
        .unwrap_or(line)
}

fn update_brace_depth(current: i32, line: &str) -> i32 {
    let opens = line.chars().filter(|value| *value == '{').count() as i32;
    let closes = line.chars().filter(|value| *value == '}').count() as i32;
    (current + opens - closes).max(0)
}

fn matches_file_set(path: &Path, file_set: FileSet) -> bool {
    match file_set {
        FileSet::Source => has_extension(path, SOURCE_EXTENSIONS),
        FileSet::DocsConfig => {
            has_extension(path, DOC_EXTENSIONS) || has_extension(path, CONFIG_EXTENSIONS)
        }
    }
}

fn has_extension(path: &Path, allowed: &[&str]) -> bool {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };
    allowed
        .iter()
        .any(|allowed_extension| extension.eq_ignore_ascii_case(allowed_extension))
}

fn root_paths(repo: &Path, roots: &[String], degraded_reasons: &mut Vec<String>) -> Vec<PathBuf> {
    if roots.is_empty() {
        return vec![repo.to_path_buf()];
    }
    roots
        .iter()
        .map(|root| repo.join(root))
        .inspect(|path| {
            if !path.exists() {
                degraded_reasons.push(format!("search_root_missing:{}", relative_path(repo, path)));
            }
        })
        .filter(|path| path.exists())
        .collect()
}

fn symbol_name_counts(symbols: &[ScopedSymbol]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for symbol in symbols {
        *counts.entry(symbol.fact.name.clone()).or_insert(0) += 1;
    }
    counts
}

fn ambiguity_for(
    symbol: &ScopedSymbol,
    usage: &UsageEvidence,
    name_counts: &BTreeMap<String, usize>,
) -> Vec<String> {
    let mut ambiguity = Vec::new();
    if usage.external_callers.is_empty() && !usage.internal_callers.is_empty() {
        ambiguity.push("exported_but_internal_only".to_string());
    }
    if usage.external_callers.is_empty() && !usage.docs_config_references.is_empty() {
        ambiguity.push("docs_config_only_references".to_string());
    }
    if usage.external_callers.is_empty()
        && symbol
            .class_kind
            .as_deref()
            .is_some_and(|kind| matches!(kind, "interface" | "trait"))
    {
        ambiguity.push("interface_or_trait_public_method".to_string());
    }
    if name_counts
        .get(&symbol.fact.name)
        .copied()
        .unwrap_or_default()
        > 1
    {
        ambiguity.push("name_collision_in_scope".to_string());
    }
    ambiguity
}

fn confidence_for(status: &AnswerStatus, usage: &UsageEvidence) -> f32 {
    match status {
        AnswerStatus::Unused => 0.95,
        AnswerStatus::Ambiguous => {
            if !usage.docs_config_references.is_empty() {
                0.75
            } else if !usage.internal_callers.is_empty() {
                0.65
            } else {
                0.6
            }
        }
        AnswerStatus::Used => 0.98,
    }
}

fn confidence_summary<'a, I>(items: I) -> DeadCodeConfidenceSummary
where
    I: Iterator<Item = &'a DeadCodeCandidate>,
{
    let mut high = 0;
    let mut medium = 0;
    let mut low = 0;
    let mut min = None::<f32>;
    let mut max = None::<f32>;

    for item in items {
        let confidence = item.confidence;
        if confidence >= 0.8 {
            high += 1;
        } else if confidence >= 0.5 {
            medium += 1;
        } else {
            low += 1;
        }
        min = Some(min.map_or(confidence, |value| value.min(confidence)));
        max = Some(max.map_or(confidence, |value| value.max(confidence)));
    }

    DeadCodeConfidenceSummary {
        high,
        medium,
        low,
        min,
        max,
    }
}

fn rationale_for(
    status: &AnswerStatus,
    usage: &UsageEvidence,
    ambiguity: &[String],
    roots: &[String],
) -> String {
    let searched = if roots.is_empty() {
        "the repository".to_string()
    } else {
        roots.join(", ")
    };
    match status {
        AnswerStatus::Unused => format!("No external callers found under [{}].", searched),
        AnswerStatus::Ambiguous => format!(
            "No external callers found under [{}], but ambiguity remains: {}.",
            searched,
            ambiguity.join(", ")
        ),
        AnswerStatus::Used => format!(
            "External callers found: {}.",
            usage
                .external_callers
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn normalize_roots(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| normalize_relative_path(value))
        .filter(|value| !value.is_empty())
        .collect()
}

fn normalize_relative_path(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("./")
        .trim_matches('/')
        .replace('\\', "/")
}

fn relative_path(repo: &Path, path: &Path) -> String {
    path.strip_prefix(repo)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

fn is_inside_boundary(path: &str, boundary: &str) -> bool {
    path == boundary || path.starts_with(&format!("{boundary}/"))
}

fn is_excluded_relative_path(relative: &str) -> bool {
    relative
        .split('/')
        .any(|segment| DEFAULT_EXCLUDED_DIRS.contains(&segment))
}

fn compact_code(line: &str) -> String {
    line.chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn compact_snippet(line: &str) -> String {
    let snippet = line.trim();
    let mut value = snippet.chars().take(160).collect::<String>();
    if snippet.chars().count() > 160 {
        value.push_str("...");
    }
    value
}

fn is_identifier_char(value: char) -> bool {
    value.is_ascii_alphanumeric() || value == '_'
}

fn dedupe(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::model::analysis::AnswerStatus;

    use super::{analyze_usage_boundary_scope_first, usage_boundary_auth_seed_score};

    fn temp_repo(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("aethyme_engine_{name}_{nonce}"))
    }

    #[test]
    fn auth_usage_boundary_seed_score_prefers_inbound_request_surfaces() {
        let request = Some("trace token issuing and validation behavior");
        let inbound = usage_boundary_auth_seed_score(
            "backend/api_keys/middleware.py",
            Some("authenticate_api_key"),
            request,
        );
        let outbound = usage_boundary_auth_seed_score(
            "backend/accounts/auth0_management.py",
            Some("get_management_token"),
            request,
        );

        assert!(
            inbound > outbound,
            "broad token tasks should scan inbound request auth before provider helpers"
        );
    }

    #[test]
    fn auth_usage_boundary_seed_score_keeps_named_provider_helpers() {
        let broad = usage_boundary_auth_seed_score(
            "backend/accounts/auth0_management.py",
            Some("get_management_token"),
            Some("trace token issuing and validation behavior"),
        );
        let named = usage_boundary_auth_seed_score(
            "backend/accounts/auth0_management.py",
            Some("get_management_token"),
            Some("trace Auth0 management token behavior"),
        );
        let inbound = usage_boundary_auth_seed_score(
            "backend/api_keys/middleware.py",
            Some("authenticate_api_key"),
            Some("trace Auth0 management token behavior"),
        );

        assert!(
            named > broad,
            "explicit provider-management tasks should not suppress the named helper"
        );
        assert!(
            named > inbound,
            "the named provider-management helper should outrank generic inbound auth for that task"
        );
    }

    #[test]
    fn auth_usage_boundary_seed_score_does_not_promote_unmatched_integrations() {
        let auth0 = usage_boundary_auth_seed_score(
            "backend/accounts/auth0_management.py",
            Some("get_management_token"),
            Some("trace Auth0 management token behavior"),
        );
        let integration = usage_boundary_auth_seed_score(
            "backend/integrations/api_views.py",
            Some("decrypt_token"),
            Some("trace Auth0 management token behavior"),
        );

        assert!(
            auth0 > integration,
            "Auth0-specific tasks should not boost unrelated integration token helpers"
        );
    }

    #[test]
    fn usage_boundary_finds_used_ambiguous_and_unused_php_methods() {
        let root = temp_repo("usage_boundary_php_methods");
        fs::create_dir_all(root.join("includes/Watchlist")).expect("create scope");
        fs::create_dir_all(root.join("includes/Api")).expect("create external");
        fs::write(
            root.join("includes/Watchlist/Store.php"),
            "<?php\nclass Store {\n    public function externalUsed() {}\n    public function internalOnly() {}\n    public function unusedMethod() {}\n}\n",
        )
        .expect("write store");
        fs::write(
            root.join("includes/Watchlist/Manager.php"),
            "<?php\nclass Manager {\n    public function run($store) { $store->internalOnly(); }\n}\n",
        )
        .expect("write manager");
        fs::write(
            root.join("includes/Api/Controller.php"),
            "<?php\nclass Controller {\n    public function handle($store) { $store->externalUsed(); }\n}\n",
        )
        .expect("write controller");

        let answer = analyze_usage_boundary_scope_first(
            &root,
            "includes/Watchlist",
            &[],
            true,
            Some(5_000),
            4,
        )
        .expect("analyze");

        let unused = answer
            .candidates
            .iter()
            .find(|candidate| candidate.function.name == "unusedMethod")
            .expect("unused method");
        assert_eq!(unused.status, AnswerStatus::Unused);

        let internal = answer
            .candidates
            .iter()
            .find(|candidate| candidate.function.name == "internalOnly")
            .expect("internal only method");
        assert_eq!(internal.status, AnswerStatus::Ambiguous);
        assert!(internal
            .ambiguity
            .contains(&"exported_but_internal_only".to_string()));

        let used = answer
            .excluded
            .iter()
            .find(|candidate| candidate.function.name == "externalUsed")
            .expect("external used method");
        assert_eq!(used.status, AnswerStatus::Used);
        assert_eq!(answer.summary.total_candidates, 4);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn usage_boundary_ignores_tests_as_external_callers() {
        let root = temp_repo("usage_boundary_ignores_tests");
        fs::create_dir_all(root.join("includes/Watchlist")).expect("create scope");
        fs::create_dir_all(root.join("tests/phpunit")).expect("create tests");
        fs::write(
            root.join("includes/Watchlist/Store.php"),
            "<?php\nclass Store {\n    public function onlyUsedByTest() {}\n}\n",
        )
        .expect("write store");
        fs::write(
            root.join("tests/phpunit/StoreTest.php"),
            "<?php\n$store->onlyUsedByTest();\n",
        )
        .expect("write test");

        let answer = analyze_usage_boundary_scope_first(
            &root,
            "includes/Watchlist",
            &[],
            true,
            Some(5_000),
            4,
        )
        .expect("analyze");

        let candidate = answer
            .candidates
            .iter()
            .find(|candidate| candidate.function.name == "onlyUsedByTest")
            .expect("candidate");
        assert_eq!(candidate.status, AnswerStatus::Unused);
        assert!(answer.excluded.is_empty());

        let _ = fs::remove_dir_all(&root);
    }
}
