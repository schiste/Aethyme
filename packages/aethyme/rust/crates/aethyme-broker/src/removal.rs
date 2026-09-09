//! Removing a directory tree that something else is still writing into.
//!
//! `std::fs::remove_dir_all` and `git worktree remove` both walk depth-first
//! and `rmdir` on the way out. A file created in a directory after the walk
//! entered it and before the `rmdir` makes that `rmdir` fail with `ENOTEMPTY`,
//! and the whole removal aborts partway with most of the tree already gone.
//!
//! On macOS this is not a rare race. Finder writes `.DS_Store` into any
//! directory it indexes, and `.DS_Store` is gitignored in most repositories --
//! so it does not trip a clean-worktree check, removal is allowed to start, and
//! then it dies on the way back up. Observed three times on this repository,
//! each time leaving the same six `.DS_Store` files and a stranded tree.
//!
//! Two consequences shape this module:
//!
//! - **Retry, rather than fail.** The interfering writer creates one file per
//!   directory it visits, not an endless stream. A second pass over what is
//!   left almost always finishes, so a bounded retry converts the common
//!   failure into a success instead of reporting it.
//! - **Report what was actually freed.** All-or-nothing accounting says zero
//!   bytes were reclaimed after deleting 8 GB, because the final `rmdir`
//!   failed. Measuring before and after describes what happened.

use std::path::Path;

/// Bounded because the interfering writer creates a file per visited directory,
/// so progress is finite -- but an unbounded loop against a process writing
/// continuously would spin forever.
const REMOVAL_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemovalOutcome {
    /// Bytes measured before removal minus bytes still present after it.
    /// Honest under partial failure, which is the case that matters.
    pub freed_bytes: u64,
    /// Whether the directory itself is gone.
    pub removed: bool,
    /// Why the last attempt failed, when it did.
    pub error: Option<String>,
    pub attempts: usize,
}

impl RemovalOutcome {
    /// A partial removal is a failure that still freed disk. Callers report the
    /// bytes either way; only `removed` decides whether the path is gone.
    pub fn is_partial(&self) -> bool {
        !self.removed && self.freed_bytes > 0
    }
}

/// Size of a tree, not following symlinks. Errors count as zero rather than
/// aborting: this only feeds reporting, and a partially-removed tree is exactly
/// the case where a stat is likely to lose a race with our own deletion.
pub(crate) fn tree_size(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.file_type().is_symlink() {
        return 0;
    }
    if metadata.is_file() {
        return metadata.len();
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| tree_size(&entry.path()))
        .fold(0_u64, |total, bytes| total.saturating_add(bytes))
}

/// Remove a directory tree, retrying past interference, and report the bytes
/// actually freed rather than only whole successes.
///
/// Already-absent counts as removed: the caller wanted the path gone and it is.
pub(crate) fn remove_tree(path: &Path) -> RemovalOutcome {
    remove_tree_with(path, |target| std::fs::remove_dir_all(target))
}

/// `remove_tree` with the deletion itself injectable.
///
/// The behaviour worth testing is "fails once, succeeds on the retry", and the
/// real trigger for that is another process writing into the tree mid-walk.
/// Reproducing it with a live writer would be a racy test of a race, so the
/// attempt is a seam instead.
fn remove_tree_with(
    path: &Path,
    mut attempt: impl FnMut(&Path) -> std::io::Result<()>,
) -> RemovalOutcome {
    if !path.exists() {
        return RemovalOutcome {
            freed_bytes: 0,
            removed: true,
            error: None,
            attempts: 0,
        };
    }
    let before = tree_size(path);
    let mut last_error = None;
    for number in 1..=REMOVAL_ATTEMPTS {
        match attempt(path) {
            Ok(()) => {
                return RemovalOutcome {
                    freed_bytes: before,
                    removed: true,
                    error: None,
                    attempts: number,
                };
            }
            // Already gone is the desired state, not a failure.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return RemovalOutcome {
                    freed_bytes: before,
                    removed: true,
                    error: None,
                    attempts: number,
                };
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    let after = tree_size(path);
    RemovalOutcome {
        freed_bytes: before.saturating_sub(after),
        removed: !path.exists(),
        error: last_error,
        attempts: REMOVAL_ATTEMPTS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    fn enotempty() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::DirectoryNotEmpty, "Directory not empty")
    }

    #[test]
    fn an_absent_path_is_already_in_the_desired_state() {
        let temp = tempfile::tempdir().unwrap();
        let outcome = remove_tree(&temp.path().join("never-existed"));
        assert!(outcome.removed);
        assert_eq!(outcome.freed_bytes, 0);
    }

    #[test]
    fn a_plain_tree_is_removed_in_one_attempt_and_its_bytes_reported() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tree");
        write(&root.join("a.txt"), 100);
        write(&root.join("nested/b.txt"), 50);
        let outcome = remove_tree(&root);
        assert!(outcome.removed);
        assert_eq!(outcome.freed_bytes, 150);
        assert_eq!(outcome.attempts, 1);
        assert!(!root.exists());
    }

    /// The `.DS_Store` case. Finder writes one file per directory it indexed,
    /// so the interference is finite and a second pass finishes the job --
    /// which is the difference between a stranded worktree and a clean one.
    #[test]
    fn an_interrupted_walk_succeeds_on_the_retry_instead_of_stranding_the_tree() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tree");
        write(&root.join("deep/a.txt"), 40);
        let mut calls = 0;
        let outcome = remove_tree_with(&root, |target| {
            calls += 1;
            if calls == 1 {
                return Err(enotempty());
            }
            std::fs::remove_dir_all(target)
        });
        assert!(outcome.removed, "{outcome:?}");
        assert_eq!(outcome.attempts, 2);
        assert_eq!(outcome.freed_bytes, 40);
        assert!(outcome.error.is_none());
        assert!(!root.exists());
    }

    /// Retrying is bounded: a writer that never stops must not spin forever.
    #[test]
    fn persistent_interference_gives_up_and_says_why() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tree");
        write(&root.join("a.txt"), 10);
        let outcome = remove_tree_with(&root, |_| Err(enotempty()));
        assert!(!outcome.removed);
        assert_eq!(outcome.attempts, REMOVAL_ATTEMPTS);
        assert!(outcome.error.unwrap().contains("Directory not empty"));
        assert!(
            root.exists(),
            "nothing was deleted, so nothing should be gone"
        );
    }

    /// The reported failure: 8 GB deleted, `0 bytes reclaimed`, because the
    /// final rmdir failed and accounting was all-or-nothing.
    #[test]
    fn a_partial_removal_reports_the_bytes_it_actually_freed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tree");
        write(&root.join("pinned.txt"), 7);
        write(&root.join("bulk/large.txt"), 1000);
        let outcome = remove_tree_with(&root, |target| {
            // The observed shape: the bulk goes, the final rmdir fails.
            let _ = std::fs::remove_dir_all(target.join("bulk"));
            Err(enotempty())
        });
        assert!(!outcome.removed);
        assert!(outcome.is_partial(), "a partial removal is still progress");
        assert_eq!(
            outcome.freed_bytes, 1000,
            "reporting 0 here is what made reclaim under-report by 13 GiB"
        );
    }

    #[test]
    fn symlinks_are_not_followed_when_sizing() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        write(&outside.join("big.txt"), 10_000);
        let root = temp.path().join("tree");
        write(&root.join("small.txt"), 5);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert_eq!(tree_size(&root), 5);
    }
}
