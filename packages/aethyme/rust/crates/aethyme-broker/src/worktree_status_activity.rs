//! Activity evidence from a checkout's changed files.
//!
//! Editing `src/a/b/c.rs` touches neither `src/` nor the checkout root, and
//! the Git index only changes on `add` or a status refresh, so directory and
//! index modification times miss deep edits. This module runs one
//! `git --no-optional-locks status --porcelain=v1 -z --untracked-files=all`
//! (ignored files left out; every file of a new untracked directory listed,
//! so a deep edit inside one is seen; `--no-optional-locks` so the probe
//! itself never rewrites the index),
//! bounded by time and by a cap on the paths it examines, and reads the
//! modification time of each modified or untracked path and of its parent
//! directory. The newest of them is the evidence.
//!
//! When the status call does not finish in time, fails, or the cap is
//! reached before any evidence, the result says so (`complete: false`) and
//! callers fall back to their previous evidence, counting the fallback.

use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The longest the status call may take.
pub(crate) const STATUS_ACTIVITY_BUDGET: Duration = Duration::from_secs(2);
/// Changed paths examined at most.
pub(crate) const STATUS_ACTIVITY_PATH_CAP: usize = 2_000;
/// Status output read at most.
const STATUS_ACTIVITY_MAX_BYTES: usize = 4 * 1024 * 1024;

/// What the changed files say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StatusActivity {
    /// The newest modification time (ms since the epoch) among the changed
    /// paths and their parent directories, if any was read.
    pub newest_ms: Option<i64>,
    /// Every changed path was examined.
    pub complete: bool,
    pub paths_examined: usize,
}

fn mtime_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// The changed paths `git status --porcelain=v1 -z` printed: modified,
/// staged, deleted (whose parent still tells), renamed (the new name) and
/// untracked entries.
pub(crate) fn changed_paths(stdout: &[u8]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut fields = stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        if field.len() < 4 {
            continue;
        }
        let status = &field[..2];
        // Bytes, not UTF-8: a path need not be valid UTF-8.
        let mut path = &field[3..];
        while let [rest @ .., b'/'] = path {
            path = rest;
        }
        if !path.is_empty() {
            paths.push(PathBuf::from(std::ffi::OsStr::from_bytes(path)));
        }
        // A rename or copy is followed by its source path.
        if status.contains(&b'R') || status.contains(&b'C') {
            fields.next();
        }
    }
    paths
}

/// Read the activity evidence of `worktree` (see the module documentation),
/// within `budget` and `cap` paths.
pub(crate) fn status_activity(worktree: &Path, budget: Duration, cap: usize) -> StatusActivity {
    let _span = crate::phase_telemetry::span("worktree_status_activity");
    let started = Instant::now();
    let mut command = crate::git::git_command();
    command
        .args([
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ])
        .current_dir(worktree);
    #[cfg(test)]
    if STATUS_UNAVAILABLE_FOR_TEST.with(std::cell::Cell::get) {
        crate::phase_telemetry::add("worktree_status_activity_unavailable", started.elapsed());
        return StatusActivity::default();
    }
    let output = match crate::bounded_output::output_limited(
        &mut command,
        budget.min(STATUS_ACTIVITY_BUDGET),
        STATUS_ACTIVITY_MAX_BYTES,
    ) {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) if output.status.success() => {
            output.stdout
        }
        _ => {
            crate::phase_telemetry::add("worktree_status_activity_unavailable", started.elapsed());
            return StatusActivity::default();
        }
    };
    // The broker's own runtime files (its database, logs, run state, the
    // notices it writes) are not the agent's work.
    let paths: Vec<PathBuf> = changed_paths(&output)
        .into_iter()
        .filter(|path| !crate::runtime_paths::is_broker_runtime_path(&path.to_string_lossy()))
        .collect();
    let mut activity = StatusActivity {
        complete: paths.len() <= cap,
        ..StatusActivity::default()
    };
    for relative in paths.iter().take(cap) {
        let path = worktree.join(relative);
        let parent = path.parent().and_then(mtime_ms);
        for ms in [mtime_ms(&path), parent].into_iter().flatten() {
            activity.newest_ms = Some(activity.newest_ms.map_or(ms, |known| known.max(ms)));
        }
        activity.paths_examined += 1;
    }
    if !activity.complete {
        crate::phase_telemetry::add("worktree_status_activity_capped", Duration::ZERO);
    }
    crate::phase_telemetry::add("worktree_status_activity", started.elapsed());
    activity
}

/// Whether the changed files show activity at or after `since`: `Some(true)`
/// when one does, `Some(false)` when every changed path was examined and none
/// does, `None` when the probe could not tell (timeout, failure, or the cap
/// reached without evidence): the caller keeps its previous behaviour.
pub(crate) fn recent_status_activity(worktree: &Path, since: SystemTime) -> Option<bool> {
    let since_ms = since
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_millis()).ok())
        .unwrap_or(0);
    let activity = status_activity(worktree, STATUS_ACTIVITY_BUDGET, STATUS_ACTIVITY_PATH_CAP);
    if activity.newest_ms.is_some_and(|newest| newest >= since_ms) {
        return Some(true);
    }
    if activity.complete {
        Some(false)
    } else {
        crate::phase_telemetry::add("worktree_status_activity_fallback", Duration::ZERO);
        None
    }
}

/// The longest the modification-time walk may take.
pub(crate) const MTIME_WALK_BUDGET: Duration = Duration::from_secs(5);
/// Entries the walk reads at most.
pub(crate) const MTIME_WALK_ENTRIES: usize = 200_000;

#[cfg(test)]
thread_local! {
    /// Makes the changed-file probe report that `git status` did not finish.
    pub(crate) static STATUS_UNAVAILABLE_FOR_TEST: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// When `git status` cannot finish in time: walk the checkout (every file
/// and directory, ignored ones included, links not followed) for any
/// modification time at or after `since`, until `deadline` or `cap`
/// entries. Left out: `.git`, the broker's own runtime paths (the same
/// catalogue the status probe excludes), and a directory `regenerable`
/// accepts (the caller passes the strip catalogue's own witness check, so
/// a tracked source directory merely named like a regenerable one is
/// walked). `Some(true)`: one found (activity); `Some(false)`: the walk
/// finished and found none, which proves nothing else changed; `None`: it
/// stopped at its deadline or cap (the caller keeps the session).
pub(crate) fn recent_mtime_walk(
    worktree: &Path,
    since: SystemTime,
    deadline: Instant,
    cap: usize,
    regenerable: &dyn Fn(&Path, &str) -> bool,
) -> Option<bool> {
    let _span = crate::phase_telemetry::span("worktree_mtime_walk");
    let mut read = 0_usize;
    let mut stack = vec![worktree.to_path_buf()];
    let newer =
        |metadata: &std::fs::Metadata| metadata.modified().is_ok_and(|modified| modified >= since);
    let runtime = |path: &Path, directory: bool| {
        path.strip_prefix(worktree).is_ok_and(|relative| {
            let relative = relative.to_string_lossy();
            crate::runtime_paths::is_broker_runtime_path(&relative)
                || (directory
                    && crate::runtime_paths::is_broker_runtime_path(&format!("{relative}/")))
        })
    };
    if std::fs::symlink_metadata(worktree).is_ok_and(|metadata| newer(&metadata)) {
        crate::phase_telemetry::add("worktree_mtime_walk_found_recent", Duration::ZERO);
        return Some(true);
    }
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            crate::phase_telemetry::add("worktree_mtime_walk_incomplete", Duration::ZERO);
            return None;
        };
        for entry in entries {
            read += 1;
            if read > cap || Instant::now() >= deadline {
                crate::phase_telemetry::add("worktree_mtime_walk_incomplete", Duration::ZERO);
                return None;
            }
            let Ok(entry) = entry else {
                crate::phase_telemetry::add("worktree_mtime_walk_incomplete", Duration::ZERO);
                return None;
            };
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let name = entry.file_name();
            if name == std::ffi::OsStr::new(".git") || runtime(&path, metadata.is_dir()) {
                continue;
            }
            if metadata.is_dir() && name.to_str().is_some_and(|name| regenerable(&path, name)) {
                crate::phase_telemetry::add(
                    "worktree_mtime_walk_skipped_regenerable",
                    Duration::ZERO,
                );
                continue;
            }
            if newer(&metadata) {
                crate::phase_telemetry::add("worktree_mtime_walk_found_recent", Duration::ZERO);
                return Some(true);
            }
            if metadata.is_dir() {
                stack.push(path);
            }
        }
    }
    crate::phase_telemetry::add("worktree_mtime_walk_complete", Duration::ZERO);
    Some(false)
}

/// Date every file and directory under `dir` (not `.git`) to `then`, so a
/// fixture's changed files read as old as its Git metadata.
#[cfg(test)]
pub(crate) fn age_tree_for_test(dir: &Path, then: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            age_tree_for_test(&path, then);
        }
        if !kind.is_symlink()
            && let Ok(file) = std::fs::File::open(&path)
        {
            crate::warn_unrecorded("age a fixture file", file.set_modified(then));
        }
    }
    if let Ok(file) = std::fs::File::open(dir) {
        crate::warn_unrecorded("age a fixture directory", file.set_modified(then));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_entries_yield_changed_paths_and_skip_rename_sources() {
        let out = b" M src/a/b/c.rs\0R  new.rs\0old.rs\0?? notes/\0D  gone.txt\0";
        assert_eq!(
            changed_paths(out),
            ["src/a/b/c.rs", "new.rs", "notes", "gone.txt"]
                .iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        );
        // A path that is not UTF-8 is kept, byte for byte.
        let odd = b"?? caf\xe9.txt\0";
        assert_eq!(
            changed_paths(odd),
            vec![PathBuf::from(std::ffi::OsStr::from_bytes(b"caf\xe9.txt"))]
        );
    }

    /// A deep edit (no directory above it changes) is activity; with the
    /// files old again it is not; the cap leaves the answer to the caller.
    #[test]
    fn a_deep_edit_is_recent_activity() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path();
        let git = |args: &[&str]| {
            assert!(
                crate::git::git_command()
                    .args(args)
                    .current_dir(worktree)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::create_dir_all(worktree.join("src/a/b")).unwrap();
        std::fs::write(worktree.join("src/a/b/c.rs"), "one\n").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ]);
        let old = SystemTime::now() - Duration::from_secs(10 * 3_600);
        let since = SystemTime::now() - Duration::from_secs(4 * 3_600);
        let set = |path: &Path, when: SystemTime| {
            std::fs::File::open(path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        };
        // Clean checkout: nothing changed, so nothing says active.
        assert_eq!(recent_status_activity(worktree, since), Some(false));
        // The broker writing its own runtime files is not activity.
        std::fs::create_dir_all(worktree.join(".aethyme/run")).unwrap();
        std::fs::write(worktree.join(".aethyme/run/state.json"), "{}\n").unwrap();
        std::fs::write(worktree.join(".aethyme/broker.db-wal"), "wal\n").unwrap();
        assert_eq!(recent_status_activity(worktree, since), Some(false));
        std::fs::write(worktree.join("src/a/b/c.rs"), "two\n").unwrap();
        set(&worktree.join("src/a/b"), old);
        assert_eq!(recent_status_activity(worktree, since), Some(true));
        set(&worktree.join("src/a/b/c.rs"), old);
        assert_eq!(recent_status_activity(worktree, since), Some(false));
        // A deep edit inside a new untracked directory is seen too.
        std::fs::create_dir_all(worktree.join("fresh/x/y")).unwrap();
        std::fs::write(worktree.join("fresh/x/y/z.rs"), "new\n").unwrap();
        for dir in ["fresh/x/y", "fresh/x", "fresh", ""] {
            set(&worktree.join(dir), old);
        }
        assert_eq!(recent_status_activity(worktree, since), Some(true));
        set(&worktree.join("fresh/x/y/z.rs"), old);
        // Over the cap without evidence: the caller decides.
        let capped = status_activity(worktree, STATUS_ACTIVITY_BUDGET, 0);
        assert!(!capped.complete);
        assert_eq!(capped.newest_ms, None);
    }

    /// The fallback walk finds a deep recent file, proves an old checkout
    /// idle, leaves the skipped trees out, and gives up at its cap.
    #[test]
    fn the_mtime_walk_finds_recent_files_and_fails_closed_at_its_cap() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path();
        std::fs::create_dir_all(worktree.join("src/a/b")).unwrap();
        std::fs::write(worktree.join("src/a/b/c.rs"), "x\n").unwrap();
        std::fs::create_dir_all(worktree.join("node_modules/dep")).unwrap();
        std::fs::write(worktree.join("node_modules/dep/index.js"), "x\n").unwrap();
        std::fs::create_dir_all(worktree.join("src/codegen/target")).unwrap();
        std::fs::write(worktree.join("src/codegen/target/mod.rs"), "x\n").unwrap();
        std::fs::create_dir_all(worktree.join(".aethyme/run")).unwrap();
        std::fs::write(worktree.join(".aethyme/run/state.json"), "{}\n").unwrap();
        std::fs::write(
            worktree.join(".aethyme/prepare.toml"),
            "schema_version = 1\n",
        )
        .unwrap();
        let old = SystemTime::now() - Duration::from_secs(10 * 3_600);
        let since = SystemTime::now() - Duration::from_secs(4 * 3_600);
        // Only the real install is regenerable (its marker), never a
        // source directory that happens to share a catalogued name.
        let regenerable = |path: &Path, name: &str| {
            name == "node_modules" && path.join(".modules.yaml").is_file()
        };
        std::fs::write(worktree.join("node_modules/.modules.yaml"), "x\n").unwrap();
        let far = || Instant::now() + MTIME_WALK_BUDGET;
        let touch = |path: &str| {
            std::fs::File::open(worktree.join(path))
                .unwrap()
                .set_modified(SystemTime::now())
                .unwrap();
        };
        let walk = |deadline: Instant, cap: usize| {
            recent_mtime_walk(worktree, since, deadline, cap, &regenerable)
        };
        age_tree_for_test(worktree, old);
        // The regenerable tree and the broker's runtime files do not count.
        touch("node_modules/dep/index.js");
        touch(".aethyme/run/state.json");
        assert_eq!(walk(far(), MTIME_WALK_ENTRIES), Some(false));
        // A tracked source directory named like a regenerable one does.
        touch("src/codegen/target/mod.rs");
        assert_eq!(walk(far(), MTIME_WALK_ENTRIES), Some(true));
        age_tree_for_test(worktree, old);
        touch("node_modules/dep/index.js");
        touch(".aethyme/prepare.toml");
        assert_eq!(
            walk(far(), MTIME_WALK_ENTRIES),
            Some(true),
            "the broker's config is work"
        );
        age_tree_for_test(worktree, old);
        touch("node_modules/dep/index.js");
        touch("src/a/b/c.rs");
        assert_eq!(walk(far(), MTIME_WALK_ENTRIES), Some(true));
        // Its cap and its deadline fail closed.
        assert_eq!(walk(far(), 1), None);
        assert_eq!(walk(Instant::now(), MTIME_WALK_ENTRIES), None);
    }
}
