//! Host trust for repository MCP files.
//!
//! A repository's `.aethyme/mcp.toml` arrives with a clone or a branch, so it
//! is not the owner's word. Its entries run only after the owner has run
//! `aethyme mcp trust` on that exact file: the repository identity and the
//! SHA-256 of the file's bytes are recorded in
//! `<host state>/mcp/trusted-repositories.json`. Any change to the file is a
//! different digest and needs trust again.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const TRUST_FILE: &str = "trusted-repositories.json";
/// Digests remembered per repository (the main checkout and branches may
/// each hold a trusted version).
const KEEP_PER_REPOSITORY: usize = 20;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TrustFile {
    #[serde(default)]
    repositories: BTreeMap<String, Vec<TrustRecord>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    pub sha256: String,
    pub path: PathBuf,
    pub trusted_at_ms: u64,
    /// How the owner approved: `terminal` (typed confirmation) or `digest`
    /// (`--yes --digest` matching the printed digest).
    #[serde(default)]
    pub approval: String,
}

/// SHA-256 of a repository file's bytes (not secret: the file is in Git).
pub fn file_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read(directory: &Path) -> TrustFile {
    std::fs::read_to_string(directory.join(TRUST_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Digests the host trusts for `identity`.
pub fn trusted_digests(directory: &Path, identity: &str) -> Vec<String> {
    read(directory)
        .repositories
        .get(identity)
        .map(|records| records.iter().map(|record| record.sha256.clone()).collect())
        .unwrap_or_default()
}

/// Lock file serializing trust updates, so concurrent runs lose nothing.
pub const TRUST_LOCK: &str = "trusted-repositories.lock";

/// Trust `sha256` of `path` for `identity`, recording how it was approved.
pub fn record(
    directory: &Path,
    identity: &str,
    sha256: &str,
    path: &Path,
    approval: &str,
) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    let lock = crate::service::transport::private_coordination_file(&directory.join(TRUST_LOCK))?;
    let _held = crate::file_lock::ExclusiveFileLock::acquire(lock)?;
    let mut file = read(directory);
    let records = file.repositories.entry(identity.to_string()).or_default();
    records.retain(|record| record.sha256 != sha256);
    records.push(TrustRecord {
        sha256: sha256.to_string(),
        path: path.to_path_buf(),
        trusted_at_ms: super::telemetry::now_ms(),
        approval: approval.to_string(),
    });
    let excess = records.len().saturating_sub(KEEP_PER_REPOSITORY);
    records.drain(..excess);
    let bytes = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&bytes)?;
    temporary
        .persist(directory.join(TRUST_FILE))
        .map_err(|error| error.error)?;
    Ok(())
}
