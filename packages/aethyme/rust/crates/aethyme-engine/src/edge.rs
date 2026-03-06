#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeKind {
    Contains,
    BelongsTo,
    Defines,
    Imports,
    Calls,
    References,
    Documents,
    Configures,
    EntrypointFor,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub confidence: u16,
    pub source: String,
}

impl Edge {
    pub fn new(
        from: impl Into<String>,
        to: impl Into<String>,
        kind: EdgeKind,
        confidence: u16,
        source: impl Into<String>,
    ) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            kind,
            confidence,
            source: source.into(),
        }
    }
}
