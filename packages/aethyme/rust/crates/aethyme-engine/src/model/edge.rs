use crate::model::intern::InternedStr;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
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
    Authorizes,
    Exposes,
    ForwardsTo,
    InstallsMiddleware,
    IssuesCredential,
    StoresCredential,
    UsesCredential,
    ValidatesCredential,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Edge {
    pub from: InternedStr,
    pub to: InternedStr,
    pub kind: EdgeKind,
    pub confidence: u16,
    pub source: InternedStr,
}

impl Edge {
    pub fn new(
        from: impl Into<InternedStr>,
        to: impl Into<InternedStr>,
        kind: EdgeKind,
        confidence: u16,
        source: impl Into<InternedStr>,
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
