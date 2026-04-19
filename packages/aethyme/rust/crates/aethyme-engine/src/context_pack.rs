use crate::graph::signals::GraphSignals;
use crate::model::risk::RiskFlag;
use crate::model::scope::ScopeBoundary;
use crate::model::task::TaskInput;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnchorKind {
    Symbol,
    File,
    Folder,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Anchor {
    pub kind: AnchorKind,
    pub id: String,
    pub file: Option<String>,
    pub reason: String,
}

impl Anchor {
    pub fn new(
        kind: AnchorKind,
        id: impl Into<String>,
        file: Option<impl Into<String>>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            id: id.into(),
            file: file.map(Into::into),
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DependencyEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImpactItem {
    pub symbol: String,
    pub file: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Snippet {
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoOverview {
    pub overview_docs: Vec<String>,
    pub code_areas: Vec<String>,
    pub reference_areas: Vec<String>,
    pub subareas: Vec<String>,
    pub entrypoints: Vec<String>,
    pub key_configs: Vec<String>,
    pub representative_code_files: Vec<String>,
    pub representative_docs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackSnapshot {
    pub languages: Vec<String>,
    pub top_level_dirs: Vec<String>,
    pub readme_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackSummary {
    pub snapshot: PackSnapshot,
    pub files_count: usize,
    pub functions_count: usize,
    pub classes_count: usize,
    pub docs_count: usize,
    pub configs_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub max_anchors: usize,
    pub max_files: usize,
    pub max_snippets: usize,
    pub dependency_depth: usize,
    pub impact_depth: usize,
    pub snippet_window: usize,
    pub content_budget: usize,
    pub max_content_files: usize,
    pub max_lines_per_file: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_anchors: 3,
            max_files: 5,
            max_snippets: 8,
            dependency_depth: 1,
            impact_depth: 1,
            snippet_window: 20,
            content_budget: 80_000,
            max_content_files: 15,
            max_lines_per_file: 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivationSummary {
    pub activated_node_count: usize,
    pub max_depth_reached: usize,
    pub top_activated: Vec<(String, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Confidence {
    pub anchor_confidence: f32,
    pub scope_confidence: f32,
}

impl Default for Confidence {
    fn default() -> Self {
        Self {
            anchor_confidence: 0.0,
            scope_confidence: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextPack {
    pub task: TaskInput,
    pub summary: PackSummary,
    pub signals: GraphSignals,
    pub overview: RepoOverview,
    pub anchors: Vec<Anchor>,
    pub in_scope: ScopeBoundary,
    pub out_of_scope: ScopeBoundary,
    pub dependencies: Vec<DependencyEdge>,
    pub impact: Vec<ImpactItem>,
    pub snippets: Vec<Snippet>,
    pub file_contents: Vec<FileContent>,
    pub risk_flags: Vec<RiskFlag>,
    pub navigation_order: Vec<String>,
    pub budget: Budget,
    pub confidence: Confidence,
    pub activation_summary: Option<ActivationSummary>,
}

impl ContextPack {
    pub fn sort(&mut self) {
        self.overview.overview_docs.sort();
        self.overview.code_areas.sort();
        self.overview.reference_areas.sort();
        self.overview.subareas.sort();
        self.overview.entrypoints.sort();
        self.overview.key_configs.sort();
        self.overview.representative_code_files.sort();
        self.overview.representative_docs.sort();
        self.in_scope.sort();
        self.out_of_scope.sort();
        self.dependencies.sort();
        self.impact.sort();
        self.snippets.sort();
        self.risk_flags.sort();
    }
}
