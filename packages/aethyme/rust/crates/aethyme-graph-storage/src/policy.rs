//! Typed repository policy for authoritative committed graph fragments.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const GRAPH_CONFIG_RELPATH: &str = ".aethyme/config.toml";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphAuthority {
    #[default]
    Disabled,
    CommittedFragments,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GraphIntegrityPolicy {
    pub authority: GraphAuthority,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

impl GraphIntegrityPolicy {
    pub fn load(repo_root: &Path) -> Result<Self, GraphIntegrityPolicyError> {
        let path = repo_root.join(GRAPH_CONFIG_RELPATH);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(GraphIntegrityPolicyError::Read {
                    path,
                    message: error.to_string(),
                });
            }
        };
        Self::parse_at(&text, path)
    }

    /// Parse policy bytes obtained from an exact Git tree without consulting
    /// the invoking worktree.
    pub fn parse(text: &str) -> Result<Self, GraphIntegrityPolicyError> {
        Self::parse_at(text, PathBuf::from(GRAPH_CONFIG_RELPATH))
    }

    fn parse_at(text: &str, path: PathBuf) -> Result<Self, GraphIntegrityPolicyError> {
        let value =
            text.parse::<toml::Value>()
                .map_err(|error| GraphIntegrityPolicyError::Parse {
                    path: path.clone(),
                    message: error.to_string(),
                })?;
        let Some(graph) = value.get("graph") else {
            return Ok(Self::default());
        };
        let Some(table) = graph.as_table() else {
            return Err(GraphIntegrityPolicyError::Invalid {
                path,
                message: "[graph] must be a TOML table".into(),
            });
        };
        let Some(authority) = table.get("authority") else {
            return Ok(Self::default());
        };
        let Some(authority) = authority.as_str() else {
            return Err(GraphIntegrityPolicyError::Invalid {
                path,
                message: "graph.authority must be a string".into(),
            });
        };
        let authority = match authority {
            "disabled" => GraphAuthority::Disabled,
            "committed_fragments" => GraphAuthority::CommittedFragments,
            other => {
                return Err(GraphIntegrityPolicyError::Invalid {
                    path,
                    message: format!(
                        "unsupported graph.authority {other:?}; expected disabled or committed_fragments"
                    ),
                });
            }
        };
        let repository = table
            .get("repository")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| GraphIntegrityPolicyError::Invalid {
                        path: path.clone(),
                        message: "graph.repository must be a string".into(),
                    })
                    .and_then(|value| {
                        if value.is_empty() || value.contains(':') {
                            Err(GraphIntegrityPolicyError::Invalid {
                                path: path.clone(),
                                message: "graph.repository must be non-empty and contain no ':'"
                                    .into(),
                            })
                        } else {
                            Ok(value.to_string())
                        }
                    })
            })
            .transpose()?;
        if authority == GraphAuthority::CommittedFragments && repository.is_none() {
            return Err(GraphIntegrityPolicyError::Invalid {
                path,
                message: "graph.repository is required for committed_fragments authority".into(),
            });
        }
        Ok(Self {
            authority,
            repository,
        })
    }

    pub fn enforces_committed_fragments(&self) -> bool {
        self.authority == GraphAuthority::CommittedFragments
    }

    /// Stable cache/provenance identity for policy plus implementation.
    pub fn digest(&self) -> String {
        let authority = match self.authority {
            GraphAuthority::Disabled => "disabled",
            GraphAuthority::CommittedFragments => "committed_fragments",
        };
        let repository = self.repository.as_deref().unwrap_or("");
        format!(
            "{:x}",
            Sha256::digest(
                format!(
                    "graph-integrity-v1\nauthority={authority}\nrepository={repository}\nengine={}\n",
                    env!("CARGO_PKG_VERSION")
                )
                .as_bytes()
            )
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GraphIntegrityPolicyError {
    #[error("cannot read graph integrity policy at {path}: {message}")]
    Read { path: PathBuf, message: String },
    #[error("cannot parse graph integrity policy at {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("invalid graph integrity policy at {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_equivalent_toml_formatting() {
        for source in [
            "[graph]\nauthority='committed_fragments'\nrepository='owner/repo'\n",
            "[graph]\nauthority  =  \"committed_fragments\"\nrepository = \"owner/repo\"\n",
        ] {
            let policy = GraphIntegrityPolicy::parse(source).unwrap();
            assert!(policy.enforces_committed_fragments());
            assert_eq!(policy.repository.as_deref(), Some("owner/repo"));
        }
    }

    #[test]
    fn missing_policy_defaults_to_disabled() {
        let repo = tempfile::tempdir().unwrap();
        assert_eq!(
            GraphIntegrityPolicy::load(repo.path()).unwrap(),
            GraphIntegrityPolicy::default()
        );
    }
}
