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

/// Whether `path` is a tombstone: a real directory holding exactly one
/// entry, [`TOMBSTONE_FILE`], a small regular file starting with
/// [`TOMBSTONE_HEADER`]. Anything else is not one (a live checkout, or a
/// folder with content).
pub(crate) fn is_checkout_tombstone(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    let Ok(mut entries) = std::fs::read_dir(path) else {
        return false;
    };
    let Some(Ok(only)) = entries.next() else {
        return false;
    };
    if entries.next().is_some() || only.file_name() != TOMBSTONE_FILE {
        return false;
    }
    let file = only.path();
    std::fs::symlink_metadata(&file)
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= TOMBSTONE_MAX_BYTES)
        && std::fs::read_to_string(&file).is_ok_and(|text| text.starts_with(TOMBSTONE_HEADER))
}

/// The archive a tombstone names, if any (its `archive:` line).
pub(crate) fn tombstone_archive(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path.join(TOMBSTONE_FILE)).ok()?;
    text.lines()
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
    /// All committed, on its branch.
    Branch,
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
    std::fs::create_dir(worktree)?;
    let command = restore_command(worktree);
    let (saved_text, way_back, archive_line) = match saved {
        SavedIn::Branch => (
            format!(
                "All of its work is saved: it was all committed, on branch {}.",
                session.branch
            ),
            format!("Bring it back here, exactly as it was, with one command:\n\n    {command}"),
            String::new(),
        ),
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
    let file = worktree.join(TOMBSTONE_FILE);
    let written = std::fs::write(&file, body);
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
    if !is_checkout_tombstone(path) {
        return false;
    }
    std::fs::remove_file(path.join(TOMBSTONE_FILE)).is_ok() && std::fs::remove_dir(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(tombstone_archive(&path), Some(PathBuf::from("/x/y")));
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
