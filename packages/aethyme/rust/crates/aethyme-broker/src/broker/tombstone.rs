//! The tombstone a retired checkout leaves at its path.
//!
//! When the retirement lane removes a closed session's checkout (its work
//! in Git, or saved in a verified recovery archive), the path does not
//! simply vanish: a directory stays there holding only
//! [`TOMBSTONE_FILE`], a short plain-text note saying when the session was
//! retired, that its work is saved (and where), and the one command that
//! brings the checkout back at the same path (`broker start --reuse
//! <path>`, which restores it from its archive or recreates it from its
//! branch). An agent coming back hours later finds an explanation and a way
//! back instead of a missing directory.
//!
//! Every lane that walks checkout folders recognises a tombstone
//! ([`is_checkout_tombstone`]): the strip pass keeps it as `tombstone`, the
//! closed and cleaned laps skip it (and remove it once it has expired), and
//! reuse removes it under the lifecycle claim before restoring. `start`
//! never hands a recorded path out again, so a tombstone's path is never
//! allocated to a new session. Git's registration of the checkout was
//! removed when it was retired, so `git worktree add` at the same path works
//! once the tombstone is gone.

use super::*;

/// The one file a tombstone holds.
pub(crate) const TOMBSTONE_FILE: &str = "ARCHIVED-SESSION.txt";
/// The first line of [`TOMBSTONE_FILE`].
pub(crate) const TOMBSTONE_HEADER: &str = "aethyme: retired session checkout";
/// The longest a tombstone file may be.
const TOMBSTONE_MAX_BYTES: u64 = 16 * 1024;

/// Whether `path` is a tombstone (see [`read_checkout_tombstone`]). What
/// cannot be read is not one: a caller that loses something by that asks
/// [`read_checkout_tombstone`] instead.
pub(crate) fn is_checkout_tombstone(path: &Path) -> bool {
    matches!(read_checkout_tombstone(path), Ok(Some(_)))
}

/// Whether `start --reuse` should ask for `path` to be restored: nothing is
/// there, a tombstone is, or it cannot be told.
pub(crate) fn may_be_retired_checkout(path: &Path) -> bool {
    !path.exists()
        || !matches!(read_checkout_tombstone(path), Ok(None))
        || is_tombstone_leftover(path)
}

/// Whether `path` is what is left of a tombstone whose removal or writing was
/// cut short: a real directory holding nothing, or only an empty note.
pub(crate) fn is_tombstone_leftover(path: &Path) -> bool {
    let empty_note = |entry: std::fs::DirEntry| {
        entry.file_name() == TOMBSTONE_FILE
            && std::fs::symlink_metadata(entry.path())
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == 0)
    };
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
        && std::fs::read_dir(path).is_ok_and(|mut entries| match entries.next() {
            None => true,
            Some(only) => only.is_ok_and(empty_note) && entries.next().is_none(),
        })
}

/// Remove what [`is_tombstone_leftover`] found (never anything else).
/// Returns whether the path is free.
pub(crate) fn remove_tombstone_leftover(path: &Path) -> bool {
    if !is_tombstone_leftover(path) {
        return false;
    }
    let file = path.join(TOMBSTONE_FILE);
    (std::fs::symlink_metadata(&file).is_err() || std::fs::remove_file(&file).is_ok())
        && std::fs::remove_dir(path).is_ok()
}

/// The note of the tombstone at `path`: a real directory holding exactly one
/// entry, [`TOMBSTONE_FILE`], a small regular file starting with
/// [`TOMBSTONE_HEADER`]. Anything else is not one (a live checkout, or a
/// folder with content); an error when that cannot be read.
pub(crate) fn read_checkout_tombstone(path: &Path) -> std::io::Result<Option<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() {
        return Ok(None);
    }
    let mut entries = std::fs::read_dir(path)?;
    let Some(only) = entries.next().transpose()? else {
        return Ok(None);
    };
    if only.file_name() != TOMBSTONE_FILE || entries.next().transpose()?.is_some() {
        return Ok(None);
    }
    let file = only.path();
    let metadata = std::fs::symlink_metadata(&file)?;
    if !metadata.is_file() || metadata.len() > TOMBSTONE_MAX_BYTES {
        return Ok(None);
    }
    Ok(String::from_utf8(std::fs::read(&file)?)
        .ok()
        .filter(|note| note.starts_with(TOMBSTONE_HEADER)))
}

/// The recovery archive a tombstone's note names, if any (its `archive:`
/// line).
pub(crate) fn note_archive(note: &str) -> Option<PathBuf> {
    note.lines()
        .find_map(|line| line.strip_prefix("archive: "))
        .map(|archive| PathBuf::from(archive.trim()))
}

/// The command that brings a retired checkout back at `path`.
pub(crate) fn restore_command(path: &Path) -> String {
    format!(
        "aethyme broker start --reuse '{}'",
        path.display().to_string().replace('\'', "'\\''")
    )
}

/// Where a retired checkout's work is saved, for its tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SavedIn {
    /// Its uncommitted work in a verified recovery archive that `start
    /// --reuse` restores.
    Recovery(PathBuf),
    /// An abandoned operation's state, index and changed files in an
    /// operation archive, which restore does not read: manual steps.
    Operation(PathBuf),
}

/// Leave a tombstone at `worktree` for `session`, whose checkout was just
/// retired, saying truthfully where its work is (see [`SavedIn`]). Nothing
/// is written over an existing path, and a failed write removes only what
/// this call created (the file, then the directory with `remove_dir`, which
/// fails rather than delete anything that appeared there meanwhile).
pub(crate) fn write_checkout_tombstone(
    worktree: &Path,
    session: &Session,
    saved: &SavedIn,
) -> std::io::Result<()> {
    let command = restore_command(worktree);
    let (saved_text, way_back, archive_line) = match saved {
        SavedIn::Recovery(archive) => (
            format!(
                "All of its work is saved: its commits on branch {}, and its staged and unstaged \
                 changes, untracked files and protected ignored files in the verified recovery \
                 archive below.",
                session.branch
            ),
            format!("Bring it back here, exactly as it was, with one command:\n\n    {command}"),
            format!("archive: {}\n", archive.display()),
        ),
        SavedIn::Operation(archive) => (
            format!(
                "It was in the middle of a merge, rebase, cherry-pick, revert or bisect left \
                 abandoned. Its commits are on branch {} (pinned), and the operation's state, its \
                 index and its changed files are saved in the operation archive below. That \
                 archive is not restored automatically.",
                session.branch
            ),
            format!(
                "To get the checkout back:\n\n\
                 1. Recreate it from its branch (without the saved state):\n\n    {command}\n\n\
                 2. Read {}/README.md and {}/manifest.json, and copy back by hand what you \
                 need from that archive (the archive stays where it is).",
                archive.display(),
                archive.display()
            ),
            format!(
                "operation archive: {}\nrestore: manual\n",
                archive.display()
            ),
        ),
    };
    let body = format!(
        "{TOMBSTONE_HEADER}\n\n\
         Session {id} had no activity and its checkout was retired at {at} (epoch ms) to free \
         disk space.\n{saved_text}\n\n{way_back}\n\n\
         Regenerable dependencies are not restored; prepare the session again afterwards.\n\n\
         session: {id}\n{archive_line}",
        id = session.id,
        at = crate::clock::epoch_ms(),
    );
    write_tombstone_note(worktree, &body)
}

/// Write a tombstone holding `note` at `worktree`, where nothing is.
pub(crate) fn write_tombstone_note(worktree: &Path, note: &str) -> std::io::Result<()> {
    std::fs::create_dir(worktree)?;
    let file = worktree.join(TOMBSTONE_FILE);
    let written = std::fs::write(&file, note);
    if written.is_err() {
        if std::fs::symlink_metadata(&file).is_ok() {
            crate::warn_unrecorded(
                "remove a half-written tombstone file",
                std::fs::remove_file(&file),
            );
        }
        crate::warn_unrecorded(
            "remove a half-made tombstone",
            std::fs::remove_dir(worktree),
        );
    }
    written
}

/// Remove the tombstone at `path` (never anything that is not one).
/// Returns whether one was removed.
pub(crate) fn remove_checkout_tombstone(path: &Path) -> bool {
    matches!(
        read_checkout_tombstone(path),
        Ok(Some(note)) if remove_checkout_tombstone_holding(path, &note)
    )
}

/// Remove the tombstone at `path` if its note is still `note`; a directory
/// that cannot be removed gets its note back.
pub(crate) fn remove_checkout_tombstone_holding(path: &Path, note: &str) -> bool {
    if !matches!(read_checkout_tombstone(path), Ok(Some(current)) if current == note) {
        return false;
    }
    let file = path.join(TOMBSTONE_FILE);
    if std::fs::remove_file(&file).is_err() {
        return false;
    }
    let removed = std::fs::remove_dir(path).is_ok();
    if !removed {
        crate::warn_unrecorded("put a tombstone's note back", std::fs::write(&file, note));
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tombstone whose directory cannot be removed keeps its note.
    /// Mutation caught: not writing the note back.
    #[cfg(unix)]
    #[test]
    fn a_tombstone_whose_directory_stays_keeps_its_note() {
        use std::os::unix::fs::PermissionsExt;
        // Permissions do not stop root.
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkout");
        let note = format!("{TOMBSTONE_HEADER}\narchive: /x/y\n");
        write_tombstone_note(&path, &note).unwrap();
        let mode = |mode| {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        };
        mode(0o500);
        let removed = remove_checkout_tombstone(&path);
        mode(0o700);
        assert!(!removed);
        assert_eq!(read_checkout_tombstone(&path).unwrap(), Some(note));
    }

    /// What cannot be read is an error, not a folder without a tombstone.
    /// Mutation caught: reading an unreadable directory as none.
    #[cfg(unix)]
    #[test]
    fn a_tombstone_that_cannot_be_read_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkout");
        assert!(may_be_retired_checkout(&path));
        write_tombstone_note(&path, TOMBSTONE_HEADER).unwrap();
        let mode = |mode| {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        mode(0o000);
        let read = read_checkout_tombstone(&path);
        let asked = may_be_retired_checkout(&path);
        mode(0o700);
        assert!(read.is_err());
        assert!(asked);
        std::fs::remove_file(path.join(TOMBSTONE_FILE)).unwrap();
        assert!(may_be_retired_checkout(&path));
        std::fs::write(path.join("work.txt"), "content\n").unwrap();
        assert!(!may_be_retired_checkout(&path));
    }

    #[test]
    fn only_a_lone_tombstone_file_is_a_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkout");
        assert!(!is_checkout_tombstone(&path));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join(TOMBSTONE_FILE),
            format!("{TOMBSTONE_HEADER}\narchive: /x/y\n"),
        )
        .unwrap();
        assert!(is_checkout_tombstone(&path));
        let note = read_checkout_tombstone(&path).unwrap().unwrap();
        assert_eq!(note_archive(&note), Some(PathBuf::from("/x/y")));
        std::fs::write(path.join("work.txt"), "content\n").unwrap();
        assert!(
            !is_checkout_tombstone(&path),
            "a folder with content is not one"
        );
        assert!(!remove_checkout_tombstone(&path));
        assert!(path.join("work.txt").exists());
        std::fs::remove_file(path.join("work.txt")).unwrap();
        assert!(remove_checkout_tombstone(&path));
        assert!(!path.exists());
        assert_eq!(
            restore_command(Path::new("/pool/it's")),
            "aethyme broker start --reuse '/pool/it'\\''s'"
        );
    }
}
