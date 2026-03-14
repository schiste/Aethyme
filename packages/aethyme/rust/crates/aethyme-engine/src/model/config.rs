#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct ConfigNode {
    pub id: String,
    pub file_id: String,
    pub path: String,
    pub config_type: String,
    pub area_id: Option<String>,
}

impl ConfigNode {
    pub fn new(
        repo_name: &str,
        file_id: &str,
        path: &str,
        config_type: &str,
        area_id: Option<String>,
    ) -> Self {
        Self {
            id: format!("config:{repo_name}:{path}"),
            file_id: file_id.to_string(),
            path: path.to_string(),
            config_type: config_type.to_string(),
            area_id,
        }
    }
}
