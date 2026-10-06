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
    /// Unique filesystem nodes below this checkout, including its root.
    pub inodes: u64,
    /// Where `bytes` and `inodes` came from. `unmeasured` means both are 0
    /// because the report's sizing budget ran out first (#559).
    pub size: SizeSource,
    /// When a `recorded` size was measured, in Unix milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_measured_at_ms: Option<i64>,
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
    /// A floor when `unmeasured_count` is non-zero.
    pub total_bytes: u64,
    pub total_inodes: u64,
    /// Bytes held by checkouts whose work exists nowhere else; a floor when
    /// `unmeasured_count` is non-zero.
    pub unique_work_bytes: u64,
    pub unique_work_inodes: u64,
    pub unique_work_count: usize,
    /// `bounded` (the default: recorded sizes, then walks within
    /// [`WORKTREE_REPORT_SIZE_BUDGET`]) or `measure` (every checkout walked).
    pub size_scan: &'static str,
    /// Rows whose size was not measured within the budget.
    pub unmeasured_count: usize,
}

/// Bytes under a directory, following no symlink out of it.
fn tree_usage(root: &Path) -> crate::disk_headroom::DirectoryUsage {
    crate::disk_headroom::directory_usage_best_effort(root)
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

        let usage = if entry.path.is_dir() {
            tree_usage(&entry.path)
        } else {
            crate::disk_headroom::DirectoryUsage::default()
        };
        report.total_bytes = report.total_bytes.saturating_add(usage.bytes);
        report.total_inodes = report.total_inodes.saturating_add(usage.inodes);
        report.rows.push(WorktreeRow {
            repository: repository.to_string(),
            path: entry.path.clone(),
            branch: short_branch(entry.branch.as_deref()),
            bytes: usage.bytes,
            inodes: usage.inodes,
            size: SizeSource::Measured,
            size_measured_at_ms: None,
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

/// How long a default `worktrees` pass may spend walking directories to size
/// checkouts that no earlier measurement recorded.
///
/// Sizing has no shortcut: every figure is a full recursive walk, and on a
/// host holding ~220 worktrees across a dozen repositories the unbounded walk
/// kept `worktrees --json` past a 150 s caller limit (#559). `--measure`
/// walks everything, as before.
pub const WORKTREE_REPORT_SIZE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Upper bound on the threads inspecting checkouts at once. The work is
/// process spawns and filesystem reads, not CPU.
const INSPECTION_WORKERS: usize = 8;

/// How a report sizes the checkouts it lists.
#[derive(Debug, Clone, Copy)]
pub enum WorktreeSizing {
    /// Walk every checkout to completion.
    Measure,
    /// Use a recorded measurement where one exists, and walk the rest for at
    /// most `budget`, counted from when sizing starts (after the Git
    /// inspection, which always completes). A checkout the walk did not
    /// finish is reported as unmeasured rather than as a partial or zero
    /// figure.
    Bounded { budget: std::time::Duration },
}

/// Where a row's `bytes` and `inodes` came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeSource {
    /// Walked by this report.
    Measured,
    /// Taken from the repository's size records; see `size_measured_at_ms`.
    Recorded,
    /// Not walked within the report's budget. `bytes` and `inodes` are 0 and
    /// mean nothing; the report's totals are floors.
    Unmeasured,
}

struct Sized {
    usage: crate::disk_headroom::DirectoryUsage,
    source: SizeSource,
    measured_at_ms: Option<i64>,
}

fn size_checkout(
    path: &Path,
    record: Option<crate::measurement::SizeRecord>,
    deadline: Option<std::time::Instant>,
) -> Sized {
    match deadline {
        None => Sized {
            usage: tree_usage(path),
            source: SizeSource::Measured,
            measured_at_ms: None,
        },
        Some(deadline) => {
            if let Some(record) = record
                && let Some(inodes) = record.inodes
            {
                return Sized {
                    usage: crate::disk_headroom::DirectoryUsage {
                        bytes: record.bytes,
                        inodes,
                    },
                    source: SizeSource::Recorded,
                    measured_at_ms: Some(record.measured_at_ms),
                };
            }
            match crate::disk_headroom::directory_usage_bounded(path, deadline) {
                Some(usage) => Sized {
                    usage,
                    source: SizeSource::Measured,
                    measured_at_ms: None,
                },
                None => Sized {
                    usage: crate::disk_headroom::DirectoryUsage::default(),
                    source: SizeSource::Unmeasured,
                    measured_at_ms: None,
                },
            }
        }
    }
}

/// What one checkout holds, read without changing it. Registration is matched
/// afterwards, once per repository, so this needs no shared state and can run
/// on a worker thread.
struct CheckoutState {
    /// The main checkout of the repository this worktree belongs to, and this
    /// checkout's own top level; `None` when the path is not a checkout.
    roots: Option<(PathBuf, PathBuf)>,
    branch: Option<String>,
    idle_days: Option<i64>,
    work: WorkState,
}

fn inspect_checkout(path: &Path) -> CheckoutState {
    let Ok(repo) = GitRepo::discover(path) else {
        return CheckoutState {
            roots: None,
            branch: None,
            idle_days: None,
            work: WorkState::NotACheckout,
        };
    };
    let branch = repo.current_branch().ok();
    let repository_root = repo
        .main_root()
        .unwrap_or_else(|_| repo.root().to_path_buf());

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

    let work = if dirty > 0 {
        WorkState::Uncommitted { files: dirty }
    } else {
        match repo.commits_not_on_any_remote() {
            Ok(0) => WorkState::Recoverable,
            Ok(commits) => WorkState::Unpushed { commits },
            Err(_) => WorkState::NotACheckout,
        }
    };
    CheckoutState {
        roots: Some((repository_root, repo.root().to_path_buf())),
        branch,
        idle_days,
        work,
    }
}

/// Run `inspect` over `items` on a few worker threads, keeping input order.
pub(crate) fn inspect_in_parallel<T, R, F>(items: &[T], inspect: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .clamp(1, INSPECTION_WORKERS)
        .min(items.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<(usize, R)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            break;
                        };
                        done.push((index, inspect(item)));
                    }
                    done
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("worktree inspection worker panicked"))
            .collect()
    });
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

/// Classify an enumerated set of worktrees, worst first, measuring every
/// checkout. See [`build_with`].
pub fn build(worktrees: &[(String, PathBuf)], live: &BTreeSet<PathBuf>) -> WorktreeReport {
    build_with(worktrees, live, WorktreeSizing::Measure)
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
///
/// Work classification always completes: it is what the report is for, and
/// it is per-checkout bounded Git work. Only sizing is subject to `sizing`.
pub fn build_with(
    worktrees: &[(String, PathBuf)],
    live: &BTreeSet<PathBuf>,
    sizing: WorktreeSizing,
) -> WorktreeReport {
    let present: Vec<&(String, PathBuf)> =
        worktrees.iter().filter(|(_, path)| path.is_dir()).collect();
    let states = inspect_in_parallel(&present, |(_, path)| inspect_checkout(path));

    // Size records live with each repository's main checkout; read each file
    // once. Read-only: a report never writes them.
    let mut records: BTreeMap<PathBuf, crate::measurement::SizeRecords> = BTreeMap::new();
    if matches!(sizing, WorktreeSizing::Bounded { .. }) {
        for state in &states {
            if let Some((repository_root, _)) = &state.roots {
                records
                    .entry(repository_root.clone())
                    .or_insert_with(|| crate::measurement::load_size_records(repository_root));
            }
        }
    }
    let to_size: Vec<(&PathBuf, Option<crate::measurement::SizeRecord>)> = present
        .iter()
        .zip(&states)
        .map(|((_, path), state)| {
            let record = state.roots.as_ref().and_then(|(repository_root, _)| {
                records
                    .get(repository_root)
                    .and_then(|records| records.get(&path.to_string_lossy()))
            });
            (path, record)
        })
        .collect();
    let deadline = match sizing {
        WorktreeSizing::Measure => None,
        WorktreeSizing::Bounded { budget } => Some(std::time::Instant::now() + budget),
    };
    let sizes = inspect_in_parallel(&to_size, |(path, record)| {
        size_checkout(path, *record, deadline)
    });

    let mut report = WorktreeReport {
        size_scan: match sizing {
            WorktreeSizing::Measure => "measure",
            WorktreeSizing::Bounded { .. } => "bounded",
        },
        ..WorktreeReport::default()
    };
    let mut inventories = InventoryCache::new();
    let mut inventory_repositories = BTreeMap::new();
    for (((repository, path), state), sized) in present.into_iter().zip(states).zip(sizes) {
        let (git, git_registered, git_error) = match &state.roots {
            Some((repository_root, checkout_root)) => {
                inventory_repositories
                    .entry(repository_root.clone())
                    .or_insert_with(|| repository.clone());
                let inventory = inventories
                    .entry(repository_root.clone())
                    .or_insert_with(|| {
                        GitRepo::discover(checkout_root)
                            .map_err(|error| error.to_string())
                            .and_then(|repo| {
                                repo.worktree_inventory().map_err(|error| error.to_string())
                            })
                    });
                let inventory = match inventory {
                    Ok(entries) => Ok(entries.as_slice()),
                    Err(error) => Err(error.as_str()),
                };
                identify_registration(checkout_root, inventory)
            }
            None => (None, None, None),
        };
        let usage = sized.usage;
        report.total_bytes = report.total_bytes.saturating_add(usage.bytes);
        report.total_inodes = report.total_inodes.saturating_add(usage.inodes);
        if sized.source == SizeSource::Unmeasured {
            report.unmeasured_count += 1;
        }
        if state.work.holds_unique_work() {
            report.unique_work_bytes = report.unique_work_bytes.saturating_add(usage.bytes);
            report.unique_work_inodes = report.unique_work_inodes.saturating_add(usage.inodes);
            report.unique_work_count += 1;
        }
        report.rows.push(WorktreeRow {
            repository: repository.clone(),
            live: live.contains(path),
            path: path.clone(),
            branch: state.branch,
            bytes: usage.bytes,
            inodes: usage.inodes,
            size: sized.source,
            size_measured_at_ms: sized.measured_at_ms,
            idle_days: state.idle_days,
            work: state.work,
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
        assert_eq!(row.inodes, 0);
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
        assert!(report.rows[0].inodes > 0);
        assert_eq!(report.total_inodes, report.rows[0].inodes);
        assert_eq!(report.unique_work_inodes, report.rows[0].inodes);
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

    /// #559: a budget that runs out leaves sizes unmeasured, never partial or
    /// guessed, while the work classification the report exists for still
    /// completes for every checkout.
    #[test]
    fn an_expired_size_budget_reports_unmeasured_rows_and_still_classifies_work() {
        let tmp = tempfile::tempdir().unwrap();
        let clean = checkout(tmp.path(), "clean");
        let dirty = checkout(tmp.path(), "dirty");
        std::fs::write(dirty.join("file.txt"), "edited\n").unwrap();
        let expired = WorktreeSizing::Bounded {
            budget: std::time::Duration::ZERO,
        };
        let report = build_with(
            &[
                ("repo".to_string(), clean.clone()),
                ("repo".to_string(), dirty.clone()),
            ],
            &BTreeSet::new(),
            expired,
        );

        assert_eq!(report.size_scan, "bounded");
        assert_eq!(report.unmeasured_count, 2);
        assert_eq!(report.total_bytes, 0, "an unmeasured row adds nothing");
        for row in &report.rows {
            assert_eq!(row.size, SizeSource::Unmeasured, "{row:?}");
            assert_eq!((row.bytes, row.inodes), (0, 0));
        }
        let state_of = |path: &Path| {
            report
                .rows
                .iter()
                .find(|row| row.path == path)
                .map(|row| row.work.clone())
                .unwrap()
        };
        assert_eq!(state_of(&dirty), WorkState::Uncommitted { files: 1 });
        assert_eq!(state_of(&clean), WorkState::Unpushed { commits: 1 });
        assert_eq!(report.unique_work_count, 2);

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["unmeasured_count"], 2);
        assert_eq!(json["rows"][0]["size"], "unmeasured");
    }

    /// A recorded measurement answers without a walk, so a host whose
    /// checkouts were sized by `gc plan` or status warming lists instantly,
    /// and the row says how old the figure is.
    #[test]
    fn a_recorded_size_is_used_without_walking_and_says_when_it_was_measured() {
        let tmp = tempfile::tempdir().unwrap();
        let path = checkout(tmp.path(), "recorded");
        std::fs::write(path.join(".git/info/exclude"), ".aethyme/\n").unwrap();
        let mut records = crate::measurement::SizeRecords::default();
        records.record_usage(&path.to_string_lossy(), 4_242, Some(7), 1_700_000_000_000);
        std::fs::create_dir_all(path.join(".aethyme")).unwrap();
        std::fs::write(
            path.join(".aethyme/worktree-sizes.json"),
            serde_json::to_vec(&records).unwrap(),
        )
        .unwrap();

        let report = build_with(
            &[("repo".to_string(), path)],
            &BTreeSet::new(),
            WorktreeSizing::Bounded {
                budget: std::time::Duration::ZERO,
            },
        );
        let row = &report.rows[0];
        assert_eq!(row.size, SizeSource::Recorded);
        assert_eq!((row.bytes, row.inodes), (4_242, 7));
        assert_eq!(row.size_measured_at_ms, Some(1_700_000_000_000));
        assert_eq!(report.unmeasured_count, 0);
        assert_eq!(row.work, WorkState::Unpushed { commits: 1 });
    }

    #[test]
    fn measuring_walks_every_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let path = checkout(tmp.path(), "measured");
        let report = build(&[("repo".to_string(), path)], &BTreeSet::new());
        assert_eq!(report.size_scan, "measure");
        assert_eq!(report.rows[0].size, SizeSource::Measured);
        assert!(report.rows[0].bytes > 0);
        assert_eq!(report.unmeasured_count, 0);
    }

    #[test]
    fn parallel_inspection_keeps_input_order() {
        let items: Vec<usize> = (0..200).collect();
        let doubled = inspect_in_parallel(&items, |item| item * 2);
        assert_eq!(
            doubled,
            items.iter().map(|item| item * 2).collect::<Vec<_>>()
        );
        assert!(inspect_in_parallel(&Vec::<usize>::new(), |item| *item).is_empty());
    }
}
