#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScopeKind {
    File,
    Folder,
    Symbol,
    Area,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopeItem {
    pub value: String,
    pub kind: ScopeKind,
    pub reason: String,
}

impl ScopeItem {
    pub fn new(value: impl Into<String>, kind: ScopeKind, reason: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            kind,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeBoundary {
    pub files: Vec<ScopeItem>,
    pub symbols: Vec<ScopeItem>,
    pub areas: Vec<ScopeItem>,
}

impl ScopeBoundary {
    pub fn sort(&mut self) {
        self.files.sort();
        self.symbols.sort();
        self.areas.sort();
    }
}
