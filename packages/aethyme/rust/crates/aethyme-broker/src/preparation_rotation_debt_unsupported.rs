//! Fail-closed inventory API for platforms without the descriptor-relative
//! rotation recovery primitives used by `preparation_rotation_debt.rs`.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DebtInventory {
    pub(crate) repositories: Vec<String>,
    pub(crate) blocked_repositories: BTreeSet<String>,
    pub(crate) complete: bool,
    pub(crate) block_legacy_retired: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RecoveryProgress {
    pub(crate) found_debt: bool,
    pub(crate) complete: bool,
    pub(crate) bytes_reclaimed: u64,
    pub(crate) entries_reclaimed: usize,
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

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "preparation-cache rotation recovery requires Linux or macOS descriptor-relative filesystem APIs",
    )
}
