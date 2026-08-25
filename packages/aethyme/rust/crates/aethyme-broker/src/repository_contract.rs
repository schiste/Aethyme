//! Repository deployment state pinned to a broker session.
//!
//! The Git adoption baseline answers "which commits does this session own?".
//! This module answers a different question: "which deployed Aethyme contract
//! did the broker accept when the session began?". Keeping the two snapshots
//! separate lets later compatibility policy continue an accepted session
//! without silently refreshing either authority boundary.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CANONICAL_REPOSITORY_MARKER_PATH: &str = ".aethyme/repository.json";
pub const LOCAL_REPOSITORY_MARKER_PATH: &str = ".aethyme/local/repository.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryDeploymentMode {
    Canonical,
    LocalOnly,
}

impl RepositoryDeploymentMode {
    pub fn marker_path(self) -> &'static str {
        match self {
            Self::Canonical => CANONICAL_REPOSITORY_MARKER_PATH,
            Self::LocalOnly => LOCAL_REPOSITORY_MARKER_PATH,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RepositoryMarker {
    schema_version: u32,
}

/// Immutable repository contract captured for one session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryContract {
    /// Repository deployment schema accepted by the broker, when enrolled.
    pub repository_schema: Option<u32>,
    /// Digest of every deployment-managed path, including missing/type state.
    pub deployment_state_digest: String,
    /// Binary version that captured this snapshot.
    pub aethyme_version: String,
    /// Digest of `.aethyme/gates.toml`, when that contract exists.
    pub gate_definition_digest: Option<String>,
    /// True when an upgraded broker captured the best available state for a
    /// session created before repository-contract pinning existed.
    pub backfilled: bool,
}

impl RepositoryContract {
    pub fn capture(repo: &Path, backfilled: bool) -> Result<Self, String> {
        let mode = detect_repository_mode(repo);
        Ok(Self {
            repository_schema: read_repository_schema(repo, mode)?,
            deployment_state_digest: repository_state_digest(repo, mode)?,
            aethyme_version: env!("CARGO_PKG_VERSION").to_string(),
            gate_definition_digest: file_digest_if_present(
                repo,
                crate::gates::GATES_CONFIG_RELPATH,
            )?,
            backfilled,
        })
    }
}

pub fn detect_repository_mode(repo: &Path) -> RepositoryDeploymentMode {
    if repo.join(LOCAL_REPOSITORY_MARKER_PATH).is_file()
        || repo
            .join(aethyme_enhance::local::LOCAL_MARKER_PATH)
            .is_file()
    {
        RepositoryDeploymentMode::LocalOnly
    } else {
        RepositoryDeploymentMode::Canonical
    }
}

pub fn repository_managed_paths(mode: RepositoryDeploymentMode) -> Vec<String> {
    let mut paths = BTreeSet::new();
    paths.extend([
        ".aethyme/config.toml".into(),
        crate::gates::GATES_CONFIG_RELPATH.into(),
        mode.marker_path().into(),
    ]);
    match mode {
        RepositoryDeploymentMode::Canonical => {
            paths.insert(".gitignore".into());
            paths.insert("AGENTS.md".into());
            paths.insert("CLAUDE.md".into());
            paths.extend(
                aethyme_enhance::deploy::TARGETS
                    .iter()
                    .map(|(path, _)| (*path).into()),
            );
        }
        RepositoryDeploymentMode::LocalOnly => {
            paths.insert(aethyme_enhance::local::LOCAL_MARKER_PATH.into());
            paths.insert(aethyme_enhance::local::LOCAL_POLICY_PATH.into());
            paths.extend(
                aethyme_enhance::deploy::TARGETS
                    .iter()
                    .filter(|(path, _)| *path != "CLAUDE.md")
                    .map(|(path, _)| (*path).into()),
            );
        }
    }
    paths.insert(aethyme_enhance::deploy::SETTINGS_FILE.into());
    paths.extend([
        aethyme_enhance::AGENTS_OVERRIDE_PATH.into(),
        aethyme_enhance::onboarding::ONBOARDING_JSON_PATH.into(),
        aethyme_enhance::onboarding::ACT_STARTER_JSON_PATH.into(),
        aethyme_enhance::onboarding::ONBOARDING_CLAUDE_PATH.into(),
        aethyme_enhance::onboarding::ONBOARDING_CODEX_PATH.into(),
        aethyme_enhance::onboarding::ACT_CLAUDE_PATH.into(),
        aethyme_enhance::onboarding::ACT_CODEX_PATH.into(),
    ]);
    paths.into_iter().collect()
}

pub fn repository_state_digest(
    repo: &Path,
    mode: RepositoryDeploymentMode,
) -> Result<String, String> {
    let mut hasher = Sha256::new();
    for relative in repository_managed_paths(mode) {
        hasher.update(relative.as_bytes());
        let path = repo.join(&relative);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => hasher.update(b"symlink"),
            Ok(metadata) if metadata.is_file() => {
                hasher.update(b"file");
                hasher
                    .update(std::fs::read(&path).map_err(|error| format!("{relative}: {error}"))?);
            }
            Ok(_) => hasher.update(b"other"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => hasher.update(b"missing"),
            Err(error) => return Err(format!("{relative}: {error}")),
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn read_repository_schema(
    repo: &Path,
    mode: RepositoryDeploymentMode,
) -> Result<Option<u32>, String> {
    let relative = mode.marker_path();
    let path = repo.join(relative);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{relative}: {error}")),
    };
    let marker: RepositoryMarker = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{relative} is not a valid repository marker: {error}"))?;
    Ok(Some(marker.schema_version))
}

fn file_digest_if_present(repo: &Path, relative: &str) -> Result<Option<String>, String> {
    let bytes = match std::fs::read(repo.join(relative)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{relative}: {error}")),
    };
    Ok(Some(format!("{:x}", Sha256::digest(bytes))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_and_gate_digests_change_independently() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
        std::fs::write(
            tmp.path().join(CANONICAL_REPOSITORY_MARKER_PATH),
            r#"{"schema_version":1,"applied_migrations":[]}"#,
        )
        .unwrap();
        let first = RepositoryContract::capture(tmp.path(), false).unwrap();

        std::fs::write(tmp.path().join(".aethyme/gates.toml"), "[[gate]]\n").unwrap();
        let gates_changed = RepositoryContract::capture(tmp.path(), false).unwrap();
        assert_ne!(
            first.deployment_state_digest,
            gates_changed.deployment_state_digest
        );
        assert_ne!(
            first.gate_definition_digest,
            gates_changed.gate_definition_digest
        );
        assert_eq!(gates_changed.repository_schema, Some(1));
        assert!(!gates_changed.backfilled);
    }

    #[test]
    fn missing_repository_marker_is_recorded_without_guessing_a_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let contract = RepositoryContract::capture(tmp.path(), true).unwrap();
        assert_eq!(contract.repository_schema, None);
        assert_eq!(contract.gate_definition_digest, None);
        assert!(contract.backfilled);
        assert_eq!(contract.deployment_state_digest.len(), 64);
    }
}
