//! Small, private atomic-file primitive shared by host-state writers.
//!
//! A named temporary file gives every writer a random, create-only path and
//! owns cleanup through Drop. The caller supplies the final publication
//! operation so it can choose rename, hard-link publication, or another
//! filesystem primitive without duplicating the fragile temporary-file
//! lifecycle.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Write and sync a private temporary file, then publish it through publish.
///
/// The temporary is created in the target's directory so publication remains
/// a same-filesystem operation. NamedTempFile removes it on every ordinary
/// return path, including write, sync, and publication errors; its random name
/// also means an interrupted process cannot wedge a later writer by reusing a
/// PID-and-clock filename.
pub(crate) fn with_synced_temporary<T, F>(
    target: &Path,
    bytes: &[u8],
    publish: F,
) -> io::Result<T>
where
    F: FnOnce(&Path) -> io::Result<T>,
{
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no parent: {}", target.display()),
        )
    })?;
    fs::create_dir_all(parent)?;

    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let prefix = format!(".{file_name}-");
    let mut temporary = tempfile::Builder::new()
        .prefix(&prefix)
        .tempfile_in(parent)?;
    crate::host_state::protect_host_state_path(temporary.path(), false)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    publish(temporary.path())
}

#[cfg(test)]
mod tests {
    use super::with_synced_temporary;

    #[test]
    fn a_failed_publication_leaves_no_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("marker.json");
        let error = with_synced_temporary(&target, b"payload", |_| {
            Err::<(), _>(std::io::Error::other("publish failed"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn an_existing_old_temporary_name_does_not_block_publication() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("journal.json");
        std::fs::write(directory.path().join(".journal.json-1234"), b"stale").unwrap();

        with_synced_temporary(&target, b"payload", |temporary| {
            std::fs::rename(temporary, &target)
        })
        .unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"payload");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }
}
