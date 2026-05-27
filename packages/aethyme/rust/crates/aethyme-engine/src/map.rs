use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use crate::store::redb::parse_store::ParseStore;
use crate::indexer::tree_sitter::{GrammarRegistry, default_grammars_dir};
use crate::map_cache;
use crate::model::area::AreaNode;
use crate::model::class::ClassNode;
use crate::model::config::ConfigNode;
use crate::model::directory::DirectoryNode;
use crate::model::doc::DocNode;
use crate::model::edge::Edge;
use crate::model::file::FileNode;
use crate::model::function::FunctionNode;
use crate::model::graph::{GraphNode, GraphNodeKind, NormalizedGraph};
use crate::model::risk::{RiskArea, RiskFlag, RiskLevel};
use crate::model::symbol::Symbol;
use crate::passes;
use crate::repo::{RepoSnapshot, discover_repo};
use aethyme_graph_storage::{AETHYME_DIR, FragmentStore, GRAPH_SUBDIR};
use aethyme_producers::{
    ConfigsProducer, DocsProducer, OverlayProducer, ProducerCtx,
    RepoFileView, RepoView, RisksProducer,
};

/// Pre-computed HashMap indexes for O(1) lookups on entity id → area_id and display string.
/// Built lazily via `OnceLock` on first access; not serialized (derived data).
///
/// `edges_in_by_target_id` is the inverted edge index that fixes the O(F × E)
/// scan in `function_usage_fact`: for each node id, store the indices into
/// `map.edges` of every incoming edge. Lookup becomes O(in_degree) instead of
/// O(|edges|). On MediaWiki this collapses `analyze-dead-code` from ~3 minutes
/// to ~seconds for the per-function caller scan.
#[derive(Debug, Clone)]
struct MapIndex {
    area_id_by_id: HashMap<String, Option<String>>,
    display_by_id: HashMap<String, String>,
    edges_in_by_target_id: HashMap<String, Vec<usize>>,
}

impl MapIndex {
    fn build(map: &RepositoryMap) -> Self {
        let capacity = map.files.len()
            + map.functions.len()
            + map.classes.len()
            + map.docs.len()
            + map.configs.len()
            + map.areas.len();
        let mut area_id_by_id = HashMap::with_capacity(capacity);
        let mut display_by_id = HashMap::with_capacity(capacity);

        for file in &map.files {
            area_id_by_id.insert(file.id.clone(), file.area_id.clone());
            display_by_id.insert(file.id.clone(), file.path.clone());
        }
        for function in &map.functions {
            area_id_by_id.insert(
                function.id.to_string(),
                function.area_id.as_deref().map(String::from),
            );
            display_by_id.insert(
                function.id.to_string(),
                format!("{}::{}", function.file_path, function.name),
            );
        }
        for class in &map.classes {
            area_id_by_id.insert(
                class.id.to_string(),
                class.area_id.as_deref().map(String::from),
            );
            display_by_id.insert(
                class.id.to_string(),
                format!("{}::{}", class.file_path, class.name),
            );
        }
        for doc in &map.docs {
            area_id_by_id.insert(doc.id.clone(), doc.area_id.clone());
            display_by_id.insert(doc.id.clone(), doc.path.clone());
        }
        for config in &map.configs {
            area_id_by_id.insert(config.id.clone(), config.area_id.clone());
            display_by_id.insert(config.id.clone(), config.path.clone());
        }
        for area in &map.areas {
            area_id_by_id.insert(area.id.clone(), Some(area.id.clone()));
            display_by_id.insert(area.id.clone(), area.name.clone());
        }

        // Build the inverted edge index: edge.to → indices into map.edges
        let mut edges_in_by_target_id: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, edge) in map.edges.iter().enumerate() {
            edges_in_by_target_id
                .entry(edge.to.to_string())
                .or_default()
                .push(idx);
        }

        Self {
            area_id_by_id,
            display_by_id,
            edges_in_by_target_id,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RepositoryMap {
    pub snapshot: RepoSnapshot,
    pub areas: Vec<AreaNode>,
    pub directories: Vec<DirectoryNode>,
    pub files: Vec<FileNode>,
    pub classes: Vec<ClassNode>,
    pub functions: Vec<FunctionNode>,
    pub docs: Vec<DocNode>,
    pub configs: Vec<ConfigNode>,
    pub symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
    pub risk_flags: Vec<RiskFlag>,
    pub graph: NormalizedGraph,
    #[serde(skip)]
    index: OnceLock<MapIndex>,
    /// Lazily-computed graph signals (boundary clarity, hidden coupling,
    /// parser visibility, etc). The computation walks 12k files × 3.8M
    /// edges in some assessments — `parser_visibility` alone is
    /// O(F · (F_func + F_class + E)) — so memoizing here turns repeated
    /// callers (anchors + scope + next during one task-localize call)
    /// into a single compute. The daemon benefits even more: one compute
    /// per daemon lifetime, free for every subsequent request.
    /// Not serialized (derived from the rest of the map).
    #[serde(skip)]
    signals: OnceLock<crate::graph::signals::GraphSignals>,
}

impl PartialEq for RepositoryMap {
    fn eq(&self, other: &Self) -> bool {
        self.snapshot == other.snapshot
            && self.areas == other.areas
            && self.directories == other.directories
            && self.files == other.files
            && self.classes == other.classes
            && self.functions == other.functions
            && self.docs == other.docs
            && self.configs == other.configs
            && self.symbols == other.symbols
            && self.edges == other.edges
            && self.risk_flags == other.risk_flags
            && self.graph == other.graph
    }
}

impl Eq for RepositoryMap {}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BuildStageProfile {
    pub name: String,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepositoryBuildProfile {
    pub total_duration_ms: u128,
    pub stages: Vec<BuildStageProfile>,
    pub repo_files: usize,
    pub source_files: usize,
    pub doc_files: usize,
    pub config_files: usize,
    pub areas: usize,
    pub directories: usize,
    pub classes: usize,
    pub functions: usize,
    pub docs: usize,
    pub configs: usize,
    pub graph_nodes: usize,
    pub graph_edges: usize,
    pub graph_annotations: usize,
    pub cache_hits: usize,
    pub cache_misses: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphBuildProfile {
    node_materialization_ms: u128,
    annotation_build_ms: u128,
    sort_ms: u128,
}

impl RepositoryMap {
    pub fn build(root: &Path) -> Result<Self, String> {
        Self::build_with_profile(root).map(|(map, _profile)| map)
    }

    pub fn build_with_profile(root: &Path) -> Result<(Self, RepositoryBuildProfile), String> {
        Self::build_with_profile_and_progress(root, |_| {})
    }

    pub fn build_no_cache(root: &Path) -> Result<(Self, RepositoryBuildProfile), String> {
        Self::build_internal(root, |_| {}, true, false)
    }

    pub fn build_with_profile_and_progress<F>(
        root: &Path,
        progress: F,
    ) -> Result<(Self, RepositoryBuildProfile), String>
    where
        F: FnMut(&BuildStageProfile),
    {
        Self::build_internal(root, progress, false, false)
    }

    /// Explicit opt-in to the Variant C1 fragments path: source the
    /// overlay planes (docs/configs/risks) from the on-disk graph via
    /// the producer crate instead of the legacy engine passes. Falls
    /// back to the pass pipeline when no `.aethyme/graph` directory is
    /// present, so this is always safe to call. The default build path
    /// (`build`/`build_with_profile*`) is unaffected.
    pub fn build_from_fragments(
        root: &Path,
    ) -> Result<(Self, RepositoryBuildProfile), String> {
        Self::build_internal(root, |_| {}, true, true)
    }

    fn build_internal<F>(
        root: &Path,
        mut progress: F,
        no_cache: bool,
        from_fragments: bool,
    ) -> Result<(Self, RepositoryBuildProfile), String>
    where
        F: FnMut(&BuildStageProfile),
    {
        let total_started = Instant::now();
        let mut stages = Vec::new();

        // Variant C1 gate. When explicitly requested AND an on-disk
        // graph exists, populate the map from fragments + producers.
        // This branch deliberately neither reads nor writes the map
        // cache: reading would let a stale pass-built cache shadow the
        // requested fragments build, and writing would let a
        // fragments-built map leak into the default (pass) path on a
        // later run. Both would violate "old path still default".
        if from_fragments && fragments_dir_exists(root) {
            let started = Instant::now();
            let map = Self::populate_from_fragments(root)?;
            push_stage(
                &mut stages,
                "populate_from_fragments",
                started.elapsed().as_millis(),
                &mut progress,
            );
            let profile = map.derive_build_profile(
                total_started.elapsed().as_millis(),
                stages,
            );
            return Ok((map, profile));
        }

        // Try loading a cached map BEFORE discover_repo (avoids 20s+ file walk)
        if !no_cache {
            let cache_started = Instant::now();
            if let Some(cached_map) = map_cache::try_load_cached_map(root) {
                push_stage(
                    &mut stages,
                    "map_cache_hit",
                    cache_started.elapsed().as_millis(),
                    &mut progress,
                );
                let profile = RepositoryBuildProfile {
                    total_duration_ms: total_started.elapsed().as_millis(),
                    stages,
                    repo_files: cached_map.snapshot.files.len(),
                    source_files: cached_map
                        .files
                        .iter()
                        .filter(|f| matches!(f.role, crate::model::file::FileRole::Source))
                        .count(),
                    doc_files: cached_map
                        .files
                        .iter()
                        .filter(|f| matches!(f.role, crate::model::file::FileRole::Doc))
                        .count(),
                    config_files: cached_map
                        .files
                        .iter()
                        .filter(|f| matches!(f.role, crate::model::file::FileRole::Config))
                        .count(),
                    areas: cached_map.areas.len(),
                    directories: cached_map.directories.len(),
                    classes: cached_map.classes.len(),
                    functions: cached_map.functions.len(),
                    docs: cached_map.docs.len(),
                    configs: cached_map.configs.len(),
                    graph_nodes: cached_map.graph.nodes.len(),
                    graph_edges: cached_map.graph.edges.len(),
                    graph_annotations: cached_map.graph.annotations.len(),
                    cache_hits: 0,
                    cache_misses: 0,
                };
                return Ok((cached_map, profile));
            }
        }

        let started = Instant::now();
        let snapshot = discover_repo(root)?;
        push_stage(
            &mut stages,
            "discover_repo",
            started.elapsed().as_millis(),
            &mut progress,
        );

        let started = Instant::now();
        let structure = passes::structure::build(&snapshot);
        push_stage(
            &mut stages,
            "structure",
            started.elapsed().as_millis(),
            &mut progress,
        );

        let grammar_registry = default_grammars_dir().map(|dir| GrammarRegistry::load(&dir));

        // Option X: when --no-cache is set, do not open the parse store at all.
        // No reads, no writes, no transactions opened. The flag now matches its name.
        let parse_store = if no_cache {
            None
        } else {
            match ParseStore::open(root) {
                Ok(store) => Some(store),
                Err(err) => {
                    eprintln!("aethyme: parse store unavailable, falling back to no-cache: {err}");
                    None
                }
            }
        };
        let (code, code_profile, cache_stats) = passes::code::build_with_profile(
            root,
            &structure,
            parse_store.as_ref(),
            grammar_registry.as_ref(),
        );
        push_stage(
            &mut stages,
            "code_parse_files",
            code_profile.parse_files_ms,
            &mut progress,
        );
        push_stage(
            &mut stages,
            "code_normalize_symbols",
            code_profile.normalize_symbols_ms,
            &mut progress,
        );
        push_stage(
            &mut stages,
            "code_resolve_imports",
            code_profile.resolve_imports_ms,
            &mut progress,
        );
        push_stage(
            &mut stages,
            "code_resolve_calls",
            code_profile.resolve_calls_ms,
            &mut progress,
        );
        push_stage(
            &mut stages,
            "code_resolve_references",
            code_profile.resolve_references_ms,
            &mut progress,
        );

        let (configs, _configs_profile) = passes::configs::build_with_profile_and_progress(
            root,
            &structure,
            &code,
            |name, duration_ms| push_stage(&mut stages, name, duration_ms, &mut progress),
        );

        let (docs, _docs_profile) = passes::docs::build_with_profile_and_progress(
            root,
            &structure,
            &code,
            Some(&configs),
            |name, duration_ms| push_stage(&mut stages, name, duration_ms, &mut progress),
        );

        let started = Instant::now();
        let mut edges = Vec::new();
        edges.extend(structure.edges.clone());
        edges.extend(code.edges.clone());
        edges.extend(docs.edges.clone());
        edges.extend(configs.edges.clone());
        edges.sort();
        edges.dedup();
        push_stage(
            &mut stages,
            "edge_normalization",
            started.elapsed().as_millis(),
            &mut progress,
        );

        let mut map = Self {
            snapshot,
            areas: structure.areas,
            directories: structure.directories,
            files: structure.files,
            classes: code.classes,
            functions: code.functions,
            docs: docs.docs,
            configs: configs.configs,
            symbols: code.compatibility_symbols,
            edges,
            risk_flags: Vec::new(),
            graph: NormalizedGraph {
                nodes: Vec::new(),
                edges: Vec::new(),
                annotations: Vec::new(),
            },
            index: OnceLock::new(),
            signals: OnceLock::new(),
        };

        let started = Instant::now();
        map.risk_flags = passes::overlays::detect_risks(&map);
        push_stage(
            &mut stages,
            "overlays",
            started.elapsed().as_millis(),
            &mut progress,
        );

        let (graph, graph_profile) = build_graph_with_profile(&map);
        map.graph = graph;
        push_stage(
            &mut stages,
            "graph_nodes",
            graph_profile.node_materialization_ms,
            &mut progress,
        );
        push_stage(
            &mut stages,
            "graph_annotations",
            graph_profile.annotation_build_ms,
            &mut progress,
        );
        push_stage(
            &mut stages,
            "graph_sort",
            graph_profile.sort_ms,
            &mut progress,
        );

        let profile = RepositoryBuildProfile {
            total_duration_ms: total_started.elapsed().as_millis(),
            stages,
            repo_files: map.snapshot.files.len(),
            source_files: map
                .files
                .iter()
                .filter(|file| matches!(file.role, crate::model::file::FileRole::Source))
                .count(),
            doc_files: map
                .files
                .iter()
                .filter(|file| matches!(file.role, crate::model::file::FileRole::Doc))
                .count(),
            config_files: map
                .files
                .iter()
                .filter(|file| matches!(file.role, crate::model::file::FileRole::Config))
                .count(),
            areas: map.areas.len(),
            directories: map.directories.len(),
            classes: map.classes.len(),
            functions: map.functions.len(),
            docs: map.docs.len(),
            configs: map.configs.len(),
            graph_nodes: map.graph.nodes.len(),
            graph_edges: map.graph.edges.len(),
            graph_annotations: map.graph.annotations.len(),
            cache_hits: cache_stats.hits,
            cache_misses: cache_stats.misses,
        };

        // Save the built map to cache for future runs
        if !no_cache {
            map_cache::save_cached_map(root, &map);
        }

        Ok((map, profile))
    }

    pub fn matching_target_ids(&self, target: &str) -> Vec<String> {
        let lowered = target.to_ascii_lowercase();
        let mut matches = Vec::new();

        if self.graph.nodes.iter().any(|node| node.id == target) {
            matches.push(target.to_string());
        }

        for area in &self.areas {
            if area.id == target
                || area.name.eq_ignore_ascii_case(&lowered)
                || area.path_prefix.eq_ignore_ascii_case(&lowered)
                || area.name.eq_ignore_ascii_case(target)
            {
                push_unique(&mut matches, area.id.clone());
            }
        }
        for directory in &self.directories {
            if directory.id == target
                || directory.path.eq_ignore_ascii_case(target)
                || directory.name.eq_ignore_ascii_case(target)
            {
                push_unique(&mut matches, directory.id.clone());
            }
        }
        for class in &self.classes {
            if class.id == target
                || class.name.eq_ignore_ascii_case(target)
                || class.qualified_name.eq_ignore_ascii_case(target)
            {
                push_unique(&mut matches, class.id.to_string());
            }
        }
        for function in &self.functions {
            if function.id == target
                || function.name.eq_ignore_ascii_case(target)
                || function.qualified_name.eq_ignore_ascii_case(target)
            {
                push_unique(&mut matches, function.id.to_string());
            }
        }
        for doc in &self.docs {
            if doc.id == target
                || doc.path.eq_ignore_ascii_case(target)
                || doc.title.eq_ignore_ascii_case(target)
            {
                push_unique(&mut matches, doc.id.clone());
            }
        }
        for config in &self.configs {
            if config.id == target || config.path.eq_ignore_ascii_case(target) {
                push_unique(&mut matches, config.id.clone());
            }
        }
        for file in &self.files {
            if file.id == target
                || file.path.eq_ignore_ascii_case(target)
                || file.name.eq_ignore_ascii_case(target)
            {
                push_unique(&mut matches, file.id.clone());
            }
        }

        if matches.is_empty() {
            matches.push(target.to_string());
        }
        matches
    }

    fn index(&self) -> &MapIndex {
        self.index.get_or_init(|| MapIndex::build(self))
    }

    /// Memoized `GraphSignals` for this map. Computed on first call,
    /// reused across every subsequent call against the same map.
    pub fn signals(&self) -> &crate::graph::signals::GraphSignals {
        self.signals
            .get_or_init(|| crate::graph::signals::evaluate_graph_signals(self))
    }

    pub fn display_for(&self, value: &str) -> String {
        self.index()
            .display_by_id
            .get(value)
            .cloned()
            .unwrap_or_else(|| value.to_string())
    }

    pub fn area_id_for_target(&self, value: &str) -> Option<String> {
        self.index().area_id_by_id.get(value).cloned().flatten()
    }

    /// Indices into `self.edges` of every edge whose `to` matches `target_id`.
    /// Returns an empty slice when the target has no incoming edges.
    ///
    /// Replaces the O(|edges|) full scan that `function_usage_fact` and
    /// other "who points at this node" callers used to do.
    pub fn edges_to(&self, target_id: &str) -> &[usize] {
        self.index()
            .edges_in_by_target_id
            .get(target_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Variant C1 fragments path: build the map's **overlay planes**
    /// (docs / configs / risks) by running the `aethyme-producers`
    /// producers against the on-disk graph, while the **structure** and
    /// **code** planes still come from the legacy passes.
    ///
    /// ## Why structure + code stay on passes (this commit)
    ///
    /// Phase 4.7.7 cuts over only what the producer crate has already
    /// ported. Structure and code have no producer yet, so we run their
    /// passes exactly as the default path does, then feed
    /// `structure.files` into the producers' [`RepoView`]. Sourcing the
    /// producers from the engine's own structure-filtered file list
    /// (not a fresh repo walk) keeps both pipelines classifying a
    /// byte-identical path set — the isolation the parity gate relies
    /// on.
    ///
    /// ## The schema → model impedance
    ///
    /// Producers emit *schema* types (`NonCodeFile`, producer
    /// `RiskFlag`) that are deliberately lossier than the engine *model*
    /// types this map holds. The conversions below are therefore
    /// one-directional and stamp empty/derived values for fields the
    /// producers dropped: `DocNode::title`/`doc_type` and
    /// `ConfigNode::config_type` become `""`, and overlay-emitted edges
    /// are discarded. `area_id` is recovered by joining each overlay
    /// file back to its `FileNode` by path. These gaps are pinned (and
    /// explained) by the fragments-vs-passes parity test.
    fn populate_from_fragments(root: &Path) -> Result<Self, String> {
        // -- Structure + code planes: legacy passes (unchanged) --------
        let snapshot = discover_repo(root)?;
        let structure = passes::structure::build(&snapshot);

        let grammar_registry =
            default_grammars_dir().map(|dir| GrammarRegistry::load(&dir));
        let parse_store = match ParseStore::open(root) {
            Ok(store) => Some(store),
            Err(err) => {
                eprintln!(
                    "aethyme: parse store unavailable, falling back to no-cache: {err}"
                );
                None
            }
        };
        let (code, _code_profile, _cache_stats) = passes::code::build_with_profile(
            root,
            &structure,
            parse_store.as_ref(),
            grammar_registry.as_ref(),
        );

        let repo_name = snapshot.repo_name();

        // -- Overlay planes: run the producers against the graph -------
        let store = FragmentStore::open(root).map_err(|err| {
            format!("open fragment store at {}: {err}", root.display())
        })?;
        let view = StructureFilesView {
            name: repo_name.clone(),
            root_path: snapshot.root.clone(),
            files: structure
                .files
                .iter()
                .map(|f| RepoFileView {
                    path: f.path.clone(),
                    language: f.language.clone(),
                    byte_size: f.size_bytes,
                    // Doc/config/risk classification is path-only; a
                    // stub hash keeps view construction cheap without
                    // touching any compared surface.
                    content_hash: format!("stub-{}", f.path),
                })
                .collect(),
        };
        let ctx = ProducerCtx::with_repo(&store, &view);

        // path → FileNode, for sourcing file_id + area_id by path.
        let file_index: HashMap<&str, &FileNode> =
            structure.files.iter().map(|f| (f.path.as_str(), f)).collect();
        let file_id_of = |path: &str| -> String {
            file_index
                .get(path)
                .map(|f| f.id.clone())
                .unwrap_or_else(|| format!("file:{repo_name}:{path}"))
        };
        let area_of =
            |path: &str| -> Option<String> { file_index.get(path).and_then(|f| f.area_id.clone()) };

        let configs_overlay =
            ConfigsProducer.produce(&ctx).map_err(|e| e.to_string())?;
        let configs: Vec<ConfigNode> = configs_overlay
            .payload()
            .files
            .iter()
            .map(|nc| {
                let path = nc.path();
                // config_type is graph-derived in the engine model and
                // is not carried by the producer overlay; stamp empty.
                ConfigNode::new(&repo_name, &file_id_of(path), path, "", area_of(path))
            })
            .collect();

        let docs_overlay = DocsProducer.produce(&ctx).map_err(|e| e.to_string())?;
        let docs: Vec<DocNode> = docs_overlay
            .payload()
            .files
            .iter()
            .map(|nc| {
                let path = nc.path();
                // title + doc_type are dropped by the docs producer.
                DocNode::new(&repo_name, &file_id_of(path), path, "", "", area_of(path))
            })
            .collect();

        let risks_overlay = RisksProducer.produce(&ctx).map_err(|e| e.to_string())?;
        let risk_flags: Vec<RiskFlag> = risks_overlay
            .payload()
            .risks
            .iter()
            .map(|r| {
                RiskFlag::new(
                    r.scope.clone(),
                    convert_risk_area(&r.area),
                    convert_risk_level(&r.level),
                    r.reason.clone(),
                )
            })
            .collect();

        // -- Edges: structure + code only; overlay edges are dropped ---
        let mut edges = Vec::new();
        edges.extend(structure.edges.clone());
        edges.extend(code.edges.clone());
        edges.sort();
        edges.dedup();

        // `file_index` borrows `structure.files`; its last use (via the
        // closures above) precedes the move below, so NLL permits it.
        let mut map = Self {
            snapshot,
            areas: structure.areas,
            directories: structure.directories,
            files: structure.files,
            classes: code.classes,
            functions: code.functions,
            docs,
            configs,
            symbols: code.compatibility_symbols,
            edges,
            risk_flags,
            graph: NormalizedGraph {
                nodes: Vec::new(),
                edges: Vec::new(),
                annotations: Vec::new(),
            },
            index: OnceLock::new(),
            signals: OnceLock::new(),
        };

        let (graph, _graph_profile) = build_graph_with_profile(&map);
        map.graph = graph;
        Ok(map)
    }

    /// Assemble a [`RepositoryBuildProfile`] for a fragments-sourced
    /// map. Mirrors the cache-hit profile block: per-plane counts are
    /// read straight off the finished map, and `cache_hits` /
    /// `cache_misses` are zero because the fragments path bypasses the
    /// map cache entirely (see the gate comment in `build_internal`).
    fn derive_build_profile(
        &self,
        total_duration_ms: u128,
        stages: Vec<BuildStageProfile>,
    ) -> RepositoryBuildProfile {
        use crate::model::file::FileRole;
        RepositoryBuildProfile {
            total_duration_ms,
            stages,
            repo_files: self.snapshot.files.len(),
            source_files: self
                .files
                .iter()
                .filter(|file| matches!(file.role, FileRole::Source))
                .count(),
            doc_files: self
                .files
                .iter()
                .filter(|file| matches!(file.role, FileRole::Doc))
                .count(),
            config_files: self
                .files
                .iter()
                .filter(|file| matches!(file.role, FileRole::Config))
                .count(),
            areas: self.areas.len(),
            directories: self.directories.len(),
            classes: self.classes.len(),
            functions: self.functions.len(),
            docs: self.docs.len(),
            configs: self.configs.len(),
            graph_nodes: self.graph.nodes.len(),
            graph_edges: self.graph.edges.len(),
            graph_annotations: self.graph.annotations.len(),
            cache_hits: 0,
            cache_misses: 0,
        }
    }
}

/// True when `root` already holds a materialized on-disk graph at
/// `<root>/.aethyme/graph/`. The fragments build path is gated on this:
/// `--from-fragments` falls back to the legacy pass pipeline when no
/// graph directory is present, so a fresh checkout never hard-fails.
fn fragments_dir_exists(root: &Path) -> bool {
    root.join(AETHYME_DIR).join(GRAPH_SUBDIR).is_dir()
}

/// Convert the producer crate's
/// [`RiskArea`](aethyme_producers::risks::RiskArea) into the engine
/// model's [`RiskArea`](crate::model::risk::RiskArea). The two enums are
/// verbatim mirrors — the producer's was copied from the model during
/// the Phase 4.7.6 port — so this is a total, variant-for-variant map;
/// the `UserDefined` payload is cloned across the type boundary.
fn convert_risk_area(
    area: &aethyme_producers::risks::RiskArea,
) -> RiskArea {
    use aethyme_producers::risks::RiskArea as P;
    match area {
        P::Auth => RiskArea::Auth,
        P::Permissions => RiskArea::Permissions,
        P::Secrets => RiskArea::Secrets,
        P::Migrations => RiskArea::Migrations,
        P::Infra => RiskArea::Infra,
        P::Billing => RiskArea::Billing,
        P::SharedCore => RiskArea::SharedCore,
        P::Destructive => RiskArea::Destructive,
        P::UserDefined(s) => RiskArea::UserDefined(s.clone()),
    }
}

/// Convert the producer crate's
/// [`RiskLevel`](aethyme_producers::risks::RiskLevel) into the engine
/// model's. Total mirror map (Low/Medium/High); see [`convert_risk_area`].
fn convert_risk_level(
    level: &aethyme_producers::risks::RiskLevel,
) -> RiskLevel {
    use aethyme_producers::risks::RiskLevel as P;
    match level {
        P::Low => RiskLevel::Low,
        P::Medium => RiskLevel::Medium,
        P::High => RiskLevel::High,
    }
}

/// Minimal [`RepoView`] backed by a structure pass's file list. The
/// overlay producers (`ConfigsProducer` / `DocsProducer` /
/// `RisksProducer`) classify on path shape alone, so handing them the
/// engine's already structure-filtered `FileNode` paths makes them see
/// the exact file set the legacy passes saw — the precondition for
/// fragments-vs-passes parity. Owns its fields and lends them back
/// through the trait.
struct StructureFilesView {
    name: String,
    root_path: String,
    files: Vec<RepoFileView>,
}

impl RepoView for StructureFilesView {
    fn name(&self) -> &str {
        &self.name
    }

    fn root_path(&self) -> &str {
        &self.root_path
    }

    fn vcs(&self) -> &str {
        "git"
    }

    fn files(&self) -> &[RepoFileView] {
        &self.files
    }
}

fn stage_profile(name: &str, duration_ms: u128) -> BuildStageProfile {
    BuildStageProfile {
        name: name.to_string(),
        duration_ms,
    }
}

fn push_stage<F>(
    stages: &mut Vec<BuildStageProfile>,
    name: &str,
    duration_ms: u128,
    progress: &mut F,
) where
    F: FnMut(&BuildStageProfile),
{
    let stage = stage_profile(name, duration_ms);
    progress(&stage);
    stages.push(stage);
}

fn push_unique(values: &mut Vec<String>, candidate: String) {
    if !values.contains(&candidate) {
        values.push(candidate);
    }
}

fn build_graph_with_profile(map: &RepositoryMap) -> (NormalizedGraph, GraphBuildProfile) {
    let nodes_started = Instant::now();
    let mut nodes = Vec::new();
    let repo_name = map.snapshot.repo_name();
    nodes.push(GraphNode {
        id: format!("repo:{repo_name}"),
        kind: GraphNodeKind::Repo,
        label: repo_name,
        path: Some(map.snapshot.root.clone()),
        language: None,
        confidence: 1000,
        source: "structure".to_string(),
        metadata: std::collections::BTreeMap::new(),
    });

    for area in &map.areas {
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("inferred".to_string(), area.inferred.to_string());
        nodes.push(GraphNode {
            id: area.id.clone(),
            kind: GraphNodeKind::Area,
            label: area.name.clone(),
            path: Some(area.path_prefix.clone()),
            language: None,
            confidence: 1000,
            source: "structure".to_string(),
            metadata,
        });
    }
    for directory in &map.directories {
        nodes.push(GraphNode {
            id: directory.id.clone(),
            kind: GraphNodeKind::Directory,
            label: directory.name.clone(),
            path: Some(directory.path.clone()),
            language: None,
            confidence: 1000,
            source: "structure".to_string(),
            metadata: std::collections::BTreeMap::new(),
        });
    }
    for file in &map.files {
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert(
            "role".to_string(),
            format!("{:?}", file.role).to_ascii_lowercase(),
        );
        metadata.insert("generated".to_string(), file.generated.to_string());
        nodes.push(GraphNode {
            id: file.id.clone(),
            kind: GraphNodeKind::File,
            label: file.name.clone(),
            path: Some(file.path.clone()),
            language: file.language.clone(),
            confidence: 1000,
            source: "structure".to_string(),
            metadata,
        });
    }
    for class in &map.classes {
        nodes.push(GraphNode {
            id: class.id.to_string(),
            kind: GraphNodeKind::Class,
            label: class.name.to_string(),
            path: Some(class.file_path.to_string()),
            language: Some(class.language.to_string()),
            confidence: 1000,
            source: "code".to_string(),
            metadata: std::collections::BTreeMap::new(),
        });
    }
    for function in &map.functions {
        nodes.push(GraphNode {
            id: function.id.to_string(),
            kind: GraphNodeKind::Function,
            label: function.name.to_string(),
            path: Some(function.file_path.to_string()),
            language: Some(function.language.to_string()),
            confidence: 1000,
            source: "code".to_string(),
            metadata: std::collections::BTreeMap::new(),
        });
    }
    for doc in &map.docs {
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("doc_type".to_string(), doc.doc_type.clone());
        nodes.push(GraphNode {
            id: doc.id.clone(),
            kind: GraphNodeKind::Doc,
            label: doc.title.clone(),
            path: Some(doc.path.clone()),
            language: None,
            confidence: 900,
            source: "docs".to_string(),
            metadata,
        });
    }
    for config in &map.configs {
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("config_type".to_string(), config.config_type.clone());
        nodes.push(GraphNode {
            id: config.id.clone(),
            kind: GraphNodeKind::Config,
            label: config.config_type.clone(),
            path: Some(config.path.clone()),
            language: None,
            confidence: 900,
            source: "config".to_string(),
            metadata,
        });
    }
    let node_materialization_ms = nodes_started.elapsed().as_millis();

    let annotation_started = Instant::now();
    let annotations = passes::overlays::graph_annotations(map);
    let annotation_build_ms = annotation_started.elapsed().as_millis();
    let mut graph = NormalizedGraph {
        nodes,
        edges: map.edges.clone(),
        annotations,
    };
    let sort_started = Instant::now();
    graph.sort();
    let sort_ms = sort_started.elapsed().as_millis();
    (
        graph,
        GraphBuildProfile {
            node_materialization_ms,
            annotation_build_ms,
            sort_ms,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::RepositoryMap;

    #[test]
    fn build_map_creates_graph_layers() {
        let root = std::env::temp_dir().join("aethyme_engine_map_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/auth")).expect("create temp repo");
        fs::write(root.join("README.md"), "# Demo\n").expect("write readme");
        fs::write(
            root.join("src/auth/service.py"),
            "from app.core import token\nclass AuthService:\n    pass\n\ndef validate_token():\n    return True\n",
        )
        .expect("write source file");
        fs::write(root.join("src/auth/architecture.md"), "# Auth\n").expect("write docs");
        fs::write(root.join("Cargo.toml"), "[package]\nname = 'demo'\n").expect("write config");

        let map = RepositoryMap::build(&root).expect("build repository map");

        assert!(!map.areas.is_empty());
        assert!(!map.directories.is_empty());
        assert!(
            map.functions
                .iter()
                .any(|function| function.name == "validate_token")
        );
        assert!(map.classes.iter().any(|class| class.name == "AuthService"));
        assert!(!map.docs.is_empty());
        assert!(!map.configs.is_empty());
        assert!(!map.graph.nodes.is_empty());
        assert!(
            map.risk_flags
                .iter()
                .any(|flag| flag.scope == "src/auth/service.py")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn build_with_profile_reports_stage_timings() {
        let root = std::env::temp_dir().join("aethyme_engine_profile_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp repo");
        fs::write(root.join("README.md"), "# Demo\n").expect("write readme");
        fs::write(root.join("src/main.py"), "def run():\n    return True\n").expect("write source");

        let (map, profile) =
            RepositoryMap::build_with_profile(&root).expect("build repository map with profile");

        assert!(!profile.stages.is_empty());
        assert!(
            profile
                .stages
                .iter()
                .any(|stage| stage.name == "discover_repo")
        );
        assert_eq!(profile.repo_files, map.snapshot.files.len());
        assert_eq!(profile.graph_nodes, map.graph.nodes.len());

        let _ = fs::remove_dir_all(&root);
    }
}
