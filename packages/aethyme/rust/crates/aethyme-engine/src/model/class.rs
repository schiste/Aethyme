use crate::model::intern::InternedStr;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct ClassNode {
    pub id: InternedStr,
    pub name: InternedStr,
    pub qualified_name: InternedStr,
    pub file_id: InternedStr,
    pub file_path: InternedStr,
    pub area_id: Option<InternedStr>,
    pub language: InternedStr,
    pub line: usize,
    pub signature: InternedStr,
}

impl ClassNode {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo_name: &str,
        file_id: InternedStr,
        file_path: InternedStr,
        area_id: Option<InternedStr>,
        language: InternedStr,
        name: InternedStr,
        line: usize,
        signature: InternedStr,
    ) -> Self {
        let qualified_name = InternedStr::from(format!("{file_path}::{name}"));
        // The start line is part of the identity. Without it a file
        // declaring `struct Foo` and `trait Foo`, or two `impl` blocks
        // each declaring `fn new`, mints the same id for distinct
        // symbols; the store's insert is an upsert, so one silently
        // overwrites the other and every edge through it becomes
        // ambiguous.
        let id = InternedStr::from(format!("class:{repo_name}:{file_path}:{name}@{line}"));
        Self {
            id,
            name,
            qualified_name,
            file_id,
            file_path,
            area_id,
            language,
            line,
            signature,
        }
    }
}
