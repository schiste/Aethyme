//! Immutable, content-addressed cache for derived graph-store artifacts.
//!
//! Entries are opaque files. Callers open only a private copy, validate its
//! storage schema, and rebind worktree-local metadata before publication.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const GRAPH_STORE_CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphStoreCacheKey {
    pub source_tree_sha256: String,
    pub fragment_manifest_sha256: String,
    pub engine_version: String,
    pub engine_protocol_version: u32,
    pub storage_schema_version: u32,
}

impl GraphStoreCacheKey {
    pub fn digest(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        Ok(sha256_bytes(&bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedGraphStoreArtifact {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CacheMetadata {
    schema_version: u32,
    key: GraphStoreCacheKey,
    key_sha256: String,
    artifact_sha256: String,
    artifact_bytes: u64,
}

pub struct GraphStoreArtifactCache {
    root: PathBuf,
}

impl GraphStoreArtifactCache {
    pub fn for_environment(repo_root: &Path) -> Option<Self> {
        let explicit = std::env::var_os("AETHYME_HOST_CACHE_DIR").filter(|path| !path.is_empty());
        if explicit.is_none() && path_is_ephemeral(repo_root) {
            return None;
        }
        let base = explicit
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_CACHE_HOME")
                    .filter(|path| !path.is_empty())
                    .map(|path| PathBuf::from(path).join("aethyme"))
            })
            .or_else(|| {
                let home = PathBuf::from(std::env::var_os("HOME")?);
                Some(if cfg!(target_os = "macos") {
                    home.join("Library/Caches/Aethyme")
                } else {
                    home.join(".cache/aethyme")
                })
            })?;
        Some(Self::new(
            base.join("graph-stores")
                .join(format!("v{GRAPH_STORE_CACHE_SCHEMA_VERSION}")),
        ))
    }

    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn acquire(&self, key: GraphStoreCacheKey) -> Result<GraphStoreCacheEntry, String> {
        let key_sha256 = key.digest()?;
        let directory = self.root.join(&key_sha256);
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("create graph cache entry: {error}"))?;
        protect(&self.root, true)?;
        protect(&directory, true)?;
        let lock_path = directory.join("entry.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| format!("open graph cache lock: {error}"))?;
        protect(&lock_path, false)?;
        #[cfg(unix)]
        {
            let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
            if result != 0 {
                return Err(format!(
                    "lock graph cache entry: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        #[cfg(not(unix))]
        {
            return Err("graph store host caching requires Unix file locking".into());
        }
        Ok(GraphStoreCacheEntry {
            _lock: lock,
            directory,
            key,
            key_sha256,
        })
    }
}

pub struct GraphStoreCacheEntry {
    _lock: File,
    directory: PathBuf,
    key: GraphStoreCacheKey,
    key_sha256: String,
}

impl GraphStoreCacheEntry {
    pub fn key_sha256(&self) -> &str {
        &self.key_sha256
    }

    pub fn lookup(&self) -> Result<Option<CachedGraphStoreArtifact>, String> {
        let metadata_path = self.directory.join("metadata.json");
        let artifact_path = self.directory.join("graph_store.redb");
        let metadata_bytes = match read_regular_bounded(&metadata_path, 64 * 1024)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        let metadata = match serde_json::from_slice::<CacheMetadata>(&metadata_bytes) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(None),
        };
        if metadata.schema_version != GRAPH_STORE_CACHE_SCHEMA_VERSION
            || metadata.key != self.key
            || metadata.key_sha256 != self.key_sha256
        {
            return Ok(None);
        }
        let artifact_metadata = match std::fs::symlink_metadata(&artifact_path) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("inspect cached graph store: {error}")),
        };
        if artifact_metadata.len() != metadata.artifact_bytes {
            return Ok(None);
        }
        let artifact_sha256 = sha256_file(&artifact_path)?;
        if artifact_sha256 != metadata.artifact_sha256 {
            return Ok(None);
        }
        Ok(Some(CachedGraphStoreArtifact {
            path: artifact_path,
            bytes: metadata.artifact_bytes,
            sha256: artifact_sha256,
        }))
    }

    pub fn store(&self, source: &Path) -> Result<CachedGraphStoreArtifact, String> {
        let source_metadata = std::fs::symlink_metadata(source)
            .map_err(|error| format!("inspect graph store for caching: {error}"))?;
        if !source_metadata.file_type().is_file() {
            return Err("graph store cache source is not a regular file".into());
        }
        let suffix = format!("{}.{}", std::process::id(), now_nanos());
        let artifact_temp = self.directory.join(format!("artifact.{suffix}.tmp"));
        let metadata_temp = self.directory.join(format!("metadata.{suffix}.tmp"));
        let artifact_path = self.directory.join("graph_store.redb");
        let metadata_path = self.directory.join("metadata.json");

        copy_new(source, &artifact_temp)?;
        let artifact_sha256 = sha256_file(&artifact_temp)?;
        let artifact_bytes = std::fs::metadata(&artifact_temp)
            .map_err(|error| format!("inspect staged cached graph store: {error}"))?
            .len();
        let metadata = CacheMetadata {
            schema_version: GRAPH_STORE_CACHE_SCHEMA_VERSION,
            key: self.key.clone(),
            key_sha256: self.key_sha256.clone(),
            artifact_sha256: artifact_sha256.clone(),
            artifact_bytes,
        };
        let metadata_bytes = serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?;
        write_new(&metadata_temp, &metadata_bytes)?;
        replace_regular(&artifact_temp, &artifact_path)?;
        replace_regular(&metadata_temp, &metadata_path)?;
        sync_parent(&metadata_path)?;
        Ok(CachedGraphStoreArtifact {
            path: artifact_path,
            bytes: artifact_bytes,
            sha256: artifact_sha256,
        })
    }
}

fn read_regular_bounded(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= limit => metadata,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect graph cache metadata: {error}")),
    };
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|error| format!("read graph cache metadata: {error}"))?;
    Ok(Some(bytes))
}

fn copy_new(source: &Path, target: &Path) -> Result<(), String> {
    let mut input =
        File::open(source).map_err(|error| format!("open graph cache source: {error}"))?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)
        .map_err(|error| format!("create graph cache artifact: {error}"))?;
    protect(target, false)?;
    std::io::copy(&mut input, &mut output)
        .map_err(|error| format!("copy graph cache artifact: {error}"))?;
    output
        .sync_all()
        .map_err(|error| format!("sync graph cache artifact: {error}"))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("create graph cache metadata: {error}"))?;
    protect(path, false)?;
    file.write_all(bytes)
        .map_err(|error| format!("write graph cache metadata: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("sync graph cache metadata: {error}"))
}

fn replace_regular(source: &Path, target: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_file() => std::fs::remove_file(target)
            .map_err(|error| format!("replace graph cache file: {error}"))?,
        Ok(_) => return Err("graph cache destination is not a regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect graph cache destination: {error}")),
    }
    std::fs::rename(source, target).map_err(|error| format!("publish graph cache file: {error}"))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("open graph cache file: {error}"))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("hash graph cache file: {error}"))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn path_is_ephemeral(path: &Path) -> bool {
    let temp = std::env::temp_dir();
    let temp = std::fs::canonicalize(&temp).unwrap_or(temp);
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    path.starts_with(temp)
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn sync_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "graph cache file has no parent".to_string())?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync graph cache directory: {error}"))
}

#[cfg(unix)]
fn protect(path: &Path, directory: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if directory { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| format!("protect graph cache path: {error}"))
}

#[cfg(not(unix))]
fn protect(_path: &Path, _directory: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> GraphStoreCacheKey {
        GraphStoreCacheKey {
            source_tree_sha256: "a".repeat(64),
            fragment_manifest_sha256: "b".repeat(64),
            engine_version: "1.2.3".into(),
            engine_protocol_version: 4,
            storage_schema_version: 5,
        }
    }

    #[test]
    fn key_digest_binds_every_compatibility_dimension() {
        let original = key();
        let digest = original.digest().unwrap();
        let mut changed = original.clone();
        changed.storage_schema_version += 1;
        assert_ne!(digest, changed.digest().unwrap());
        changed = original.clone();
        changed.engine_protocol_version += 1;
        assert_ne!(digest, changed.digest().unwrap());
        changed = original.clone();
        changed.engine_version.push_str("-preview");
        assert_ne!(digest, changed.digest().unwrap());
        changed = original.clone();
        changed.fragment_manifest_sha256.replace_range(..1, "c");
        assert_ne!(digest, changed.digest().unwrap());
        changed = original;
        changed.source_tree_sha256.replace_range(..1, "d");
        assert_ne!(digest, changed.digest().unwrap());
    }

    #[test]
    fn cache_detects_artifact_tampering_and_never_returns_it() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.redb");
        std::fs::write(&source, b"verified-store").unwrap();
        let cache = GraphStoreArtifactCache::new(temp.path().join("cache"));
        let entry = cache.acquire(key()).unwrap();
        let stored = entry.store(&source).unwrap();
        assert_eq!(entry.lookup().unwrap().unwrap().sha256, stored.sha256);
        std::fs::write(&stored.path, b"tampered").unwrap();
        assert!(entry.lookup().unwrap().is_none());
    }
}
