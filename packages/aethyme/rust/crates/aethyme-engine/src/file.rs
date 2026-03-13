#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum FileRole {
    Source,
    Test,
    Doc,
    Config,
    Asset,
    Generated,
    Binary,
    Cache,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct FileNode {
    pub id: String,
    pub path: String,
    pub name: String,
    pub language: Option<String>,
    pub role: FileRole,
    pub line_count: usize,
    pub size_bytes: u64,
    pub generated: bool,
    pub area_id: Option<String>,
}

impl FileNode {
    pub fn new(
        repo_name: &str,
        path: &str,
        language: Option<String>,
        role: FileRole,
        line_count: usize,
        size_bytes: u64,
        generated: bool,
        area_id: Option<String>,
    ) -> Self {
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        Self {
            id: format!("file:{repo_name}:{path}"),
            path: path.to_string(),
            name,
            language,
            role,
            line_count,
            size_bytes,
            generated,
            area_id,
        }
    }
}
