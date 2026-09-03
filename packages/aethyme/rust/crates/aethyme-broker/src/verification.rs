//! Stable disposable checkouts for exact-tree verification.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use crate::{BrokerError, BrokerOpError, GitRepo};

/// One repository-local checkout serialized by an advisory file lock.
///
/// A stable path lets build tools reuse safe path-sensitive fingerprints.
/// Callers choose a distinct namespace when their verification lifetimes must
/// not contend with one another.
/// The file lock covers checkout materialization, gate execution, and cleanup.
pub(crate) struct ExactTreeVerificationSlot {
    repository_root: PathBuf,
    path: PathBuf,
    _lock: File,
}

impl ExactTreeVerificationSlot {
    pub(crate) fn acquire(main_root: &Path, namespace: &str) -> Result<Self, BrokerOpError> {
        let directory = main_root.join(".aethyme/run").join(namespace);
        std::fs::create_dir_all(&directory).map_err(|source| BrokerError::Io {
            path: directory.clone(),
            source,
        })?;
        let lock_path = directory.join("slot.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| BrokerError::Io {
                path: lock_path.clone(),
                source,
            })?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(BrokerError::Io {
                path: lock_path,
                source: std::io::Error::last_os_error(),
            }
            .into());
        }
        Ok(Self {
            repository_root: main_root.to_path_buf(),
            path: directory.join("slot"),
            _lock: lock,
        })
    }

    pub(crate) fn materialize(
        &mut self,
        repository: &GitRepo,
        commit: &str,
    ) -> Result<GitRepo, BrokerOpError> {
        self.cleanup();
        Ok(repository.worktree_add_detached(&self.path, commit)?)
    }

    pub(crate) fn cleanup(&mut self) {
        // Always try Git-level removal. After a crash, the common Git
        // directory can retain a registration created by another process.
        if let Ok(repository) = GitRepo::discover(&self.repository_root) {
            let _ = repository.worktree_remove(&self.path, true);
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl Drop for ExactTreeVerificationSlot {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_reuses_one_path_and_removes_stale_contents() {
        let root = tempfile::tempdir().unwrap();
        let expected = root.path().join(".aethyme/run/merge-sim/slot");
        {
            let mut first = ExactTreeVerificationSlot::acquire(root.path(), "merge-sim").unwrap();
            assert_eq!(first.path, expected);
            std::fs::create_dir_all(&first.path).unwrap();
            std::fs::write(first.path.join("stale"), "old run").unwrap();
            first.cleanup();
            assert!(!expected.exists());
        }
        let second = ExactTreeVerificationSlot::acquire(root.path(), "merge-sim").unwrap();
        assert_eq!(second.path, expected);
    }
}
