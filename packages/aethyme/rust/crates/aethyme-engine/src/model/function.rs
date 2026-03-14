#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct FunctionNode {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub file_id: String,
    pub file_path: String,
    pub area_id: Option<String>,
    pub parent_class_id: Option<String>,
    pub language: String,
    pub line: usize,
    pub signature: String,
}

impl FunctionNode {
    pub fn new(
        repo_name: &str,
        file_id: &str,
        file_path: &str,
        area_id: Option<String>,
        parent_class_id: Option<String>,
        language: &str,
        name: &str,
        line: usize,
        signature: &str,
    ) -> Self {
        let qualified_name = format!("{file_path}::{name}");
        Self {
            id: format!("fn:{repo_name}:{file_path}:{name}"),
            name: name.to_string(),
            qualified_name,
            file_id: file_id.to_string(),
            file_path: file_path.to_string(),
            area_id,
            parent_class_id,
            language: language.to_string(),
            line,
            signature: signature.to_string(),
        }
    }
}
