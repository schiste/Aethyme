#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AreaNode {
    pub id: String,
    pub name: String,
    pub path_prefix: String,
    pub inferred: bool,
}

impl AreaNode {
    pub fn new(repo_name: &str, path_prefix: &str, inferred: bool) -> Self {
        Self {
            id: format!("area:{repo_name}:{path_prefix}"),
            name: path_prefix.to_string(),
            path_prefix: path_prefix.to_string(),
            inferred,
        }
    }
}
