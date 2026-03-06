#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SymbolKind {
    Function,
    Class,
    Constant,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Symbol {
    pub id: String,
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line: usize,
    pub signature: String,
}

impl Symbol {
    pub fn new(
        name: impl Into<String>,
        kind: SymbolKind,
        file: impl Into<String>,
        line: usize,
        signature: impl Into<String>,
    ) -> Self {
        let resolved_name = name.into();
        let resolved_file = file.into();
        Self {
            id: format!("{}::{}::{}", resolved_file, line, resolved_name),
            name: resolved_name,
            kind,
            file: resolved_file,
            line,
            signature: signature.into(),
        }
    }
}
