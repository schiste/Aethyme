//! Disk I/O for fragments and index shards.
//!
//! Wraps the pure byte functions in `binary` and `index_shard` with
//! filesystem operations: layout-derived paths, directory creation,
//! atomic writes (write to a sibling tempfile, then rename).
//!
//! Atomic write semantics matter because graph fragments live in
//! git: a partially-written `.bin` file from a crashed indexer
//! must not be staged. Writing to `<path>.tmp` then renaming to
//! `<path>` is the POSIX-portable way to make the file appear
//! atomically — git sees either the old version or the new
//! version, never half-written bytes.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::binary::{
    read_fragment_bytes, write_fragment_bytes, FragmentDecodeError,
    FragmentEncodeError,
};
use crate::fragment::Fragment;
use crate::index_shard::{
    read_index_shard_bytes, write_index_shard_bytes, IndexShardDecodeError,
    IndexShardEncodeError, SymbolRecord,
};
use crate::layout::{
    fragment_path, index_shard_path, validate_module_name,
    validate_source_path, InvalidPath,
};

/// Write a Fragment to its canonical location under `<repo>/.aethyme/
/// graph/<source_path>.bin`. Creates parent directories as needed.
/// Uses atomic-rename to make the file appear atomically.
pub fn write_fragment(
    repo_root: &Path,
    source_path: &str,
    fragment: &Fragment,
) -> Result<PathBuf, FragmentWriteError> {
    validate_source_path(source_path).map_err(FragmentWriteError::Path)?;
    let bytes = write_fragment_bytes(fragment)
        .map_err(FragmentWriteError::Encode)?;
    let target = fragment_path(repo_root, source_path);
    atomic_write(&target, &bytes).map_err(FragmentWriteError::Io)?;
    Ok(target)
}

/// Read a Fragment from its canonical location.
pub fn read_fragment(
    repo_root: &Path,
    source_path: &str,
) -> Result<Fragment, FragmentReadError> {
    validate_source_path(source_path).map_err(FragmentReadError::Path)?;
    let target = fragment_path(repo_root, source_path);
    let bytes = read_file(&target).map_err(FragmentReadError::Io)?;
    read_fragment_bytes(&bytes).map_err(FragmentReadError::Decode)
}

/// Write an index shard to its canonical location under `<repo>/
/// .aethyme/graph/_index/<module>.ndjson`. Creates parent
/// directories as needed; atomic-renames into place.
pub fn write_index_shard(
    repo_root: &Path,
    module: &str,
    records: &[SymbolRecord],
) -> Result<PathBuf, IndexShardWriteError> {
    validate_module_name(module).map_err(IndexShardWriteError::Path)?;
    let bytes = write_index_shard_bytes(records)
        .map_err(IndexShardWriteError::Encode)?;
    let target = index_shard_path(repo_root, module);
    atomic_write(&target, &bytes).map_err(IndexShardWriteError::Io)?;
    Ok(target)
}

/// Read an index shard from its canonical location.
pub fn read_index_shard(
    repo_root: &Path,
    module: &str,
) -> Result<Vec<SymbolRecord>, IndexShardReadError> {
    validate_module_name(module).map_err(IndexShardReadError::Path)?;
    let target = index_shard_path(repo_root, module);
    let bytes = read_file(&target).map_err(IndexShardReadError::Io)?;
    read_index_shard_bytes(&bytes).map_err(IndexShardReadError::Decode)
}

// ─── Internal helpers ────────────────────────────────────────────────

/// Write `bytes` to `target` atomically by writing to a sibling
/// tempfile first and then renaming.
///
/// The tempfile sits next to the target (not in the system temp
/// dir) so the rename is guaranteed to be within the same
/// filesystem — `fs::rename` is only atomic across same-filesystem
/// renames on POSIX.
fn atomic_write(target: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let pid = std::process::id();
    // Use a deterministic-but-collision-resistant tmp name. The
    // file path + PID is sufficient: two indexers writing the
    // same target file is itself a bug.
    let mut tmp = target.as_os_str().to_owned();
    tmp.push(format!(".tmp.{pid}"));
    let tmp = PathBuf::from(tmp);
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, target)
}

fn read_file(target: &Path) -> io::Result<Vec<u8>> {
    let mut f = fs::File::open(target)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    Ok(bytes)
}

// ─── Error types ─────────────────────────────────────────────────────

#[derive(Debug)]
pub enum FragmentWriteError {
    Path(InvalidPath),
    Encode(FragmentEncodeError),
    Io(io::Error),
}

impl std::fmt::Display for FragmentWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(e) => write!(f, "fragment write: {e}"),
            Self::Encode(e) => write!(f, "fragment write: {e}"),
            Self::Io(e) => write!(f, "fragment write I/O: {e}"),
        }
    }
}

impl std::error::Error for FragmentWriteError {}

#[derive(Debug)]
pub enum FragmentReadError {
    Path(InvalidPath),
    Decode(FragmentDecodeError),
    Io(io::Error),
}

impl std::fmt::Display for FragmentReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(e) => write!(f, "fragment read: {e}"),
            Self::Decode(e) => write!(f, "fragment read: {e}"),
            Self::Io(e) => write!(f, "fragment read I/O: {e}"),
        }
    }
}

impl std::error::Error for FragmentReadError {}

#[derive(Debug)]
pub enum IndexShardWriteError {
    Path(InvalidPath),
    Encode(IndexShardEncodeError),
    Io(io::Error),
}

impl std::fmt::Display for IndexShardWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(e) => write!(f, "index shard write: {e}"),
            Self::Encode(e) => write!(f, "index shard write: {e}"),
            Self::Io(e) => write!(f, "index shard write I/O: {e}"),
        }
    }
}

impl std::error::Error for IndexShardWriteError {}

#[derive(Debug)]
pub enum IndexShardReadError {
    Path(InvalidPath),
    Decode(IndexShardDecodeError),
    Io(io::Error),
}

impl std::fmt::Display for IndexShardReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(e) => write!(f, "index shard read: {e}"),
            Self::Decode(e) => write!(f, "index shard read: {e}"),
            Self::Io(e) => write!(f, "index shard read I/O: {e}"),
        }
    }
}

impl std::error::Error for IndexShardReadError {}
