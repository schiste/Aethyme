/// Addressable configuration fact from a persisted fragment. Values remain
/// hashed; no raw configuration content is exposed by this projection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct ConfigValueLocation {
    pub config_path: String,
    pub value_hash: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct ConfigNode {
    pub id: String,
    pub file_id: String,
    pub path: String,
    pub config_type: String,
    pub area_id: Option<String>,
    #[serde(default)]
    pub value: Option<ConfigValueLocation>,
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
            value: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_file_and_value_round_trip_through_storage_and_json() {
        let file = ConfigNode::new(
            "fixture",
            "file:config",
            "config/runtime.toml",
            "toml",
            None,
        );
        let mut value = file.clone();
        value.id.push_str("#value:test");
        value.value = Some(ConfigValueLocation {
            config_path: "server.handler".into(),
            value_hash: "a".repeat(64),
            start_line: 2,
            end_line: 2,
        });
        for node in [file, value] {
            let bytes = bincode::serialize(&node).unwrap();
            assert_eq!(bincode::deserialize::<ConfigNode>(&bytes).unwrap(), node);
            let json = serde_json::to_value(&node).unwrap();
            assert_eq!(json["value"].is_null(), node.value.is_none());
            assert_eq!(serde_json::from_value::<ConfigNode>(json).unwrap(), node);
        }
    }
}
