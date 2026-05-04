use crate::model::intern::InternedStr;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum SymbolKind {
    Function,
    Class,
    Constant,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Symbol {
    pub id: InternedStr,
    pub name: InternedStr,
    pub kind: SymbolKind,
    pub file: InternedStr,
    pub line: usize,
    pub signature: InternedStr,
    pub language: Option<InternedStr>,
    pub area: Option<InternedStr>,
}

impl Symbol {
    pub fn new(
        name: impl Into<InternedStr>,
        kind: SymbolKind,
        file: impl Into<InternedStr>,
        line: usize,
        signature: impl Into<InternedStr>,
    ) -> Self {
        let resolved_name: InternedStr = name.into();
        let resolved_file: InternedStr = file.into();
        let id = InternedStr::from(format!("{}::{}::{}", resolved_file, line, resolved_name));
        Self {
            id,
            name: resolved_name,
            kind,
            file: resolved_file,
            line,
            signature: signature.into(),
            language: None,
            area: None,
        }
    }

    pub fn with_context(
        mut self,
        language: Option<InternedStr>,
        area: Option<InternedStr>,
    ) -> Self {
        self.language = language;
        self.area = area;
        self
    }
}
