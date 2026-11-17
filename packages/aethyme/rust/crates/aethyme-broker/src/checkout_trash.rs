//! Retired checkouts: renamed into a `.trash` directory beside them, then
//! deleted later by one host-wide, self-paced deleter.
//!
//! Removing a checkout used to mean `git worktree remove`, which unlinks the
//! whole tree (often hundreds of thousands of build outputs) inside the
//! caller's deadline and under the repository lifecycle claim. Here the
//! caller does two O(1) steps instead: `rename(2)` the directory into the
//! pool's `.trash` (same parent, so the same volume) and drop its Git
//! registration (the worktree admin directory). The branch ref is never
//! touched. The tree is then unlinked by [`drain_trash`], which runs from the
//! background worker, one at a time per host, at a fixed rate that slows
//! under load but never below a floor, never waits for memory pressure, and
//! runs unpaced when the volume is below its headroom.
//!
//! Only a caller that has already established that every byte of the
//! checkout is in Git, in a verified recovery archive, or catalogued
//! regenerable output may move it here: the trash is deleted without
//! further checks.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The directory, beside the checkouts of one pool, that holds retired
/// checkouts until the deleter removes them. Pool scanners skip names that
/// start with a dot (`is_worktree_root_infrastructure`).
pub(crate) const TRASH_DIR: &str = ".trash";
/// Suffix of the sidecar file counting failed deletions of one entry.
const FAILURES_SUFFIX: &str = ".failures";
/// After this many failed deletions an entry is reported by name and tried
/// after every other entry, so it never wedges the queue.
pub(crate) const TRASH_FAILURE_REPORT_AFTER: u32 = 3;
/// The deleter's steady rate.
pub(crate) const DELETER_STEADY_UNLINKS_PER_SECOND: u32 = 2_000;
/// The rate the deleter never goes below, however loaded the host.
pub(crate) const DELETER_FLOOR_UNLINKS_PER_SECOND: u32 = 200;
/// File name of the host-wide single-flight lock.
pub(crate) const DELETER_LOCK_FILE: &str = "checkout-trash-deleter.lock";

static TRASH_NONCE: AtomicU64 = AtomicU64::new(0);

/// The trash directory of the pool holding `worktree`.
pub(crate) fn trash_dir_beside(worktree: &Path) -> Option<PathBuf> {
    worktree.parent().map(|pool| pool.join(TRASH_DIR))
}

/// A checkout moved into the trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrashedCheckout {
    pub(crate) path: PathBuf,
    /// The Git registration (worktree admin directory) is gone. When it could
    /// not be removed, `git worktree prune` drops it later: it names a path
    /// that no longer exists.
    pub(crate) registration_removed: bool,
}

/// The worktree admin directory a linked checkout's `.git` file names.
fn worktree_admin_dir(worktree: &Path) -> Result<PathBuf, String> {
    let dot_git = worktree.join(".git");
    let metadata = std::fs::symlink_metadata(&dot_git)
        .map_err(|error| format!("cannot read {}: {error}", dot_git.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "{} is not a linked checkout's .git file",
            dot_git.display()
        ));
    }
    let text = std::fs::read_to_string(&dot_git)
        .map_err(|error| format!("cannot read {}: {error}", dot_git.display()))?;
    let target = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .ok_or_else(|| format!("{} names no gitdir", dot_git.display()))?;
    let target = Path::new(target);
    Ok(if target.is_absolute() {
        target.to_path_buf()
    } else {
        worktree.join(target)
    })
}

/// Move `worktree`, a linked checkout of the repository whose Git common
/// directory is `common_dir`, into the trash beside it and drop its Git
/// registration. `session_id` names the entry. Environments bound to the
/// checkout are destroyed first (their hooks live in it), and a lock the
/// broker placed on the worktree is released; a lock anyone else placed
/// refuses the move. Nothing is unlinked here except the small admin
/// directory, after the rename.
///
/// The caller must have proven that every byte of the checkout is in Git, in
/// a verified archive, or catalogued regenerable output.
pub(crate) fn move_worktree_to_trash(
    common_dir: &Path,
    worktree: &Path,
    session_id: i64,
) -> Result<TrashedCheckout, String> {
    let _span = crate::phase_telemetry::span("checkout_trash_move");
    let metadata = std::fs::symlink_metadata(worktree)
        .map_err(|error| format!("cannot inspect {}: {error}", worktree.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!("{} is not a real directory", worktree.display()));
    }
    let admin = worktree_admin_dir(worktree)?;
    let canonical_admin = admin
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", admin.display()))?;
    let registrations = common_dir
        .join("worktrees")
        .canonicalize()
        .map_err(|error| format!("cannot resolve the worktree registrations: {error}"))?;
    if canonical_admin.parent() != Some(registrations.as_path()) {
        return Err(format!(
            "{} is not registered with this repository",
            worktree.display()
        ));
    }
    let lock = canonical_admin.join("locked");
    match std::fs::read_to_string(&lock) {
        Ok(reason) if reason.trim() == crate::git::BROKER_WORKTREE_LOCK_REASON => {
            std::fs::remove_file(&lock)
                .map_err(|error| format!("cannot release the broker's worktree lock: {error}"))?;
        }
        Ok(reason) => {
            return Err(format!(
                "{} is locked ({}); its lock is somebody else's protection",
                worktree.display(),
                reason.trim()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot read the worktree lock: {error}")),
    }
    crate::environments::lifecycle::destroy_for_worktree(worktree)?;
    let trash = trash_dir_beside(worktree)
        .ok_or_else(|| format!("{} has no parent directory", worktree.display()))?;
    std::fs::create_dir_all(&trash)
        .map_err(|error| format!("cannot create {}: {error}", trash.display()))?;
    let entry = trash.join(format!(
        "{:013}-{session_id}-{}-{}",
        crate::clock::epoch_ms().max(0),
        std::process::id(),
        TRASH_NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::rename(worktree, &entry).map_err(|error| {
        format!(
            "cannot move {} into {}: {error}",
            worktree.display(),
            trash.display()
        )
    })?;
    crate::phase_telemetry::add("checkout_trashed", Duration::ZERO);
    let registration_removed = match std::fs::remove_dir_all(&canonical_admin) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => {
            crate::phase_telemetry::add("checkout_trash_registration_left", Duration::ZERO);
            false
        }
    };
    Ok(TrashedCheckout {
        path: entry,
        registration_removed,
    })
}

/// How fast the deleter may unlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleterPace {
    /// The steady rate.
    Steady,
    /// The host is loaded: the floor rate.
    Floor,
    /// The volume is below its headroom: as fast as the disk allows.
    Unpaced,
}

impl DeleterPace {
    /// The pace for a volume that is (or is not) below its headroom on a
    /// host that is (or is not) loaded. Memory pressure is deliberately not
    /// an input: unlinking frees page cache rather than using memory.
    pub(crate) const fn for_host(below_headroom: bool, loaded: bool) -> Self {
        if below_headroom {
            Self::Unpaced
        } else if loaded {
            Self::Floor
        } else {
            Self::Steady
        }
    }

    pub(crate) const fn unlinks_per_second(self) -> Option<u32> {
        match self {
            Self::Steady => Some(DELETER_STEADY_UNLINKS_PER_SECOND),
            Self::Floor => Some(DELETER_FLOOR_UNLINKS_PER_SECOND),
            Self::Unpaced => None,
        }
    }
}

/// Whether the host's one-minute load average exceeds its parallelism.
pub(crate) fn host_loaded() -> bool {
    let mut load = [0f64; 3];
    // SAFETY: `load` has room for the three samples requested.
    let samples = unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
    let parallelism = std::thread::available_parallelism().map_or(1, usize::from) as f64;
    samples >= 1 && load[0] > parallelism
}

/// Holds unlinks to a rate by sleeping only when ahead of it.
struct UnlinkLimiter {
    per_second: Option<u32>,
    started: Instant,
    unlinks: u64,
}

impl UnlinkLimiter {
    fn new(pace: DeleterPace) -> Self {
        Self {
            per_second: pace.unlinks_per_second(),
            started: Instant::now(),
            unlinks: 0,
        }
    }

    fn before_unlink(&mut self) {
        self.unlinks = self.unlinks.saturating_add(1);
        let Some(per_second) = self.per_second.filter(|rate| *rate > 0) else {
            return;
        };
        let due = Duration::from_secs_f64(self.unlinks as f64 / f64::from(per_second));
        let elapsed = self.started.elapsed();
        // Sleep in batches of at least 5 ms so the pacing costs no syscall
        // per unlink.
        if let Some(ahead) = due.checked_sub(elapsed)
            && ahead >= Duration::from_millis(5)
        {
            std::thread::sleep(ahead);
        }
    }
}

/// The host-wide single-flight deleter lock. It is a kernel file lock, so a
/// holder that died releases it with its descriptors: a dead owner never
/// keeps the next deleter out. The file names the current holder.
pub(crate) struct DeleterLock {
    _lock: crate::file_lock::ExclusiveFileLock,
}

/// Take the deleter lock at `path` if nobody holds it.
pub(crate) fn try_deleter_lock(path: &Path) -> std::io::Result<Option<DeleterLock>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let Some(lock) = crate::file_lock::ExclusiveFileLock::try_acquire(file)? else {
        return Ok(None);
    };
    crate::warn_unrecorded(
        "name the checkout trash deleter",
        lock.record_holder(&format!(
            "pid {} since {}\n",
            std::process::id(),
            crate::clock::epoch_ms()
        )),
    );
    Ok(Some(DeleterLock { _lock: lock }))
}

/// One retired checkout waiting for the deleter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrashEntry {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) failures: u32,
}

fn failures_path(entry: &Path) -> PathBuf {
    let mut name = entry.as_os_str().to_os_string();
    name.push(FAILURES_SUFFIX);
    PathBuf::from(name)
}

fn read_failures(entry: &Path) -> u32 {
    std::fs::read_to_string(failures_path(entry))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// The entries of one trash directory in deletion order: oldest first (the
/// name starts with the time it was retired), entries that failed
/// [`TRASH_FAILURE_REPORT_AFTER`] times after all others.
pub(crate) fn trash_entries(trash: &Path) -> Vec<TrashEntry> {
    let Ok(listing) = std::fs::read_dir(trash) else {
        return Vec::new();
    };
    let mut entries = listing
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            if name.starts_with('.') || name.ends_with(FAILURES_SUFFIX) {
                return None;
            }
            let path = entry.path();
            let failures = read_failures(&path);
            Some(TrashEntry {
                path,
                name,
                failures,
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        (left.failures >= TRASH_FAILURE_REPORT_AFTER, &left.name)
            .cmp(&(right.failures >= TRASH_FAILURE_REPORT_AFTER, &right.name))
    });
    entries
}

/// Clear the user-immutable and append-only flags that make an unlink fail
/// with `EPERM` on macOS (Finder's "Locked"). Best effort, never follows a
/// link; nothing to do elsewhere.
#[cfg(target_os = "macos")]
fn clear_deletion_flags(root: &Path) {
    fn visit(path: &Path, depth: usize) {
        use std::os::unix::ffi::OsStrExt;
        if depth > 256 {
            return;
        }
        if let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
            // SAFETY: `c_path` is NUL-terminated and outlives the call.
            unsafe {
                libc::lchflags(c_path.as_ptr(), 0);
            }
        }
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return;
        };
        if metadata.is_dir()
            && let Ok(children) = std::fs::read_dir(path)
        {
            for child in children.filter_map(Result::ok) {
                visit(&child.path(), depth + 1);
            }
        }
    }
    visit(root, 0);
}

#[cfg(not(target_os = "macos"))]
fn clear_deletion_flags(_root: &Path) {}

/// What one deleter run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TrashDrainReport {
    pub(crate) entries_removed: usize,
    /// The run's deadline stopped an entry part-way; the next run resumes it.
    pub(crate) entries_interrupted: usize,
    pub(crate) unlinks: usize,
    pub(crate) bytes: u64,
    /// Entries that failed this run.
    pub(crate) failed: Vec<String>,
    /// Entries that have failed at least [`TRASH_FAILURE_REPORT_AFTER`] times.
    pub(crate) reported: Vec<String>,
    pub(crate) pace: Option<DeleterPace>,
    /// Entries still in the trash after the run.
    pub(crate) entries_pending: usize,
}

/// Delete the entries of `trash_dirs` oldest first until `deadline`, at
/// `pace`. The caller holds the deleter lock. A deadline stops an entry
/// resumably; a failed entry gets its deletion flags cleared and its failure
/// counted, and the run goes on with the next entry.
pub(crate) fn drain_trash(
    trash_dirs: &[PathBuf],
    deadline: Instant,
    pace: DeleterPace,
) -> TrashDrainReport {
    let _span = crate::phase_telemetry::span("checkout_trash_drain");
    let mut report = TrashDrainReport {
        pace: Some(pace),
        ..TrashDrainReport::default()
    };
    match pace {
        DeleterPace::Unpaced => {
            crate::phase_telemetry::add("checkout_trash_unpaced_runs", Duration::ZERO);
        }
        DeleterPace::Floor => {
            crate::phase_telemetry::add("checkout_trash_floor_runs", Duration::ZERO);
        }
        DeleterPace::Steady => {}
    }
    let started = Instant::now();
    // The worker's unattended pacer would stop this deleter on memory
    // pressure; it paces itself instead.
    crate::gc::unattended_pacing::without_pacer(|| {
        'dirs: for trash in trash_dirs {
            for entry in trash_entries(trash) {
                if Instant::now() >= deadline {
                    break 'dirs;
                }
                let mut limiter = UnlinkLimiter::new(pace);
                match crate::gc::remove_condemned_tree_with_progress_and_observed_file_bytes_and_guard(
                    &entry.path,
                    None,
                    Some(deadline),
                    || {
                        limiter.before_unlink();
                        Ok(())
                    },
                ) {
                    Ok((crate::gc::TreeRemoval::Complete, unlinks, bytes)) => {
                        report.entries_removed += 1;
                        report.unlinks = report.unlinks.saturating_add(unlinks);
                        report.bytes = report.bytes.saturating_add(bytes);
                        let sidecar = failures_path(&entry.path);
                        if sidecar.exists() {
                            crate::warn_unrecorded(
                                "clear a removed trash entry's failure count",
                                std::fs::remove_file(sidecar),
                            );
                        }
                        crate::phase_telemetry::add("checkout_trash_entries_removed", Duration::ZERO);
                    }
                    Ok((crate::gc::TreeRemoval::Interrupted, unlinks, bytes)) => {
                        report.entries_interrupted += 1;
                        report.unlinks = report.unlinks.saturating_add(unlinks);
                        report.bytes = report.bytes.saturating_add(bytes);
                        break 'dirs;
                    }
                    Err(error) => {
                        report.unlinks = report.unlinks.saturating_add(error.entries_reclaimed);
                        clear_deletion_flags(&entry.path);
                        let failures = entry.failures.saturating_add(1);
                        crate::warn_unrecorded(
                            "count a failed checkout trash deletion",
                            std::fs::write(failures_path(&entry.path), failures.to_string()),
                        );
                        crate::phase_telemetry::add("checkout_trash_entry_failed", Duration::ZERO);
                        report.failed.push(entry.name.clone());
                        if failures >= TRASH_FAILURE_REPORT_AFTER {
                            report.reported.push(entry.name.clone());
                        }
                    }
                }
            }
        }
    });
    crate::phase_telemetry::add("checkout_trash_drain_time", started.elapsed());
    report.entries_pending = trash_dirs
        .iter()
        .map(|trash| trash_entries(trash).len())
        .sum();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repository with one commit and a linked checkout in a pool beside it.
    fn repository_with_checkout() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        std::fs::write(main.join("base.txt"), "base\n").unwrap();
        git(&main, &["add", "base.txt"]);
        git(&main, &["commit", "-qm", "base"]);
        let pool = dir.path().join("pool");
        std::fs::create_dir_all(&pool).unwrap();
        let checkout = pool.join("checkout");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "session-branch",
                checkout.to_str().unwrap(),
            ],
        );
        (dir, main, checkout)
    }

    /// Retiring renames the checkout into the pool's trash before anything is
    /// unlinked: the moved tree still holds every file, Git no longer lists
    /// the worktree, and the branch is untouched.
    #[test]
    fn a_checkout_is_renamed_into_the_trash_before_any_unlink() {
        let (_dir, main, checkout) = repository_with_checkout();
        std::fs::create_dir_all(checkout.join("target/debug")).unwrap();
        std::fs::write(checkout.join("target/debug/artifact"), "built\n").unwrap();
        let common = main.join(".git");

        let trashed = move_worktree_to_trash(&common, &checkout, 7).unwrap();

        assert!(!checkout.exists(), "the checkout path is free at once");
        assert!(
            trashed
                .path
                .starts_with(checkout.parent().unwrap().join(TRASH_DIR))
        );
        assert_eq!(
            std::fs::read_to_string(trashed.path.join("target/debug/artifact")).unwrap(),
            "built\n",
            "nothing was unlinked by the move"
        );
        assert!(trashed.registration_removed);
        let listing = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&main)
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&listing.stdout).into_owned();
        assert!(!listing.contains("checkout"), "{listing}");
        let branch = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "refs/heads/session-branch"])
            .current_dir(&main)
            .output()
            .unwrap();
        assert!(branch.status.success(), "the branch ref is untouched");

        let entries = trash_entries(&checkout.parent().unwrap().join(TRASH_DIR));
        assert_eq!(entries.len(), 1);
        assert!(entries[0].name.contains("-7-"), "{entries:?}");
    }

    /// A worktree someone else locked is refused and left in place.
    #[test]
    fn a_foreign_lock_refuses_the_move() {
        let (_dir, main, checkout) = repository_with_checkout();
        git(
            &main,
            &[
                "worktree",
                "lock",
                "--reason",
                "mine",
                checkout.to_str().unwrap(),
            ],
        );
        let error = move_worktree_to_trash(&main.join(".git"), &checkout, 1).unwrap_err();
        assert!(error.contains("locked"), "{error}");
        assert!(checkout.join("base.txt").exists());
    }

    /// A directory that is not a checkout of this repository is refused.
    #[test]
    fn a_foreign_directory_is_refused() {
        let (dir, main, _checkout) = repository_with_checkout();
        let stray = dir.path().join("pool/stray");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join(".git"), "gitdir: /nowhere/at/all\n").unwrap();
        assert!(move_worktree_to_trash(&main.join(".git"), &stray, 1).is_err());
        assert!(stray.exists());
    }

    fn fill(entry: &Path, files: usize) {
        std::fs::create_dir_all(entry.join("a/b")).unwrap();
        for index in 0..files {
            std::fs::write(entry.join(format!("a/b/file-{index}")), "x").unwrap();
        }
    }

    /// A run stopped part-way (its deadline, as a killed run would be) leaves
    /// the entry in the trash, and the next run finishes it.
    #[test]
    fn the_deleter_resumes_an_entry_stopped_part_way() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join(TRASH_DIR);
        let entry = trash.join("0000000000001-1-1-0");
        fill(&entry, 50);
        // A tree half-removed by an earlier run.
        for index in 0..25 {
            std::fs::remove_file(entry.join(format!("a/b/file-{index}"))).unwrap();
        }
        let stopped = drain_trash(
            std::slice::from_ref(&trash),
            Instant::now(),
            DeleterPace::Unpaced,
        );
        assert_eq!(stopped.entries_removed, 0);
        assert!(entry.exists(), "a spent deadline removes nothing more");

        let resumed = drain_trash(
            std::slice::from_ref(&trash),
            Instant::now() + Duration::from_secs(600),
            DeleterPace::Unpaced,
        );
        assert_eq!(resumed.entries_removed, 1, "{resumed:?}");
        assert!(!entry.exists());
        assert_eq!(resumed.entries_pending, 0);
    }

    /// Entries go oldest first; one that keeps failing goes after the others
    /// and is reported by name.
    #[test]
    fn a_repeatedly_failing_entry_never_wedges_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join(TRASH_DIR);
        let stuck = trash.join("0000000000001-1-1-0");
        let next = trash.join("0000000000002-2-1-0");
        fill(&stuck, 1);
        fill(&next, 1);
        std::fs::write(failures_path(&stuck), "3").unwrap();
        let order = trash_entries(&trash)
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        assert_eq!(order, vec!["0000000000002-2-1-0", "0000000000001-1-1-0"]);
    }

    /// The pace follows disk pressure and load, never memory: below the
    /// headroom it is unpaced, under load it keeps the floor.
    #[test]
    fn the_pace_ignores_memory_and_keeps_a_floor() {
        assert_eq!(DeleterPace::for_host(true, true), DeleterPace::Unpaced);
        assert_eq!(DeleterPace::for_host(true, false), DeleterPace::Unpaced);
        assert_eq!(DeleterPace::for_host(false, true), DeleterPace::Floor);
        assert_eq!(DeleterPace::for_host(false, false), DeleterPace::Steady);
        assert_eq!(DeleterPace::Unpaced.unlinks_per_second(), None);
        assert_eq!(
            DeleterPace::Floor.unlinks_per_second(),
            Some(DELETER_FLOOR_UNLINKS_PER_SECOND)
        );
        assert!(DELETER_FLOOR_UNLINKS_PER_SECOND >= 200);
    }

    /// The deleter runs with the worker's pacer set aside, so memory
    /// pressure (which that pacer acts on) cannot stop it, and the pacer is
    /// back afterwards.
    #[test]
    fn the_deleter_runs_without_the_memory_pacer() {
        let installed_inside = crate::gc::unattended_pacing::without_pacer(
            crate::gc::unattended_pacing::pacer_installed,
        );
        assert!(!installed_inside);
    }

    /// A lock file left by a holder that is gone (its descriptor closed, as a
    /// dead process's is) is taken over; a held lock is not.
    #[test]
    fn a_dead_deleter_lock_owner_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DELETER_LOCK_FILE);
        std::fs::write(&path, "pid 999999 since 1\n").unwrap();
        let first = try_deleter_lock(&path).unwrap();
        assert!(
            first.is_some(),
            "a stale holder line does not keep anyone out"
        );
        assert!(
            try_deleter_lock(&path).unwrap().is_none(),
            "a live holder does"
        );
        drop(first);
        assert!(try_deleter_lock(&path).unwrap().is_some());
        let holder = std::fs::read_to_string(&path).unwrap();
        assert!(
            holder.starts_with(&format!("pid {}", std::process::id())),
            "{holder}"
        );
    }
}
