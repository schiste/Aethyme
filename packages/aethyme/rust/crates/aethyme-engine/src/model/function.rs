use crate::model::intern::InternedStr;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct FunctionNode {
    pub id: InternedStr,
    pub name: InternedStr,
    pub qualified_name: InternedStr,
    pub file_id: InternedStr,
    pub file_path: InternedStr,
    pub area_id: Option<InternedStr>,
    pub parent_class_id: Option<InternedStr>,
    pub language: InternedStr,
    pub line: usize,
    pub signature: InternedStr,
}

impl FunctionNode {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo_name: &str,
        file_id: InternedStr,
        file_path: InternedStr,
        area_id: Option<InternedStr>,
        parent_class_id: Option<InternedStr>,
        language: InternedStr,
        name: InternedStr,
        line: usize,
        signature: InternedStr,
    ) -> Self {
        let qualified_name = InternedStr::from(format!("{file_path}::{name}"));
        // The start line is part of the identity. Without it a method
        // and a module-level function sharing a name in one file, or two
        // `impl` blocks each declaring the same method name, mint the
        // same id for distinct symbols; the store's insert is an
        // upsert, so one silently overwrites the other.
        let id = InternedStr::from(format!("fn:{repo_name}:{file_path}:{name}@{line}"));
        Self {
            id,
            name,
            qualified_name,
            file_id,
            file_path,
            area_id,
            parent_class_id,
            language,
            line,
            signature,
        }
    }
}
