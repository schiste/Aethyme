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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::git::{GitRepo, GitWorktreeInfo};

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
    /// Git retains a registration marked prunable; its work could not be classified.
    PrunableRegistration,
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
            Self::PrunableRegistration => "prunable_registration",
        }
    }
}

/// Git registration details for a checkout matched to Git's inventory.
/// None means the checkout's registration state could not be confirmed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GitWorktreeState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    pub detached: bool,
    pub locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_reason: Option<String>,
    pub prunable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prunable_reason: Option<String>,
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
    /// Git's lock and prune state, when the registration was readable.
    pub git: Option<GitWorktreeState>,
    /// Whether Git listed this checkout. Absent when this is not a checkout or
    /// the inventory could not be read; see git_error for the latter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_registered: Option<bool>,
    /// Why Git inventory could not be read. A missing registration is reported
    /// separately by git_registered = false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_error: Option<String>,
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

fn path_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn same_path(left: &Path, right: &Path) -> bool {
    path_key(left) == path_key(right)
}

fn git_state(entry: &GitWorktreeInfo) -> GitWorktreeState {
    GitWorktreeState {
        head: entry.head.clone(),
        detached: entry.detached,
        locked: entry.locked,
        lock_reason: entry.lock_reason.clone(),
        prunable: entry.prunable,
        prunable_reason: entry.prunable_reason.clone(),
    }
}

fn short_branch(branch: Option<&str>) -> Option<String> {
    branch.map(|branch| {
        branch
            .strip_prefix("refs/heads/")
            .unwrap_or(branch)
            .to_string()
    })
}

fn identify_registration(
    root: &Path,
    inventory: Result<&[GitWorktreeInfo], &str>,
) -> (Option<GitWorktreeState>, Option<bool>, Option<String>) {
    let entries = match inventory {
        Ok(entries) => entries,
        Err(error) => return (None, None, Some(error.to_string())),
    };
    match entries.iter().find(|entry| same_path(&entry.path, root)) {
        Some(entry) => (Some(git_state(entry)), Some(true), None),
        None => (None, Some(false), None),
    }
}

type InventoryCache = BTreeMap<PathBuf, Result<Vec<GitWorktreeInfo>, String>>;

fn sort_report(report: &mut WorktreeReport) {
    report.rows.sort_by(|a, b| {
        b.work
            .holds_unique_work()
            .cmp(&a.work.holds_unique_work())
            .then_with(|| b.bytes.cmp(&a.bytes))
            .then_with(|| a.path.cmp(&b.path))
    });
}

fn append_prunable_rows(
    report: &mut WorktreeReport,
    repository: &str,
    entries: &[GitWorktreeInfo],
    live: &BTreeSet<PathBuf>,
) {
    for entry in entries.iter().filter(|entry| entry.prunable) {
        if let Some(row) = report
            .rows
            .iter_mut()
            .find(|row| same_path(&row.path, &entry.path))
        {
            row.git = Some(git_state(entry));
            row.git_registered = Some(true);
            row.git_error = None;
            if row.branch.is_none() {
                row.branch = short_branch(entry.branch.as_deref());
            }
            if row.work == WorkState::NotACheckout {
                row.work = WorkState::PrunableRegistration;
            }
            continue;
        }

        let bytes = if entry.path.is_dir() {
            tree_bytes(&entry.path)
        } else {
            0
        };
        report.total_bytes += bytes;
        report.rows.push(WorktreeRow {
            repository: repository.to_string(),
            path: entry.path.clone(),
            branch: short_branch(entry.branch.as_deref()),
            bytes,
            idle_days: None,
            work: WorkState::PrunableRegistration,
            live: live.iter().any(|path| same_path(path, &entry.path)),
            git: Some(git_state(entry)),
            git_registered: Some(true),
            git_error: None,
        });
    }
}

pub(crate) fn append_prunable_registrations(
    report: &mut WorktreeReport,
    repository: &str,
    entries: &[GitWorktreeInfo],
    live: &BTreeSet<PathBuf>,
) {
    append_prunable_rows(report, repository, entries, live);
    sort_report(report);
}

/// Read a checkout's state without changing it.
fn inspect(
    path: &Path,
    repository: &str,
    inventories: &mut InventoryCache,
    inventory_repositories: &mut BTreeMap<PathBuf, String>,
) -> (
    Option<String>,
    Option<i64>,
    WorkState,
    Option<GitWorktreeState>,
    Option<bool>,
    Option<String>,
) {
    let Ok(repo) = GitRepo::discover(path) else {
        return (None, None, WorkState::NotACheckout, None, None, None);
    };
    let branch = repo.current_branch().ok();
    let repository_root = repo
        .main_root()
        .unwrap_or_else(|_| repo.root().to_path_buf());
    inventory_repositories
        .entry(repository_root.clone())
        .or_insert_with(|| repository.to_string());
    let inventory = inventories
        .entry(repository_root)
        .or_insert_with(|| repo.worktree_inventory().map_err(|error| error.to_string()));
    let inventory = match inventory {
        Ok(entries) => Ok(entries.as_slice()),
        Err(error) => Err(error.as_str()),
    };
    let (git, git_registered, git_error) = identify_registration(repo.root(), inventory);

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
        return (
            branch,
            idle_days,
            WorkState::Uncommitted { files: dirty },
            git,
            git_registered,
            git_error,
        );
    }
    let state = match repo.commits_not_on_any_remote() {
        Ok(0) => WorkState::Recoverable,
        Ok(commits) => WorkState::Unpushed { commits },
        Err(_) => WorkState::NotACheckout,
    };
    (branch, idle_days, state, git, git_registered, git_error)
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
    let mut inventories = InventoryCache::new();
    let mut inventory_repositories = BTreeMap::new();
    for (repository, path) in worktrees {
        if !path.is_dir() {
            continue;
        }
        let bytes = tree_bytes(path);
        let (branch, idle_days, work, git, git_registered, git_error) = inspect(
            path,
            repository,
            &mut inventories,
            &mut inventory_repositories,
        );
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
            git,
            git_registered,
            git_error,
        });
    }
    for (root, inventory) in &inventories {
        if let (Some(repository), Ok(entries)) = (inventory_repositories.get(root), inventory) {
            append_prunable_rows(&mut report, repository, entries, live);
        }
    }
    // Work at risk first, then the largest, then stable by path. A reader
    // scanning from the top sees what they could lose before what they could
    // free -- the inverse of a disk report, deliberately.
    sort_report(&mut report);
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(path: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn checkout(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(&path).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            run_git(&path, &args);
        }
        std::fs::write(path.join("file.txt"), "one\n").unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-qm", "one"]] {
            run_git(&path, &args);
        }
        path
    }

    #[test]
    fn inventory_failure_is_distinct_from_an_unregistered_checkout() {
        let path = Path::new("/repo/checkout");
        let (state, registered, error) =
            identify_registration(path, Err("git worktree list timed out"));
        assert_eq!(state, None);
        assert_eq!(registered, None);
        assert_eq!(error.as_deref(), Some("git worktree list timed out"));

        let (state, registered, error) = identify_registration(path, Ok(&[] as &[GitWorktreeInfo]));
        assert_eq!(state, None);
        assert_eq!(registered, Some(false));
        assert_eq!(error, None);
    }

    #[test]
    fn report_matches_detached_and_locked_worktree_inventory() {
        let tmp = tempfile::tempdir().unwrap();
        let main = checkout(tmp.path(), "main");
        let linked = tmp.path().join("linked");
        let linked_arg = linked.to_str().unwrap();
        run_git(&main, &["worktree", "add", "--detach", linked_arg]);
        run_git(
            &main,
            &[
                "worktree",
                "lock",
                "--reason",
                "keep for review",
                linked_arg,
            ],
        );

        let report = build(
            &[
                ("repo".to_string(), main),
                ("repo".to_string(), linked.clone()),
            ],
            &BTreeSet::new(),
        );
        let row = report
            .rows
            .iter()
            .find(|row| same_path(&row.path, &linked))
            .unwrap();
        assert_eq!(row.git_registered, Some(true));
        let git = row.git.as_ref().unwrap();
        assert!(git.detached);
        assert!(git.locked);
        assert_eq!(git.lock_reason.as_deref(), Some("keep for review"));
    }

    #[test]
    fn report_includes_missing_prunable_registrations() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let main = checkout(&root, "main");
        let missing = root.join("removed");
        let missing_arg = missing.to_str().unwrap();
        run_git(&main, &["worktree", "add", "--detach", missing_arg]);
        std::fs::remove_dir_all(&missing).unwrap();

        let inventory = GitRepo::discover(&main)
            .unwrap()
            .worktree_inventory()
            .unwrap();
        assert!(
            inventory
                .iter()
                .any(|entry| same_path(&entry.path, &missing) && entry.prunable),
            "fixture should produce a prunable registration: {inventory:?}"
        );

        let report = build(&[("repo".to_string(), main)], &BTreeSet::new());
        let row = report
            .rows
            .iter()
            .find(|row| same_path(&row.path, &missing))
            .unwrap();
        assert_eq!(row.work, WorkState::PrunableRegistration);
        assert_eq!(row.bytes, 0);
        assert_eq!(row.git_registered, Some(true));
        assert!(row.git.as_ref().unwrap().prunable);
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
