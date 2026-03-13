#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct DocNode {
    pub id: String,
    pub file_id: String,
    pub path: String,
    pub title: String,
    pub doc_type: String,
    pub area_id: Option<String>,
}

impl DocNode {
    pub fn new(
        repo_name: &str,
        file_id: &str,
        path: &str,
        title: &str,
        doc_type: &str,
        area_id: Option<String>,
    ) -> Self {
        Self {
            id: format!("doc:{repo_name}:{path}"),
            file_id: file_id.to_string(),
            path: path.to_string(),
            title: title.to_string(),
            doc_type: doc_type.to_string(),
            area_id,
        }
    }
}
