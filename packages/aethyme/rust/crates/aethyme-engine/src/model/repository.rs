#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct RepositoryNode {
    pub id: String,
    pub name: String,
    pub root_path: String,
}

impl RepositoryNode {
    pub fn new(name: &str, root_path: &str) -> Self {
        Self {
            id: format!("repo:{name}"),
            name: name.to_string(),
            root_path: root_path.to_string(),
        }
    }
}
