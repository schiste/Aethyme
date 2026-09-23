//! Exclusive advisory file locks, shared by every broker lock file.
//!
//! These are `std::fs::File::lock` locks, which on Unix are `flock(2)`: they
//! belong to the open file description, are advisory, and are released when
//! the last descriptor for that description closes. That is exactly what the
//! hand-written `libc::flock` copies this replaces relied on, so dropping the
//! guard (or the process dying) still releases the lock.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Open (creating if needed) a lock file without truncating it.
pub(crate) fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

/// An exclusive lock held on an open file until the guard drops.
#[derive(Debug)]
pub(crate) struct ExclusiveFileLock {
    file: File,
}

impl ExclusiveFileLock {
    /// Block until `file` is locked exclusively.
    pub(crate) fn acquire(file: File) -> io::Result<Self> {
        file.lock()?;
        Ok(Self { file })
    }
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        // Closing the descriptor would release the lock anyway; unlocking
        // first makes the release independent of any duplicated descriptor.
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_excludes_another_open_until_it_drops() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broker.lock");
        let held = ExclusiveFileLock::acquire(open_lock_file(&path).unwrap()).unwrap();

        // A second open file description contends, as another process would.
        let contender = open_lock_file(&path).unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        contender
            .try_lock()
            .expect("the lock is free once the guard drops");
    }

    #[test]
    fn opening_a_lock_file_keeps_its_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("slot.lock");
        std::fs::write(&path, b"holder").unwrap();
        let _lock = ExclusiveFileLock::acquire(open_lock_file(&path).unwrap()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"holder");
    }
}
