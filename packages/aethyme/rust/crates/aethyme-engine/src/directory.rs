#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DirectoryNode {
    pub id: String,
    pub path: String,
    pub name: String,
    pub area_id: Option<String>,
}

impl DirectoryNode {
    pub fn new(repo_name: &str, path: &str, area_id: Option<String>) -> Self {
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        Self {
            id: format!("dir:{repo_name}:{path}"),
            path: path.to_string(),
            name,
            area_id,
        }
    }
}
