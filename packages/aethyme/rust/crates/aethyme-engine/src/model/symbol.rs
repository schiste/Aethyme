use std::sync::Arc;

use crate::model::intern::{arc_str, arc_str_opt};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum SymbolKind {
    Function,
    Class,
    Constant,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Symbol {
    #[serde(with = "arc_str")]
    pub id: Arc<str>,
    #[serde(with = "arc_str")]
    pub name: Arc<str>,
    pub kind: SymbolKind,
    #[serde(with = "arc_str")]
    pub file: Arc<str>,
    pub line: usize,
    #[serde(with = "arc_str")]
    pub signature: Arc<str>,
    #[serde(with = "arc_str_opt")]
    pub language: Option<Arc<str>>,
    #[serde(with = "arc_str_opt")]
    pub area: Option<Arc<str>>,
}

impl Symbol {
    pub fn new(
        name: impl Into<Arc<str>>,
        kind: SymbolKind,
        file: impl Into<Arc<str>>,
        line: usize,
        signature: impl Into<Arc<str>>,
    ) -> Self {
        let resolved_name: Arc<str> = name.into();
        let resolved_file: Arc<str> = file.into();
        let id: Arc<str> =
            Arc::from(format!("{}::{}::{}", resolved_file, line, resolved_name));
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

    pub fn with_context(mut self, language: Option<Arc<str>>, area: Option<Arc<str>>) -> Self {
        self.language = language;
        self.area = area;
        self
    }
}
