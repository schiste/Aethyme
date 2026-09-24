//! Read-side result types: stored nodes, displays, symbol candidates,
//! Surface/Flow candidates, relations and overview slices.

use super::*;

// ── Read primitives ─────────────────────────────────────────────────────────
// Mirror the surface of `super::super::read` (live functions only —
// `subgraph` and `files_in_area` were dead code in the SurrealDB version
// and are not ported).

/// Top-level overview returned from `GraphStore::overview`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    pub repo: Option<RepoMetadata>,
    pub areas: Vec<AreaNode>,
    pub entrypoint_paths: Vec<String>,
    pub risks: Vec<RiskFlag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StoredNodeKind {
    Repository,
    Directory,
    File,
    Area,
    Function,
    Class,
    Doc,
    Config,
    BehaviorTestSurface,
    CliSurface,
    CredentialOperation,
    JobSurface,
    MiddlewareInstallation,
    ProxySurface,
    QueueSurface,
    RouteSurface,
    WebhookSurface,
    WorkerSurface,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredNode {
    Repository(RepositoryNode),
    Directory(DirectoryNode),
    File(FileNode),
    Area(AreaNode),
    Function(FunctionNode),
    Class(ClassNode),
    Doc(DocNode),
    Config(ConfigNode),
    Surface(SurfaceNode),
    Unresolved(UnresolvedNode),
}

impl StoredNode {
    pub fn id(&self) -> &str {
        match self {
            Self::Repository(node) => &node.id,
            Self::Directory(node) => &node.id,
            Self::File(node) => &node.id,
            Self::Area(node) => &node.id,
            Self::Function(node) => node.id.as_str(),
            Self::Class(node) => node.id.as_str(),
            Self::Doc(node) => &node.id,
            Self::Config(node) => &node.id,
            Self::Surface(node) => node.id.as_str(),
            Self::Unresolved(node) => node.id.as_str(),
        }
    }

    pub fn kind(&self) -> StoredNodeKind {
        match self {
            Self::Repository(_) => StoredNodeKind::Repository,
            Self::Directory(_) => StoredNodeKind::Directory,
            Self::File(_) => StoredNodeKind::File,
            Self::Area(_) => StoredNodeKind::Area,
            Self::Function(_) => StoredNodeKind::Function,
            Self::Class(_) => StoredNodeKind::Class,
            Self::Doc(_) => StoredNodeKind::Doc,
            Self::Config(_) => StoredNodeKind::Config,
            Self::Surface(node) => stored_kind_from_surface_kind(node.kind),
            Self::Unresolved(_) => StoredNodeKind::Unresolved,
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Repository(node) => Some(&node.root_path),
            Self::Directory(node) => Some(&node.path),
            Self::File(node) => Some(&node.path),
            Self::Area(node) => Some(&node.path_prefix),
            Self::Function(node) => Some(node.file_path.as_str()),
            Self::Class(node) => Some(node.file_path.as_str()),
            Self::Doc(node) => Some(&node.path),
            Self::Config(node) => Some(&node.path),
            Self::Surface(node) => Some(node.file_path.as_str()),
            Self::Unresolved(node) => Some(node.file_path.as_str()),
        }
    }
}

pub(super) fn stored_kind_from_surface_kind(kind: SurfaceKind) -> StoredNodeKind {
    match kind {
        SurfaceKind::BehaviorTestSurface => StoredNodeKind::BehaviorTestSurface,
        SurfaceKind::CliSurface => StoredNodeKind::CliSurface,
        SurfaceKind::CredentialOperation => StoredNodeKind::CredentialOperation,
        SurfaceKind::JobSurface => StoredNodeKind::JobSurface,
        SurfaceKind::MiddlewareInstallation => StoredNodeKind::MiddlewareInstallation,
        SurfaceKind::ProxySurface => StoredNodeKind::ProxySurface,
        SurfaceKind::QueueSurface => StoredNodeKind::QueueSurface,
        SurfaceKind::RouteSurface => StoredNodeKind::RouteSurface,
        SurfaceKind::WebhookSurface => StoredNodeKind::WebhookSurface,
        SurfaceKind::WorkerSurface => StoredNodeKind::WorkerSurface,
    }
}

pub(super) fn is_surface_stored_kind(kind: StoredNodeKind) -> bool {
    matches!(
        kind,
        StoredNodeKind::BehaviorTestSurface
            | StoredNodeKind::CliSurface
            | StoredNodeKind::CredentialOperation
            | StoredNodeKind::JobSurface
            | StoredNodeKind::MiddlewareInstallation
            | StoredNodeKind::ProxySurface
            | StoredNodeKind::QueueSurface
            | StoredNodeKind::RouteSurface
            | StoredNodeKind::WebhookSurface
            | StoredNodeKind::WorkerSurface
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolLookup {
    pub id: String,
    pub kind: StoredNodeKind,
    pub name: String,
    pub path: String,
    pub line: usize,
    pub signature: String,
    pub language: String,
    pub area_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeDisplay {
    pub id: String,
    pub kind: StoredNodeKind,
    pub display: String,
    pub name: String,
    pub path: Option<String>,
    pub language: Option<String>,
    pub area_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphRelation {
    Children,
    Parents,
    Callers,
    Callees,
    Docs,
    Configs,
    Imports,
    Importers,
    References,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedbRelationItem {
    pub node: NodeDisplay,
    pub relation: String,
    pub edge_kind: EdgeKind,
    pub confidence: u16,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedbRelationView {
    pub target: Option<NodeDisplay>,
    pub relation: GraphRelation,
    pub items: Vec<RedbRelationItem>,
}

pub(super) const MIN_STEM_LEN: usize = 4;
pub(super) const NAME_BASE: i32 = 100;
pub(super) const NAME_COMPOUND_PER_EXTRA: i32 = 150;
pub(super) const PATH_PER_TOKEN: i32 = 60;
pub(super) const AREA_PER_TOKEN: i32 = 40;
pub(super) const BASENAME_EXACT_BONUS: i32 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SymbolMatchSignals {
    pub exact: bool,
    pub case_insensitive: bool,
    pub prefix: bool,
    pub component: bool,
    pub path: bool,
    pub area: bool,
    pub basename: bool,
}

impl SymbolMatchSignals {
    pub(super) fn signal_count(&self) -> u8 {
        self.exact as u8
            + self.case_insensitive as u8
            + self.prefix as u8
            + self.component as u8
            + self.path as u8
            + self.area as u8
            + self.basename as u8
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.exact |= other.exact;
        self.case_insensitive |= other.case_insensitive;
        self.prefix |= other.prefix;
        self.component |= other.component;
        self.path |= other.path;
        self.area |= other.area;
        self.basename |= other.basename;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolCandidate {
    pub symbol: SymbolLookup,
    pub signals: SymbolMatchSignals,
    pub rank: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolMatchOptions {
    pub limit: usize,
    pub kind: Option<StoredNodeKind>,
    pub path_prefix: Option<String>,
    pub area_id: Option<String>,
}

impl Default for SymbolMatchOptions {
    fn default() -> Self {
        Self {
            limit: 50,
            kind: None,
            path_prefix: None,
            area_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAnchorCandidate {
    pub node: NodeDisplay,
    pub signals: SymbolMatchSignals,
    pub matched_tokens: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageBoundaryCandidate {
    pub node: NodeDisplay,
    pub symbol: Option<SymbolLookup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceFlowCandidate {
    pub node: NodeDisplay,
    pub signals: SymbolMatchSignals,
    pub matched_tokens: Vec<String>,
    pub relation_kinds: Vec<EdgeKind>,
    pub rank: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfacePathCandidate {
    pub path: String,
    pub surfaces: Vec<NodeDisplay>,
    pub matched_tokens: Vec<String>,
    pub relation_kinds: Vec<EdgeKind>,
    pub rank: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowRelationStep {
    pub from: NodeDisplay,
    pub to: NodeDisplay,
    pub edge_kind: EdgeKind,
    pub confidence: u16,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowChain {
    pub roots: Vec<NodeDisplay>,
    pub steps: Vec<FlowRelationStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsystemCandidate {
    pub id: Option<String>,
    pub path_prefix: String,
    pub matched_tokens: Vec<String>,
    pub nodes: Vec<NodeDisplay>,
    pub rank: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskClassCoverage {
    pub task_class: String,
    pub tokens: Vec<String>,
    pub entrypoints: Vec<SurfaceFlowCandidate>,
    pub surface_paths: Vec<SurfacePathCandidate>,
    pub credential_flows: Vec<SurfaceFlowCandidate>,
    pub subsystems: Vec<SubsystemCandidate>,
    pub tests: Vec<NodeDisplay>,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeighborDirection {
    Outgoing,
    Incoming,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewV2Limits {
    pub area_limit: usize,
    pub directory_limit: usize,
    pub entrypoint_limit: usize,
    pub risk_limit: usize,
    pub file_limit: usize,
    pub function_limit: usize,
    pub class_limit: usize,
    pub doc_limit: usize,
    pub config_limit: usize,
    pub surface_limit: usize,
    pub unresolved_limit: usize,
}

impl Default for OverviewV2Limits {
    fn default() -> Self {
        Self {
            area_limit: 20,
            directory_limit: 20,
            entrypoint_limit: 10,
            risk_limit: 20,
            file_limit: 20,
            function_limit: 20,
            class_limit: 20,
            doc_limit: 10,
            config_limit: 10,
            surface_limit: 20,
            unresolved_limit: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewV2 {
    pub repo: Option<RepoMetadata>,
    pub repository: Option<RepositoryNode>,
    pub areas: Vec<AreaNode>,
    pub directories: Vec<DirectoryNode>,
    pub entrypoint_paths: Vec<String>,
    pub risks: Vec<RiskFlag>,
    pub files: Vec<FileNode>,
    pub functions: Vec<FunctionNode>,
    pub classes: Vec<ClassNode>,
    pub docs: Vec<DocNode>,
    pub configs: Vec<ConfigNode>,
    pub surfaces: Vec<SurfaceNode>,
    pub unresolved: Vec<UnresolvedNode>,
}
