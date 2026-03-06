#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeKind {
    Imports,
    Calls,
    Defines,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub confidence: EdgeConfidence,
}

impl Edge {
    pub fn new(
        from: impl Into<String>,
        to: impl Into<String>,
        kind: EdgeKind,
        confidence: EdgeConfidence,
    ) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            kind,
            confidence,
        }
    }
}
