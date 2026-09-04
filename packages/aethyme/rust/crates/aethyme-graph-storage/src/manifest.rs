//! Repository-level provenance for committed graph fragments.
//!
//! The manifest binds generated graph bytes to the exact committed tree while
//! excluding the graph outputs themselves. Consumers can therefore validate
//! freshness from Git metadata and committed fragment bytes without parsing
//! repository source or regenerating the graph.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const GRAPH_MANIFEST_RELPATH: &str = ".aethyme/graph/manifest.json";
pub const GRAPH_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphAuthorityManifest {
    pub schema_version: u32,
    pub repository: String,
    pub engine_version: String,
    pub source_tree_sha256: String,
    pub fragment_set_sha256: String,
}

impl GraphAuthorityManifest {
    pub fn build(
        repo_root: &Path,
        revision: &str,
        repository: &str,
        engine_version: &str,
    ) -> Result<Self, GraphManifestError> {
        Ok(Self {
            schema_version: GRAPH_MANIFEST_SCHEMA_VERSION,
            repository: repository.to_string(),
            engine_version: engine_version.to_string(),
            source_tree_sha256: committed_source_tree_digest(repo_root, revision)?,
            fragment_set_sha256: filesystem_fragment_set_digest(repo_root)?,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, GraphManifestError> {
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| GraphManifestError::Encode(error.to_string()))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, GraphManifestError> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| GraphManifestError::Decode(error.to_string()))?;
        if manifest.schema_version != GRAPH_MANIFEST_SCHEMA_VERSION {
            return Err(GraphManifestError::UnsupportedSchema(
                manifest.schema_version,
            ));
        }
        Ok(manifest)
    }
}

pub fn write_graph_authority_manifest(
    repo_root: &Path,
    revision: &str,
    repository: &str,
    engine_version: &str,
) -> Result<GraphAuthorityManifest, GraphManifestError> {
    let manifest = GraphAuthorityManifest::build(repo_root, revision, repository, engine_version)?;
    let path = repo_root.join(GRAPH_MANIFEST_RELPATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| GraphManifestError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(&path, manifest.encode()?)
        .map_err(|source| GraphManifestError::Io { path, source })?;
    Ok(manifest)
}

/// Digest exact tracked inputs without reading or parsing source contents.
///
/// `git ls-tree` records bind path, mode, object kind, and content object ID.
/// Graph outputs are excluded so committing a regenerated manifest does not
/// invalidate itself.
pub fn committed_source_tree_digest(
    repo_root: &Path,
    revision: &str,
) -> Result<String, GraphManifestError> {
    let output = Command::new("git")
        .args(["ls-tree", "-r", "-z", revision])
        .current_dir(repo_root)
        .output()
        .map_err(|source| GraphManifestError::Io {
            path: repo_root.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(GraphManifestError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let mut hasher = Sha256::new();
    for record in output.stdout.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(GraphManifestError::InvalidGitTree)?;
        let path = &record[separator + 1..];
        if path.starts_with(b".aethyme/graph/") {
            continue;
        }
        hasher.update(record);
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn graph_fragment_set_digest<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str, &'a [u8])>,
) -> String {
    let mut entries = files
        .into_iter()
        .filter(|(path, _, _)| *path != GRAPH_MANIFEST_RELPATH)
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    let mut hasher = Sha256::new();
    for (path, mode, bytes) in entries {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(mode.as_bytes());
        hasher.update([0]);
        hasher.update(Sha256::digest(bytes));
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

fn filesystem_fragment_set_digest(repo_root: &Path) -> Result<String, GraphManifestError> {
    let graph_root = repo_root.join(".aethyme/graph");
    let mut pending = vec![graph_root.clone()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(&directory).map_err(|source| GraphManifestError::Io {
            path: directory.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| GraphManifestError::Io {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|source| GraphManifestError::Io {
                    path: path.clone(),
                    source,
                })?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(repo_root)
                    .map_err(|_| GraphManifestError::OutsideRepository(path.clone()))?
                    .to_str()
                    .ok_or_else(|| GraphManifestError::NonUtf8Path(path.clone()))?
                    .replace('\\', "/");
                if relative == GRAPH_MANIFEST_RELPATH {
                    continue;
                }
                let bytes = std::fs::read(&path).map_err(|source| GraphManifestError::Io {
                    path: path.clone(),
                    source,
                })?;
                files.push((relative, file_mode(&metadata), bytes));
            } else {
                return Err(GraphManifestError::NonRegularPath(path));
            }
        }
    }
    Ok(graph_fragment_set_digest(files.iter().map(
        |(path, mode, bytes)| (path.as_str(), mode.as_str(), bytes.as_slice()),
    )))
}

#[cfg(unix)]
fn file_mode(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 == 0 {
        "100644".into()
    } else {
        "100755".into()
    }
}

#[cfg(not(unix))]
fn file_mode(_metadata: &std::fs::Metadata) -> String {
    "100644".into()
}

#[derive(Debug, thiserror::Error)]
pub enum GraphManifestError {
    #[error("graph manifest I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("graph manifest Git inspection failed: {0}")]
    Git(String),
    #[error("graph manifest encountered an invalid Git tree record")]
    InvalidGitTree,
    #[error("graph manifest path is outside the repository: {0}")]
    OutsideRepository(PathBuf),
    #[error("graph manifest path is not UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("graph manifest output is not a regular file: {0}")]
    NonRegularPath(PathBuf),
    #[error("encode graph manifest: {0}")]
    Encode(String),
    #[error("decode graph manifest: {0}")]
    Decode(String),
    #[error("unsupported graph manifest schema {0}")]
    UnsupportedSchema(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "Aethyme Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Aethyme Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .unwrap();
        assert!(output.status.success());
    }

    #[test]
    fn source_digest_ignores_graph_outputs_but_tracks_repository_inputs() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join("source.rs"), "fn one() {}\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "source"]);
        let first = committed_source_tree_digest(repo.path(), "HEAD").unwrap();

        std::fs::create_dir_all(repo.path().join(".aethyme/graph")).unwrap();
        std::fs::write(repo.path().join(GRAPH_MANIFEST_RELPATH), "generated").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "graph"]);
        assert_eq!(
            committed_source_tree_digest(repo.path(), "HEAD").unwrap(),
            first
        );

        std::fs::write(repo.path().join("source.rs"), "fn two() {}\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "change source"]);
        assert_ne!(
            committed_source_tree_digest(repo.path(), "HEAD").unwrap(),
            first
        );
    }
}
