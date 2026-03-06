use crate::risk::RiskFlag;
use crate::scope::ScopeBoundary;
use crate::task::TaskInput;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub max_anchors: usize,
    pub max_files: usize,
    pub max_snippets: usize,
    pub dependency_depth: usize,
    pub impact_depth: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_anchors: 3,
            max_files: 5,
            max_snippets: 8,
            dependency_depth: 1,
            impact_depth: 1,
        }
    }
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
    pub anchors: Vec<Anchor>,
    pub in_scope: ScopeBoundary,
    pub out_of_scope: ScopeBoundary,
    pub dependencies: Vec<DependencyEdge>,
    pub impact: Vec<ImpactItem>,
    pub snippets: Vec<Snippet>,
    pub risk_flags: Vec<RiskFlag>,
    pub navigation_order: Vec<String>,
    pub budget: Budget,
    pub confidence: Confidence,
}

impl ContextPack {
    pub fn sort(&mut self) {
        self.in_scope.sort();
        self.out_of_scope.sort();
        self.dependencies.sort();
        self.impact.sort();
        self.snippets.sort();
        self.risk_flags.sort();
    }
}
