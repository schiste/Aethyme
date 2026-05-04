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
        let id = InternedStr::from(format!("class:{repo_name}:{file_path}:{name}"));
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
