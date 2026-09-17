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

/// Maximum number of provider-ranked impact paths admitted to one report.
pub const GRAPH_IMPACT_RESULT_LIMIT: usize = 64;
/// Maximum incoming relationship hops from a changed graph node.
pub const GRAPH_IMPACT_MAX_DEPTH: usize = 2;
/// Maximum distinct graph nodes admitted to the bounded traversal.
pub const GRAPH_IMPACT_MAX_NODES: usize = 128;

/// Relationship used to derive a bounded graph-impact frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphImpactMode {
    Calls,
    Imports,
}

impl GraphImpactMode {
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
}
