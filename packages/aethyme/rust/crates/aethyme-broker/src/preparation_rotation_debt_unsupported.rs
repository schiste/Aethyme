//! Fail-closed inventory API for platforms without the descriptor-relative
//! rotation recovery primitives used by `preparation_rotation_debt.rs`.

use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::path::Path;
use std::time::Instant;

/// Platform placeholder for filesystem identity checks. Operations that need
/// descriptor-relative identity capture fail closed through `capture`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectoryIdentity;

impl DirectoryIdentity {
    pub(crate) fn capture(_path: &Path) -> io::Result<Self> {
        Err(unsupported())
    }

    pub(crate) fn encode(&self) -> io::Result<Vec<u8>> {
        Err(unsupported())
    }

    pub(crate) fn same_filesystem(&self, _other: &Self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DebtInventory {
    pub(crate) repositories: Vec<String>,
    pub(crate) blocked_repositories: BTreeSet<String>,
    pub(crate) complete: bool,
    pub(crate) block_legacy_retired: bool,
}

/// Fail-closed result for the bounded startup probe on unsupported platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DebtProbe {
    NoDebt,
    Pending,
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RecoveryProgress {
    pub(crate) found_debt: bool,
    pub(crate) complete: bool,
    pub(crate) bytes_reclaimed: u64,
    pub(crate) entries_reclaimed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparationCacheRootCaptureState {
    Uninitialized,
    Marked {
        coordinator_db_path: std::path::PathBuf,
        nonce: String,
        cache_root_identity: Option<Vec<u8>>,
    },
}

#[derive(Debug)]
pub(crate) struct PreparationCacheRootCapture {
    pub(crate) directory: File,
    pub(crate) identity: Vec<u8>,
    pub(crate) state: PreparationCacheRootCaptureState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitializedPreparationCacheRoot {
    pub(crate) nonce: String,
    pub(crate) root_identity: Vec<u8>,
    pub(crate) cache_root_identity: Vec<u8>,
    pub(crate) coordinator_db_path: std::path::PathBuf,
}

pub(crate) fn capture_preparation_cache_root(
    _path: &Path,
) -> io::Result<PreparationCacheRootCapture> {
    Err(unsupported())
}

pub(crate) fn ensure_preparation_cache_root_child(
    _parent_path: &Path,
    _child_name: &str,
    _expected_parent_identity: &[u8],
) -> io::Result<()> {
    Err(unsupported())
}

pub(crate) fn validate_preparation_cache_root_handle(
    _path: &Path,
    _pinned_directory: &File,
    _expected_root_identity: &[u8],
) -> io::Result<()> {
    Err(unsupported())
}

pub(crate) fn initialize_preparation_cache_root(
    _path: &Path,
    _expected_root_identity: &[u8],
    _pinned_directory: &File,
    _coordinator_db_path: &Path,
    _journal_has_rows_under_root: impl Fn() -> io::Result<bool>,
) -> io::Result<InitializedPreparationCacheRoot> {
    Err(unsupported())
}

#[derive(Debug)]
pub(crate) struct RecoveryError {
    pub(crate) source: io::Error,
    pub(crate) progress: RecoveryProgress,
}

pub(crate) fn inventory(
    _cache_root: &Path,
    _max_entries: usize,
    _deadline: Instant,
) -> io::Result<DebtInventory> {
    Err(unsupported())
}

pub(crate) fn probe(_cache_root: &Path, _max_entries: usize, _deadline: Instant) -> DebtProbe {
    DebtProbe::Unknown
}

pub(crate) fn cache_root_absent_under_observed_parent(
    _cache_root: &Path,
    _deadline: Instant,
) -> io::Result<bool> {
    Err(unsupported())
}

pub(crate) fn recover(
    _cache_root: &Path,
    _repository: &str,
    _deadline: Instant,
) -> Result<RecoveryProgress, RecoveryError> {
    Err(RecoveryError {
        source: unsupported(),
        progress: RecoveryProgress::default(),
    })
}

pub(crate) fn recover_with_root_guard(
    _cache_root: &Path,
    _repository: &str,
    _deadline: Instant,
    _validate_root: impl FnMut() -> io::Result<()>,
) -> Result<RecoveryProgress, RecoveryError> {
    Err(RecoveryError {
        source: unsupported(),
        progress: RecoveryProgress::default(),
    })
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "preparation-cache rotation recovery requires Linux or macOS descriptor-relative filesystem APIs",
    )
}
