//! What every worktree on this host is holding, and whether anything would be
//! lost by removing it.
//!
//! Reclamation answers "what may I delete". This answers the question no
//! reclamation can: which checkouts hold work that exists nowhere else, so the
//! per-branch decision -- push it or discard it -- reaches a person. Those
//! checkouts are the majority of the disk a busy fleet accumulates, and no
//! policy can free them, because only their author knows whether the work still
//! matters.
//!
//! Strictly read-only. It walks, it reads git state, it prints.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::git::GitRepo;

/// Whether removing a worktree would lose anything.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkState {
    /// Changes that are not in any commit.
    Uncommitted { files: usize },
    /// Commits that no remote branch contains.
    Unpushed { commits: usize },
    /// Clean, and every commit is reachable from a remote.
    Recoverable,
    /// Present on disk but not a git checkout; nothing can be said about it.
    NotACheckout,
}

impl WorkState {
    /// Whether removing this worktree would destroy the only copy of work.
    pub fn holds_unique_work(&self) -> bool {
        matches!(self, Self::Uncommitted { .. } | Self::Unpushed { .. })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Uncommitted { .. } => "uncommitted",
            Self::Unpushed { .. } => "unpushed",
            Self::Recoverable => "recoverable",
            Self::NotACheckout => "not_a_checkout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorktreeRow {
    /// The repository root this worktree belongs to, as the host names it.
    pub repository: String,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub bytes: u64,
    /// Days since the last commit. Absent when there is no commit to date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_days: Option<i64>,
    #[serde(flatten)]
    pub work: WorkState,
    /// A session is using this checkout right now.
    pub live: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct WorktreeReport {
    pub rows: Vec<WorktreeRow>,
    pub total_bytes: u64,
    /// Bytes held by checkouts whose work exists nowhere else.
    pub unique_work_bytes: u64,
    pub unique_work_count: usize,
}

/// Bytes under a directory, following no symlink out of it.
fn tree_bytes(root: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// Read a checkout's state without changing it.
fn inspect(path: &Path) -> (Option<String>, Option<i64>, WorkState) {
    let Ok(repo) = GitRepo::discover(path) else {
        return (None, None, WorkState::NotACheckout);
    };
    let branch = repo.current_branch().ok();

    // Untracked build output is not work. Counting it would report every
    // checkout that has ever been built as holding something unique, which is
    // exactly the noise that makes a report like this ignorable.
    let dirty = repo
        .dirty_paths()
        .map(|paths| {
            paths
                .iter()
                .filter(|path| {
                    ![
                        "target/",
                        "node_modules/",
                        ".venv/",
                        "dist/",
                        "build/",
                        ".DS_Store",
                    ]
                    .iter()
                    .any(|ignorable| path.contains(ignorable))
                })
                .count()
        })
        .unwrap_or(0);

    let idle_days = repo
        .head_committed_at()
        .ok()
        .filter(|at| *at > 0)
        .map(|at| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs() as i64)
                .unwrap_or(at);
            (now - at).max(0) / 86_400
        });

    if dirty > 0 {
        return (branch, idle_days, WorkState::Uncommitted { files: dirty });
    }
    let state = match repo.commits_not_on_any_remote() {
        Ok(0) => WorkState::Recoverable,
        Ok(commits) => WorkState::Unpushed { commits },
        Err(_) => WorkState::NotACheckout,
    };
    (branch, idle_days, state)
}

/// Classify an enumerated set of worktrees, worst first.
///
/// The caller supplies `(repository, path)` pairs because the host supports
/// more than one layout: worktrees may sit under a directory per repository
/// key, or directly under a configured root where siblings are sessions rather
/// than repositories. Deciding that here would guess at it.
///
/// `live` marks checkouts a session is using, so a reader can tell "busy" from
/// "abandoned" without consulting the broker separately.
pub fn build(worktrees: &[(String, PathBuf)], live: &BTreeSet<PathBuf>) -> WorktreeReport {
    let mut report = WorktreeReport::default();
    for (repository, path) in worktrees {
        if !path.is_dir() {
            continue;
        }
        let bytes = tree_bytes(path);
        let (branch, idle_days, work) = inspect(path);
        report.total_bytes += bytes;
        if work.holds_unique_work() {
            report.unique_work_bytes += bytes;
            report.unique_work_count += 1;
        }
        report.rows.push(WorktreeRow {
            repository: repository.clone(),
            live: live.contains(path),
            path: path.clone(),
            branch,
            bytes,
            idle_days,
            work,
        });
    }
    // Work at risk first, then the largest, then stable by path. A reader
    // scanning from the top sees what they could lose before what they could
    // free -- the inverse of a disk report, deliberately.
    report.rows.sort_by(|a, b| {
        b.work
            .holds_unique_work()
            .cmp(&a.work.holds_unique_work())
            .then_with(|| b.bytes.cmp(&a.bytes))
            .then_with(|| a.path.cmp(&b.path))
    });
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(&path).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(&path)
                .output()
                .unwrap();
        }
        std::fs::write(path.join("file.txt"), "one\n").unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-qm", "one"]] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(&path)
                .output()
                .unwrap();
        }
        path
    }

    /// A checkout with no remote holds its commits and nothing else does.
    #[test]
    fn a_commit_no_remote_holds_is_reported_as_unique_work() {
        let tmp = tempfile::tempdir().unwrap();
        let path = checkout(tmp.path(), "solo");
        let report = build(&[("repo".to_string(), path.clone())], &BTreeSet::new());
        assert_eq!(report.rows.len(), 1);
        assert!(
            matches!(report.rows[0].work, WorkState::Unpushed { commits } if commits >= 1),
            "{:?}",
            report.rows[0]
        );
        assert_eq!(report.unique_work_count, 1);
        assert!(report.unique_work_bytes > 0);
    }

    /// Untracked build output is not work. Counting it would mark every
    /// checkout that has ever been built as holding something unique, which is
    /// the noise that makes a report like this ignorable.
    #[test]
    fn untracked_build_output_is_not_uncommitted_work() {
        let tmp = tempfile::tempdir().unwrap();
        let path = checkout(tmp.path(), "built");
        std::fs::create_dir_all(path.join("target/debug")).unwrap();
        std::fs::write(path.join("target/debug/binary"), "x".repeat(2048)).unwrap();
        std::fs::create_dir_all(path.join("node_modules/pkg")).unwrap();
        std::fs::write(path.join("node_modules/pkg/index.js"), "x\n").unwrap();

        let report = build(&[("repo".to_string(), path)], &BTreeSet::new());
        assert!(
            !matches!(report.rows[0].work, WorkState::Uncommitted { .. }),
            "build output must not read as uncommitted work: {:?}",
            report.rows[0]
        );
    }

    /// A real edit is work, and outranks the unpushed commits underneath it.
    #[test]
    fn an_edited_file_is_reported_before_anything_recoverable() {
        let tmp = tempfile::tempdir().unwrap();
        let dirty = checkout(tmp.path(), "dirty");
        std::fs::write(dirty.join("file.txt"), "edited\n").unwrap();
        let quiet = checkout(tmp.path(), "quiet");

        let report = build(
            &[
                ("repo".to_string(), quiet),
                ("repo".to_string(), dirty.clone()),
            ],
            &BTreeSet::new(),
        );
        assert_eq!(report.rows.len(), 2);
        assert!(
            matches!(report.rows[0].work, WorkState::Uncommitted { files } if files >= 1),
            "work at risk sorts first: {:?}",
            report.rows
        );
        assert_eq!(report.rows[0].path, dirty);
    }

    #[test]
    fn a_live_checkout_is_marked_so_busy_reads_differently_from_abandoned() {
        let tmp = tempfile::tempdir().unwrap();
        let path = checkout(tmp.path(), "busy");
        let live: BTreeSet<PathBuf> = [path.clone()].into_iter().collect();
        let report = build(&[("repo".to_string(), path)], &live);
        assert!(report.rows[0].live);
    }

    #[test]
    fn a_directory_that_is_not_a_checkout_says_so_rather_than_being_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("plain");
        std::fs::create_dir_all(&path).unwrap();
        let report = build(&[("repo".to_string(), path)], &BTreeSet::new());
        assert_eq!(report.rows[0].work, WorkState::NotACheckout);
        assert_eq!(
            report.unique_work_count, 0,
            "nothing can be said about it, so it is not counted as work at risk"
        );
    }
}
