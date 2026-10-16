//! Fail-closed API for platforms without the descriptor-relative rotation
//! recovery primitives used by `preparation_rotation_debt.rs`.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DirectoryIdentity {
    device: u64,
    inode: u64,
    filesystem_id: String,
}

impl DirectoryIdentity {
    pub(crate) fn capture(_path: &Path) -> io::Result<Self> {
        Err(unsupported())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RotationDebt {
    schema: String,
    version: u8,
    repository: String,
    transaction: String,
    cache_root: DirectoryIdentity,
    original_store: DirectoryIdentity,
    retired_component: String,
}

impl RotationDebt {
    pub(crate) fn new(
        _repository: &str,
        _transaction: &str,
        _cache_root: DirectoryIdentity,
        _original_store: DirectoryIdentity,
    ) -> io::Result<Self> {
        Err(unsupported())
    }

    pub(crate) fn repository(&self) -> &str {
        &self.repository
    }

    pub(crate) fn retired_component(&self) -> &str {
        &self.retired_component
    }

    pub(crate) fn cache_root(&self) -> &DirectoryIdentity {
        &self.cache_root
    }

    pub(crate) fn original_store(&self) -> &DirectoryIdentity {
        &self.original_store
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryStep {
    RenameOriginalStore,
    RemoveRetiredStore,
    CreateFreshStore,
    ClearCompletedDebt,
    Conflict,
}

#[derive(Debug, Clone)]
pub(crate) struct RetainedRotationDebt {
    pub(crate) debt: RotationDebt,
}

pub(crate) fn ensure_supported() -> io::Result<()> {
    Err(unsupported())
}

pub(crate) fn recovery_step(_cache_root: &Path, _debt: &RotationDebt) -> io::Result<RecoveryStep> {
    Err(unsupported())
}

pub(crate) fn read(
    _cache_root: &Path,
    _repository: &str,
) -> io::Result<Option<RetainedRotationDebt>> {
    Err(unsupported())
}

pub(crate) fn publish(
    _cache_root: &Path,
    _proposed: RotationDebt,
) -> io::Result<RetainedRotationDebt> {
    Err(unsupported())
}

pub(crate) fn clear_completed(
    _cache_root: &Path,
    _retained: &RetainedRotationDebt,
) -> io::Result<()> {
    Err(unsupported())
}

static NEXT_TRANSACTION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn new_transaction_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default();
    format!(
        "{nanos:016x}{:08x}{:08x}",
        std::process::id(),
        NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed) as u32
    )
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "preparation-cache rotation recovery requires Linux or macOS descriptor-relative filesystem APIs",
    )
}
