use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use crate::model::area::AreaNode;
use crate::model::class::ClassNode;
use crate::model::config::ConfigNode;
use crate::model::directory::DirectoryNode;
use crate::model::doc::DocNode;
use crate::model::edge::{Edge, EdgeKind};
use crate::model::file::{FileNode, FileRole};
use crate::model::function::FunctionNode;
use crate::model::graph::{GraphAnnotation, GraphNode, GraphNodeKind, NormalizedGraph};
use crate::model::intern::InternedStr;
use crate::model::risk::{RiskArea, RiskFlag, RiskLevel};
use crate::model::symbol::{Symbol, SymbolKind};
use crate::repo::{RepoFile, RepoSnapshot};
use aethyme_graph_schema::{
    Callable, EdgeKind as SchemaEdgeKind, Node, NonCodeFormat, SourceRange,
};
use aethyme_graph_storage::{Fragment, FragmentStore};
use aethyme_producers::{
    ConfigsProducer, DocsProducer, OverlayProducer, ProducerCtx, RepoFileView, RepoView,
    RisksProducer, StructureProducer,
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
        Self::build_internal(root, |_| {}, true)
    }

    pub fn build_with_profile_and_progress<F>(
        root: &Path,
        progress: F,
    ) -> Result<(Self, RepositoryBuildProfile), String>
    where
        F: FnMut(&BuildStageProfile),
    {
        Self::build_internal(root, progress, false)
    }

    /// Compatibility spelling for the fragments-only build path.
    ///
    /// Phase 4.7.12 deleted the legacy pass pipeline, so every
    /// `RepositoryMap` build now consumes the committed
    /// `.aethyme/graph` store. Missing fragments are a hard error.
    pub fn build_from_fragments(root: &Path) -> Result<(Self, RepositoryBuildProfile), String> {
        Self::build_internal(root, |_| {}, true)
    }

    /// Default-facing build API. The `no_cache` argument is retained
    /// for CLI compatibility; parse-store caching was deleted in
    /// 4.7.11 and the pass pipeline was deleted in 4.7.12.
    pub fn build_with_fragment_preference<F>(
        root: &Path,
        _no_cache: bool,
        progress: F,
    ) -> Result<(Self, RepositoryBuildProfile), String>
    where
        F: FnMut(&BuildStageProfile),
    {
        Self::build_internal(root, progress, false)
    }

    fn build_internal<F>(
        root: &Path,
        mut progress: F,
        _no_cache: bool,
    ) -> Result<(Self, RepositoryBuildProfile), String>
    where
        F: FnMut(&BuildStageProfile),
    {
        let total_started = Instant::now();
        let mut stages = Vec::new();

        #[cfg(test)]
        ensure_test_fragments(root)?;

        let started = Instant::now();
        let map = Self::populate_from_fragments(root)?;
        push_stage(
            &mut stages,
            "populate_from_fragments",
            started.elapsed().as_millis(),
            &mut progress,
        );
        let profile = map.derive_build_profile(total_started.elapsed().as_millis(), stages);
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

    /// Fragments-only map build: consume committed per-file fragments
    /// plus overlay producers, then adapt schema nodes into the
    /// engine's compatibility model types.
    fn populate_from_fragments(root: &Path) -> Result<Self, String> {
        let canonical_root = root
            .canonicalize()
            .map_err(|err| format!("Failed to canonicalize repo path: {err}"))?;
        let repo_name = canonical_root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("repo")
            .to_string();
        let root_path = canonical_root.to_string_lossy().to_string();

        let store = FragmentStore::open(root).map_err(|err| {
            format!(
                "open fragment store at {}: {err}. The legacy pass pipeline was deleted in 4.7.12; run `aethyme-graph-index` before using the engine.",
                root.display()
            )
        })?;
        let fragments = read_all_fragments(&store)?;
        if fragments.is_empty() {
            return Err(format!(
                "fragment store at {} contains no source fragments; run `aethyme-graph-index` before using the engine",
                root.display()
            ));
        }

        let fragment_files = collect_fragment_files(&fragments);
        let view = FragmentFilesView {
            name: repo_name.clone(),
            root_path: root_path.clone(),
            files: fragment_files
                .values()
                .map(|f| RepoFileView {
                    path: f.path.clone(),
                    language: f.language.clone(),
                    byte_size: f.byte_size,
                    content_hash: f.content_hash.clone(),
                })
                .collect(),
        };
        let ctx = ProducerCtx::with_repo(&store, &view);

        let structure_overlay = StructureProducer.produce(&ctx).map_err(|e| e.to_string())?;
        let structure = structure_overlay.payload();

        let mut top_level_dirs = BTreeSet::new();
        for file in &structure.files {
            if let Some((top, _rest)) = file.path().split_once('/') {
                top_level_dirs.insert(top.to_string());
            }
        }
        let top_level_dirs = top_level_dirs.into_iter().collect::<Vec<_>>();
        let areas: Vec<AreaNode> = top_level_dirs
            .iter()
            .map(|path| AreaNode::new(&repo_name, path, false))
            .collect();
        let mut model_id_by_schema: HashMap<String, String> = HashMap::new();
        let mut directories = Vec::with_capacity(structure.directories.len());
        for d in &structure.directories {
            let directory = DirectoryNode::new(
                &repo_name,
                d.path(),
                compatibility_area_id(&repo_name, d.path(), &top_level_dirs),
            );
            model_id_by_schema.insert(d.id().as_str().to_string(), directory.id.clone());
            directories.push(directory);
        }
        let mut files = Vec::with_capacity(structure.files.len());
        for f in &structure.files {
            let language = optional_language(f.language());
            let generated = is_generated(f.path());
            let role = classify_file_role(f.path(), language.as_deref(), generated);
            let file = FileNode::new(
                &repo_name,
                f.path(),
                language,
                role,
                0,
                f.byte_size(),
                generated,
                compatibility_area_id(&repo_name, f.path(), &top_level_dirs),
            );
            model_id_by_schema.insert(f.id().as_str().to_string(), file.id.clone());
            files.push(file);
        }

        let snapshot = snapshot_from_files(root_path, &files);
        let file_index: HashMap<&str, &FileNode> =
            files.iter().map(|f| (f.path.as_str(), f)).collect();
        let file_id_of = |path: &str| -> String {
            file_index
                .get(path)
                .map(|f| f.id.clone())
                .unwrap_or_else(|| path.to_string())
        };
        let area_of =
            |path: &str| -> Option<String> { file_index.get(path).and_then(|f| f.area_id.clone()) };

        let configs_overlay = ConfigsProducer.produce(&ctx).map_err(|e| e.to_string())?;
        let mut configs = Vec::with_capacity(configs_overlay.payload().files.len());
        for nc in &configs_overlay.payload().files {
            let path = nc.path();
            let config = ConfigNode::new(
                &repo_name,
                &file_id_of(path),
                path,
                &classify_config_type(path),
                area_of(path),
            );
            model_id_by_schema.insert(nc.id().as_str().to_string(), config.id.clone());
            configs.push(config);
        }

        let docs_overlay = DocsProducer.produce(&ctx).map_err(|e| e.to_string())?;
        let mut docs = Vec::with_capacity(docs_overlay.payload().files.len());
        for nc in &docs_overlay.payload().files {
            let path = nc.path();
            let doc = DocNode::new(
                &repo_name,
                &file_id_of(path),
                path,
                leaf_name(path),
                &classify_doc_type(path, ""),
                area_of(path),
            );
            model_id_by_schema.insert(nc.id().as_str().to_string(), doc.id.clone());
            docs.push(doc);
        }

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

        let mut classes = Vec::new();
        for fragment in &fragments {
            let Some(file) = file_index.get(fragment.file_path()) else {
                continue;
            };
            for node in fragment.nodes() {
                if let Some((schema_id, class)) = class_from_schema_node(node, file, &repo_name) {
                    model_id_by_schema.insert(schema_id, class.id.to_string());
                    classes.push(class);
                }
            }
        }
        classes.sort();
        classes.dedup();

        let mut functions = Vec::new();
        for fragment in &fragments {
            let Some(file) = file_index.get(fragment.file_path()) else {
                continue;
            };
            for node in fragment.nodes() {
                if let Some((schema_id, function)) =
                    function_from_schema_node(node, file, &repo_name, &model_id_by_schema)
                {
                    model_id_by_schema.insert(schema_id, function.id.to_string());
                    functions.push(function);
                }
            }
        }
        functions.sort();
        functions.dedup();

        let mut symbols = compatibility_symbols(&classes, &functions);

        let mut edges = Vec::new();
        let model_repo_id = format!("repo:{repo_name}");
        edges.extend(structure.edges.iter().map(|edge| {
            schema_edge_to_model_with_repo(
                edge,
                structure.repository.id().as_str(),
                &model_repo_id,
                &model_id_by_schema,
            )
        }));
        edges.extend(compatibility_area_edges(
            &model_repo_id,
            &areas,
            &directories,
            &files,
        ));
        edges.extend(compatibility_overlay_edges(&docs, &configs));
        for fragment in &fragments {
            edges.extend(
                fragment
                    .edges()
                    .iter()
                    .map(|edge| schema_edge_to_model(edge, &model_id_by_schema)),
            );
        }
        edges.sort();
        edges.dedup();
        symbols.sort();
        symbols.dedup();

        let mut map = Self {
            snapshot,
            areas,
            directories,
            files,
            classes,
            functions,
            docs,
            configs,
            symbols,
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
    /// map. Per-plane counts are read straight off the finished map, and
    /// `cache_hits` / `cache_misses` are zero because the parse-store
    /// cache was deleted in 4.7.11.
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

/// Convert the producer crate's
/// [`RiskArea`](aethyme_producers::risks::RiskArea) into the engine
/// model's [`RiskArea`](crate::model::risk::RiskArea). The two enums are
/// verbatim mirrors — the producer's was copied from the model during
/// the Phase 4.7.6 port — so this is a total, variant-for-variant map;
/// the `UserDefined` payload is cloned across the type boundary.
fn convert_risk_area(area: &aethyme_producers::risks::RiskArea) -> RiskArea {
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
fn convert_risk_level(level: &aethyme_producers::risks::RiskLevel) -> RiskLevel {
    use aethyme_producers::risks::RiskLevel as P;
    match level {
        P::Low => RiskLevel::Low,
        P::Medium => RiskLevel::Medium,
        P::High => RiskLevel::High,
    }
}

#[derive(Debug, Clone)]
struct FragmentFileInfo {
    path: String,
    language: Option<String>,
    byte_size: u64,
    content_hash: String,
}

/// Minimal [`RepoView`] backed by committed fragment paths.
struct FragmentFilesView {
    name: String,
    root_path: String,
    files: Vec<RepoFileView>,
}

impl RepoView for FragmentFilesView {
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

fn read_all_fragments(store: &FragmentStore) -> Result<Vec<Fragment>, String> {
    let paths = store
        .list_indexed_source_paths()
        .map_err(|err| format!("list indexed fragments: {err}"))?;
    let mut fragments = Vec::with_capacity(paths.len());
    for path in paths {
        let fragment = store
            .read_fragment(&path)
            .map_err(|err| format!("read fragment {path}: {err}"))?;
        fragments.push(fragment);
    }
    Ok(fragments)
}

fn collect_fragment_files(
    fragments: &[Fragment],
) -> std::collections::BTreeMap<String, FragmentFileInfo> {
    let mut files = std::collections::BTreeMap::new();
    for fragment in fragments {
        let mut info = None;
        for node in fragment.nodes() {
            match node {
                Node::File(file) => {
                    info = Some(FragmentFileInfo {
                        path: file.path().to_string(),
                        language: optional_language(file.language()),
                        byte_size: file.byte_size(),
                        content_hash: file.content_hash().to_string(),
                    });
                    break;
                }
                Node::NonCodeFile(file) => {
                    info = Some(FragmentFileInfo {
                        path: file.path().to_string(),
                        language: Some(non_code_format_label(file.format()).to_string()),
                        byte_size: 0,
                        content_hash: format!("fragment-{}", file.path()),
                    });
                    break;
                }
                _ => {}
            }
        }
        let info = info.unwrap_or_else(|| FragmentFileInfo {
            path: fragment.file_path().to_string(),
            language: None,
            byte_size: 0,
            content_hash: format!("fragment-{}", fragment.file_path()),
        });
        files.insert(info.path.clone(), info);
    }
    files
}

fn snapshot_from_files(root_path: String, files: &[FileNode]) -> RepoSnapshot {
    let mut languages = BTreeSet::new();
    let mut top_level_dirs = BTreeSet::new();
    let mut readme_path = None;
    let mut repo_files = Vec::with_capacity(files.len());

    for file in files {
        if let Some(language) = &file.language {
            languages.insert(language.clone());
        }
        if let Some((top, _rest)) = file.path.split_once('/') {
            top_level_dirs.insert(top.to_string());
        }
        if readme_path.is_none() {
            let lowercase = leaf_name(&file.path).to_ascii_lowercase();
            if lowercase == "readme.md" || lowercase == "readme" {
                readme_path = Some(file.path.clone());
            }
        }
        repo_files.push(RepoFile {
            path: file.path.clone(),
            language: file.language.clone(),
            line_count: file.line_count,
            size_bytes: file.size_bytes,
        });
    }

    repo_files.sort_by(|left, right| left.path.cmp(&right.path));

    RepoSnapshot {
        root: root_path,
        files: repo_files,
        languages: languages.into_iter().collect(),
        top_level_dirs: top_level_dirs.into_iter().collect(),
        readme_path,
    }
}

fn class_from_schema_node(
    node: &Node,
    file: &FileNode,
    repo_name: &str,
) -> Option<(String, ClassNode)> {
    match node {
        Node::Class(value) => Some((
            value.id().as_str().to_string(),
            class_node(repo_name, value.name(), value.source_range(), "class", file),
        )),
        Node::Struct(value) => Some((
            value.id().as_str().to_string(),
            class_node(
                repo_name,
                value.name(),
                value.source_range(),
                "struct",
                file,
            ),
        )),
        Node::Interface(value) => Some((
            value.id().as_str().to_string(),
            class_node(
                repo_name,
                value.name(),
                value.source_range(),
                "interface",
                file,
            ),
        )),
        Node::Trait(value) => Some((
            value.id().as_str().to_string(),
            class_node(repo_name, value.name(), value.source_range(), "trait", file),
        )),
        Node::Enum(value) => Some((
            value.id().as_str().to_string(),
            class_node(repo_name, value.name(), value.source_range(), "enum", file),
        )),
        Node::TypeAlias(value) => Some((
            value.id().as_str().to_string(),
            class_node(repo_name, value.name(), value.source_range(), "type", file),
        )),
        _ => None,
    }
}

fn class_node(
    repo_name: &str,
    name: &str,
    source_range: SourceRange,
    kind: &str,
    file: &FileNode,
) -> ClassNode {
    let file_path = InternedStr::from(file.path.clone());
    ClassNode::new(
        repo_name,
        InternedStr::from(file.id.clone()),
        file_path,
        file.area_id.clone().map(InternedStr::from),
        InternedStr::from(
            file.language
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        ),
        InternedStr::from(name),
        source_range.start_line() as usize,
        InternedStr::from(format!("{kind} {name}")),
    )
}

fn function_from_schema_node(
    node: &Node,
    file: &FileNode,
    repo_name: &str,
    model_id_by_schema: &HashMap<String, String>,
) -> Option<(String, FunctionNode)> {
    match node {
        Node::Function(value) => Some((
            value.id().as_str().to_string(),
            function_node(
                repo_name,
                value.name(),
                value.signature(),
                value.source_range(),
                None,
                file,
            ),
        )),
        Node::Method(value) => Some((
            value.id().as_str().to_string(),
            function_node(
                repo_name,
                value.name(),
                value.signature(),
                value.source_range(),
                model_id_by_schema
                    .get(value.receiver_type().as_str())
                    .cloned()
                    .map(InternedStr::from),
                file,
            ),
        )),
        Node::Lambda(value) => Some((
            value.id().as_str().to_string(),
            function_node(
                repo_name,
                value.name(),
                value.signature(),
                value.source_range(),
                model_id_by_schema
                    .get(value.enclosing_callable_id().as_str())
                    .cloned()
                    .map(InternedStr::from),
                file,
            ),
        )),
        _ => None,
    }
}

fn function_node(
    repo_name: &str,
    name: &str,
    signature: &str,
    source_range: SourceRange,
    parent_class_id: Option<InternedStr>,
    file: &FileNode,
) -> FunctionNode {
    let file_path = InternedStr::from(file.path.clone());
    FunctionNode::new(
        repo_name,
        InternedStr::from(file.id.clone()),
        file_path,
        file.area_id.clone().map(InternedStr::from),
        parent_class_id,
        InternedStr::from(
            file.language
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        ),
        InternedStr::from(name),
        source_range.start_line() as usize,
        InternedStr::from(signature),
    )
}

fn compatibility_symbols(classes: &[ClassNode], functions: &[FunctionNode]) -> Vec<Symbol> {
    let mut symbols = Vec::with_capacity(classes.len() + functions.len());
    for class in classes {
        symbols.push(
            Symbol::new(
                class.name.clone(),
                SymbolKind::Class,
                class.file_path.clone(),
                class.line,
                class.signature.clone(),
            )
            .with_context(Some(class.language.clone()), class.area_id.clone()),
        );
    }
    for function in functions {
        symbols.push(
            Symbol::new(
                function.name.clone(),
                SymbolKind::Function,
                function.file_path.clone(),
                function.line,
                function.signature.clone(),
            )
            .with_context(Some(function.language.clone()), function.area_id.clone()),
        );
    }
    symbols
}

fn compatibility_area_id(repo_name: &str, path: &str, top_level_dirs: &[String]) -> Option<String> {
    let first = path.split('/').next()?;
    if top_level_dirs.iter().any(|dir| dir == first) {
        Some(format!("area:{repo_name}:{first}"))
    } else {
        None
    }
}

fn compatibility_area_edges(
    repo_id: &str,
    areas: &[AreaNode],
    directories: &[DirectoryNode],
    files: &[FileNode],
) -> Vec<Edge> {
    let mut edges = Vec::new();
    for area in areas {
        edges.push(Edge::new(
            repo_id,
            &area.id,
            EdgeKind::Contains,
            1000,
            "structure",
        ));
    }
    for directory in directories {
        if let Some(area_id) = &directory.area_id {
            edges.push(Edge::new(
                area_id,
                &directory.id,
                EdgeKind::Contains,
                1000,
                "structure",
            ));
            edges.push(Edge::new(
                &directory.id,
                area_id,
                EdgeKind::BelongsTo,
                1000,
                "structure",
            ));
        }
    }
    for file in files {
        if let Some(area_id) = &file.area_id {
            edges.push(Edge::new(
                &file.id,
                area_id,
                EdgeKind::BelongsTo,
                1000,
                "structure",
            ));
        }
    }
    edges
}

fn compatibility_overlay_edges(docs: &[DocNode], configs: &[ConfigNode]) -> Vec<Edge> {
    let mut edges = Vec::with_capacity(docs.len() + configs.len());
    for doc in docs {
        edges.push(Edge::new(
            &doc.file_id,
            &doc.id,
            EdgeKind::Documents,
            900,
            "docs",
        ));
    }
    for config in configs {
        edges.push(Edge::new(
            &config.file_id,
            &config.id,
            EdgeKind::Configures,
            900,
            "config",
        ));
        if let Some(area_id) = &config.area_id {
            edges.push(Edge::new(
                &config.id,
                area_id,
                EdgeKind::Configures,
                700,
                "config",
            ));
        }
    }
    edges
}

fn schema_edge_to_model(
    edge: &aethyme_graph_schema::Edge,
    model_id_by_schema: &HashMap<String, String>,
) -> Edge {
    schema_edge_to_model_with_repo(edge, "", "", model_id_by_schema)
}

fn schema_edge_to_model_with_repo(
    edge: &aethyme_graph_schema::Edge,
    schema_repo_id: &str,
    model_repo_id: &str,
    model_id_by_schema: &HashMap<String, String>,
) -> Edge {
    let from = schema_endpoint_to_model_id(
        edge.src_id().as_str(),
        schema_repo_id,
        model_repo_id,
        model_id_by_schema,
    );
    let to = schema_endpoint_to_model_id(
        edge.dst_id().as_str(),
        schema_repo_id,
        model_repo_id,
        model_id_by_schema,
    );
    Edge::new(
        &from,
        &to,
        schema_edge_kind_to_model(edge.kind()),
        edge.confidence().as_milli(),
        edge.source().name(),
    )
}

fn schema_endpoint_to_model_id(
    schema_id: &str,
    schema_repo_id: &str,
    model_repo_id: &str,
    model_id_by_schema: &HashMap<String, String>,
) -> String {
    if !schema_repo_id.is_empty() && schema_id == schema_repo_id {
        model_repo_id.to_string()
    } else {
        model_id_by_schema
            .get(schema_id)
            .cloned()
            .unwrap_or_else(|| schema_id.to_string())
    }
}

fn schema_edge_kind_to_model(kind: SchemaEdgeKind) -> EdgeKind {
    match kind {
        SchemaEdgeKind::Contains => EdgeKind::Contains,
        SchemaEdgeKind::Defines => EdgeKind::Defines,
        SchemaEdgeKind::Imports => EdgeKind::Imports,
        SchemaEdgeKind::Calls => EdgeKind::Calls,
        SchemaEdgeKind::Configures => EdgeKind::Configures,
        SchemaEdgeKind::Documents => EdgeKind::Documents,
        SchemaEdgeKind::References
        | SchemaEdgeKind::Decides
        | SchemaEdgeKind::Deprecates
        | SchemaEdgeKind::Implements
        | SchemaEdgeKind::Inherits
        | SchemaEdgeKind::Reads
        | SchemaEdgeKind::Uses
        | SchemaEdgeKind::Writes
        | SchemaEdgeKind::Mocks
        | SchemaEdgeKind::Tests => EdgeKind::References,
    }
}

fn optional_language(language: &str) -> Option<String> {
    if language == "unknown" {
        None
    } else {
        Some(language.to_string())
    }
}

fn non_code_format_label(format: &NonCodeFormat) -> &'static str {
    match format {
        NonCodeFormat::Markdown => "markdown",
        NonCodeFormat::Yaml => "yaml",
        NonCodeFormat::Json => "json",
        NonCodeFormat::Toml => "toml",
        NonCodeFormat::Plain => "plain",
        NonCodeFormat::Other(_) => "other",
    }
}

fn leaf_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_generated(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("generated") || lower.ends_with(".min.js") || lower.ends_with(".lock")
}

fn classify_file_role(path: &str, language: Option<&str>, generated: bool) -> FileRole {
    if generated {
        return FileRole::Generated;
    }
    let lower = path.to_ascii_lowercase();
    if lower.contains("__pycache__")
        || lower.contains(".pytest_cache")
        || lower.contains(".mypy_cache")
    {
        return FileRole::Cache;
    }
    if lower.ends_with(".md")
        || lower.ends_with(".mdx")
        || lower.ends_with(".rst")
        || lower.ends_with("readme")
    {
        return FileRole::Doc;
    }
    if is_operational_config_path(&lower) {
        return FileRole::Config;
    }
    if lower.contains("test") || lower.contains("spec") {
        return FileRole::Test;
    }
    if language.is_some_and(is_code_language) {
        return FileRole::Source;
    }
    if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".svg")
        || lower.ends_with(".db")
        || lower.ends_with(".sqlite")
        || lower.ends_with(".tres")
        || lower.ends_with(".tscn")
    {
        return FileRole::Asset;
    }
    FileRole::Unknown
}

fn is_code_language(language: &str) -> bool {
    !matches!(
        language,
        "markdown" | "yaml" | "json" | "toml" | "plain" | "other"
    )
}

fn classify_config_type(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with("cargo.toml")
        || lower.ends_with("package.json")
        || lower.ends_with("pyproject.toml")
    {
        "manifest".to_string()
    } else if lower.ends_with("project.godot") {
        "project".to_string()
    } else if lower.ends_with("dockerfile")
        || lower.ends_with("docker-compose.yml")
        || lower.ends_with("docker-compose.yaml")
    {
        "runtime".to_string()
    } else if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        "yaml".to_string()
    } else if lower.ends_with(".toml") {
        "toml".to_string()
    } else if lower.ends_with(".json") {
        "json".to_string()
    } else {
        "config".to_string()
    }
}

fn classify_doc_type(path: &str, title: &str) -> String {
    let lower_path = path.to_ascii_lowercase();
    let lower_title = title.to_ascii_lowercase();
    if lower_path.ends_with("readme.md") {
        "readme".to_string()
    } else if lower_path.contains("architecture") || lower_title.contains("architecture") {
        "architecture".to_string()
    } else if lower_path.contains("guide") || lower_title.contains("guide") {
        "guide".to_string()
    } else if lower_path.contains("spec") || lower_title.contains("spec") {
        "spec".to_string()
    } else {
        "documentation".to_string()
    }
}

fn is_operational_config_path(lower_path: &str) -> bool {
    let file_name = lower_path.rsplit('/').next().unwrap_or(lower_path);

    if matches!(
        file_name,
        "cargo.toml"
            | "package.json"
            | "pyproject.toml"
            | "project.godot"
            | "dockerfile"
            | "docker-compose.yml"
            | "docker-compose.yaml"
            | "tsconfig.json"
            | "jsconfig.json"
            | "turbo.json"
            | "biome.json"
            | "deno.json"
            | "deno.jsonc"
            | "pnpm-workspace.yaml"
            | "pnpm-workspace.yml"
    ) {
        return true;
    }

    if file_name.starts_with(".env")
        || file_name.starts_with(".gitignore")
        || file_name.starts_with(".dockerignore")
        || file_name.starts_with(".npmrc")
        || file_name.starts_with(".yarnrc")
        || file_name.starts_with(".prettierrc")
        || file_name.starts_with(".eslintrc")
        || file_name.starts_with(".stylelintrc")
        || file_name.starts_with(".editorconfig")
    {
        return true;
    }

    if lower_path.starts_with(".github/workflows/")
        || lower_path.contains("/.github/workflows/")
        || lower_path.starts_with("config/")
        || lower_path.contains("/config/")
        || lower_path.starts_with("configs/")
        || lower_path.contains("/configs/")
        || lower_path.starts_with("deploy/")
        || lower_path.contains("/deploy/")
        || lower_path.starts_with("deployment/")
        || lower_path.contains("/deployment/")
        || lower_path.starts_with("infra/")
        || lower_path.contains("/infra/")
        || lower_path.starts_with("k8s/")
        || lower_path.contains("/k8s/")
        || lower_path.starts_with("helm/")
        || lower_path.contains("/helm/")
    {
        return lower_path.ends_with(".yaml")
            || lower_path.ends_with(".yml")
            || lower_path.ends_with(".toml")
            || lower_path.ends_with(".json");
    }

    lower_path.ends_with(".env")
}

#[cfg(test)]
fn ensure_test_fragments(root: &Path) -> Result<(), String> {
    if root.join(".aethyme").join("graph").is_dir() {
        return Ok(());
    }
    let canonical = root
        .canonicalize()
        .map_err(|err| format!("test repo canonicalize: {err}"))?;
    let repo_name = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("testrepo");
    aethyme_graph_storage::bootstrap_repo(&canonical, "test")
        .map_err(|err| format!("test repo bootstrap: {err}"))?;
    let ctx = aethyme_graph_indexer::IndexerContext::new(repo_name, canonical.clone(), "test")
        .map_err(|err| format!("test index context: {err}"))?;
    aethyme_graph_indexer::index_repo_to_disk(&ctx, &aethyme_graph_indexer::WalkOptions::default())
        .map_err(|err| format!("test index repo: {err}"))?;
    aethyme_graph_indexer::link_repo(&ctx).map_err(|err| format!("test link repo: {err}"))?;
    Ok(())
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

fn graph_annotations(map: &RepositoryMap) -> Vec<GraphAnnotation> {
    let mut annotations = Vec::new();
    let file_ids_by_path = map
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.id.clone()))
        .collect::<HashMap<_, _>>();
    annotations.reserve(map.risk_flags.len() + map.docs.len() + map.configs.len());
    for risk in &map.risk_flags {
        if let Some(file_id) = file_ids_by_path.get(risk.scope.as_str()) {
            let (confidence, level_str) = match risk.level {
                RiskLevel::High => (1000, "high"),
                RiskLevel::Medium => (800, "medium"),
                RiskLevel::Low => (600, "low"),
            };
            annotations.push(GraphAnnotation {
                target_id: file_id.clone(),
                kind: "risk".to_string(),
                value: format!("{:?}", risk.area).to_ascii_lowercase(),
                confidence,
                source: "risk-overlay".to_string(),
                reason: format!("{} ({})", risk.reason, level_str),
            });
        }
    }
    for doc in &map.docs {
        annotations.push(GraphAnnotation {
            target_id: doc.id.clone(),
            kind: "doc_type".to_string(),
            value: doc.doc_type.clone(),
            confidence: 900,
            source: "docs".to_string(),
            reason: format!("documentation classified as {}", doc.doc_type),
        });
    }
    for config in &map.configs {
        annotations.push(GraphAnnotation {
            target_id: config.id.clone(),
            kind: "config_type".to_string(),
            value: config.config_type.clone(),
            confidence: 900,
            source: "config".to_string(),
            reason: format!("configuration classified as {}", config.config_type),
        });
    }
    for edge in &map.edges {
        if matches!(edge.kind, EdgeKind::EntrypointFor) {
            annotations.push(GraphAnnotation {
                target_id: edge.from.to_string(),
                kind: "navigation".to_string(),
                value: "entrypoint".to_string(),
                confidence: edge.confidence,
                source: edge.source.to_string(),
                reason: "edge inferred as navigation entrypoint".to_string(),
            });
        }
    }
    annotations.sort();
    annotations.dedup();
    annotations
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
    let annotations = graph_annotations(map);
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
                .any(|stage| stage.name == "populate_from_fragments")
        );
        assert_eq!(profile.repo_files, map.snapshot.files.len());
        assert_eq!(profile.graph_nodes, map.graph.nodes.len());
        assert!(
            !root.join(".aethyme/parse_store.redb").exists(),
            "RepositoryMap builds must not recreate the deleted ParseStore"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
