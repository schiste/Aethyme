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
    /// `git status` ran out of its time, now or earlier in this process.
    pub timed_out: bool,
}

/// What one look at a checkout's activity read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WorktreeActivity {
    /// Newest of the Git index and HEAD.
    pub metadata_ms: i64,
    pub status: StatusActivity,
}

impl WorktreeActivity {
    /// The newest evidence: the index, HEAD or a changed file.
    pub(crate) fn newest_ms(&self) -> i64 {
        self.status
            .newest_ms
            .map_or(self.metadata_ms, |changed| changed.max(self.metadata_ms))
    }
}

/// Record one look: `look` names the lane and the look (a fixed set).
pub(crate) fn note_look(look: &str, elapsed: Duration, timed_out: bool) {
    crate::phase_telemetry::add_keyed("worktree_activity_look", look, elapsed);
    if timed_out {
        crate::phase_telemetry::add_keyed("worktree_activity_look_timed_out", look, elapsed);
    }
}

/// Record a first look whose probe did not run because `reason`, a cheaper
/// check, already kept the session.
pub(crate) fn note_look_avoided(look: &str, reason: &str) {
    count_look(
        "worktree_activity_look_avoided",
        &format!("{look}.{reason}"),
    );
}

fn count_look(name: &'static str, look: &str) {
    crate::phase_telemetry::add_keyed(name, look, Duration::ZERO);
}

/// Record that the Git index or HEAD alone decided a look.
pub(crate) fn note_index_head_decides(look: &str) {
    count_look("worktree_activity_index_head_decides", look);
}

/// Record that only the probe's newer file kept the session: told apart by
/// whether the probe examined every changed path or stopped at its cap.
fn note_probe_changed_verdict(look: &str, complete: bool) {
    count_look(
        if complete {
            "worktree_activity_probe_changed_verdict"
        } else {
            "worktree_activity_capped_probe_changed_verdict"
        },
        look,
    );
}

/// Record a probe whose newest file outranks the index, HEAD and `floor_ms`:
/// an upper bound on the looks where the probe can have mattered to anything.
fn note_probe_newest(look: &str, activity: &WorktreeActivity, floor_ms: i64) {
    if activity
        .status
        .newest_ms
        .is_some_and(|changed| changed > activity.metadata_ms.max(floor_ms))
    {
        count_look("worktree_activity_probe_newest", look);
    }
}

/// Record what decided a look that asks whether a session is recent:
/// `recent` judges a time, `session_ms` is the session's recorded activity
/// and `other_ms` the newest evidence from elsewhere (that included).
pub(crate) fn note_recent_verdict(
    look: &str,
    activity: &WorktreeActivity,
    session_ms: i64,
    other_ms: i64,
    recent: impl Fn(i64) -> bool,
) {
    // Against the evidence every reader shares: whether a holder moved on is
    // judged without the hook's or the merge's time, so a probe newer than
    // the session's own activity alone can matter there.
    note_probe_newest(look, activity, session_ms);
    // The session's own age already decides: no evidence does.
    if recent(0) {
        return;
    }
    if recent(activity.metadata_ms) {
        note_index_head_decides(look);
    } else if !recent(activity.metadata_ms.max(other_ms))
        && recent(activity.newest_ms().max(other_ms))
    {
        note_probe_changed_verdict(look, activity.status.complete);
    }
}

/// Record what decided the last look, which keeps the session when the newest
/// time differs from the `claimed` look's (asked only while HEAD is the same).
pub(crate) fn note_close_verdict(look: &str, claimed: &WorktreeActivity, now: &WorktreeActivity) {
    note_probe_newest(look, now, 0);
    if now.newest_ms() == claimed.newest_ms() {
        return;
    }
    if now.metadata_ms > claimed.newest_ms()
        || (now.metadata_ms != claimed.metadata_ms
            && now.status.newest_ms == claimed.status.newest_ms)
    {
        note_index_head_decides(look);
    } else if now.status.complete && claimed.status.complete {
        note_probe_changed_verdict(look, true);
    } else {
        // The two probes did not answer alike (one timed out, failed or
        // stopped at its cap): not necessarily a change in the checkout.
        count_look("worktree_activity_probe_answer_differs", look);
    }
}

/// How long after a probe a Git call on the same checkout counts as right
/// after it.
const PROBED_JUST_BEFORE: Duration = Duration::from_secs(30);

thread_local! {
    /// The checkout this thread last ran the probe's `git status` in, when
    /// that ended and whether it ran out of time.
    static LAST_PROBE: std::cell::RefCell<Option<(PathBuf, Instant, bool)>> =
        const { std::cell::RefCell::new(None) };
}

fn remember_probe(worktree: &Path, timed_out: bool) {
    LAST_PROBE.with(|last| {
        *last.borrow_mut() = Some((worktree.to_path_buf(), Instant::now(), timed_out));
    });
}

/// Whether this thread probed `worktree` just before, as a telemetry key.
pub(crate) fn probed_just_before(worktree: &Path) -> &'static str {
    LAST_PROBE.with(|last| match last.borrow().as_ref() {
        Some((path, ended, timed_out))
            if path == worktree && ended.elapsed() <= PROBED_JUST_BEFORE =>
        {
            if *timed_out {
                "after_timed_out_probe"
            } else {
                "after_probe"
            }
        }
        _ => "no_probe",
    })
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
    // A checkout whose status call already ran out of time in this process
    // gets the same answer at once: the worker's lanes ask about the same
    // checkouts several times a run, and on the busiest repository the
    // repeats cost 52 to 59 s of 2 s timeouts per run.
    if timed_out_before(worktree) {
        crate::phase_telemetry::add(
            "worktree_status_activity_unavailable_remembered",
            Duration::ZERO,
        );
        return StatusActivity {
            timed_out: true,
            ..StatusActivity::default()
        };
    }
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
            remember_probe(worktree, false);
            output.stdout
        }
        Ok(crate::bounded_output::LimitedOutput::Deadline) => {
            remember_timeout(worktree);
            remember_probe(worktree, true);
            crate::phase_telemetry::add("worktree_status_activity_unavailable", started.elapsed());
            return StatusActivity {
                timed_out: true,
                ..StatusActivity::default()
            };
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

/// Checkouts whose status call ran out of time in this process. A process
/// is one command or one worker run, so a checkout is asked again by the
/// next one.
static TIMED_OUT: std::sync::Mutex<Option<std::collections::BTreeSet<PathBuf>>> =
    std::sync::Mutex::new(None);

fn timed_out_before(worktree: &Path) -> bool {
    TIMED_OUT
        .lock()
        .ok()
        .is_some_and(|set| set.as_ref().is_some_and(|set| set.contains(worktree)))
}

pub(crate) fn remember_timeout(worktree: &Path) {
    if let Ok(mut set) = TIMED_OUT.lock() {
        set.get_or_insert_with(Default::default)
            .insert(worktree.to_path_buf());
    }
}

/// The idle strip's one look, in [`note_look`] and its siblings.
pub(crate) const IDLE_STRIP_LOOK: &str = "idle_strip";

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
    let started = Instant::now();
    let activity = status_activity(worktree, STATUS_ACTIVITY_BUDGET, STATUS_ACTIVITY_PATH_CAP);
    note_look(IDLE_STRIP_LOOK, started.elapsed(), activity.timed_out);
    // Asked only once nothing cheaper showed activity.
    if activity.newest_ms.is_some_and(|newest| newest >= since_ms) {
        count_look("worktree_activity_probe_newest", IDLE_STRIP_LOOK);
        note_probe_changed_verdict(IDLE_STRIP_LOOK, activity.complete);
    }
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
/// walked) and a directory `dependency_root` accepts (a catalogued name the
/// checkout's ignore rules hide, with no witness; its own time is still
/// read). `Some(true)`: one found (activity); `Some(false)`: the walk
/// finished and found none, which proves nothing else changed; `None`: it
/// stopped at its deadline or cap (the caller keeps the session).
pub(crate) fn recent_mtime_walk(
    worktree: &Path,
    since: SystemTime,
    deadline: Instant,
    cap: usize,
    regenerable: &dyn Fn(&Path, &str) -> bool,
    dependency_root: &dyn Fn(&Path, &str) -> bool,
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
            // A dependency directory the checkout's ignore rules hide but
            // that has no strip witness (a workspace package's
            // `node_modules` without its manager's marker): its own time
            // was read above (a fresh install moves it), its contents are
            // not the agent's work, and walking them is what ran the walk
            // out of time (100 000 entries and more per checkout).
            if metadata.is_dir()
                && name
                    .to_str()
                    .is_some_and(|name| dependency_root(&path, name))
            {
                crate::phase_telemetry::add(
                    "worktree_mtime_walk_skipped_dependency_root",
                    Duration::ZERO,
                );
                continue;
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

    /// A checkout whose status call ran out of time is not waited on again
    /// by the same process: the answer is the timeout's, at once.
    #[test]
    fn a_checkout_that_timed_out_is_answered_from_memory() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("checkout");
        std::fs::create_dir_all(&worktree).unwrap();
        assert!(!timed_out_before(&worktree));
        remember_timeout(&worktree);
        assert!(timed_out_before(&worktree));
        assert!(!timed_out_before(&dir.path().join("other")));

        let recorder = crate::phase_telemetry::Recorder::default();
        let activity = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            status_activity(&worktree, STATUS_ACTIVITY_BUDGET, STATUS_ACTIVITY_PATH_CAP)
        };

        assert_eq!(
            activity,
            StatusActivity {
                timed_out: true,
                ..StatusActivity::default()
            }
        );
        // Answered from memory: no `git status` ran just before.
        assert_eq!(probed_just_before(&worktree), "no_probe");
        let (_, counts) = recorder.totals();
        assert_eq!(
            counts.get("worktree_status_activity_unavailable_remembered"),
            Some(&1),
            "{counts:?}"
        );
        assert!(!counts.contains_key("subprocess_git"), "{counts:?}");
    }

    /// What decided a look is recorded once, under its name. Mutations
    /// caught: counting a verdict other evidence outranks or a session its own
    /// age keeps, counting a capped probe as a complete one, and holding the
    /// upper bound to the other evidence or to the verdict.
    #[test]
    fn the_evidence_that_decided_a_look_is_counted() {
        let recorder = crate::phase_telemetry::Recorder::default();
        let look = |name: &str, metadata_ms, changed_ms, complete, other_ms, since: i64| {
            let activity = WorktreeActivity {
                metadata_ms,
                status: StatusActivity {
                    newest_ms: changed_ms,
                    complete,
                    ..StatusActivity::default()
                },
            };
            note_recent_verdict(name, &activity, 7, other_ms, |ms| ms >= since);
        };
        {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            look("index", 10, Some(20), true, 0, 10);
            look("probe", 5, Some(20), true, 0, 10);
            look("outranked", 5, Some(20), true, 15, 10);
            look("capped", 5, Some(20), false, 0, 10);
            look("old", 5, Some(6), true, 0, 10);
            look("young", 10, Some(20), true, 0, 0);
            note_look("landed_first", Duration::ZERO, false);
            note_look("landed_close", Duration::ZERO, true);
        }
        let attached = recorder.attachments();
        let keys = |name: &str| {
            attached[name]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(keys("worktree_activity_index_head_decides"), ["index"]);
        assert_eq!(keys("worktree_activity_probe_changed_verdict"), ["probe"]);
        assert_eq!(
            keys("worktree_activity_capped_probe_changed_verdict"),
            ["capped"]
        );
        // Every probe newer than the index, HEAD and the session's activity,
        // whatever was decided; not "old" (6, the session was active at 7).
        assert_eq!(
            keys("worktree_activity_probe_newest"),
            ["capped", "index", "outranked", "probe", "young"]
        );
        assert_eq!(
            keys("worktree_activity_look"),
            ["landed_close", "landed_first"]
        );
        assert_eq!(keys("worktree_activity_look_timed_out"), ["landed_close"]);
    }

    /// The last look compares with `!=` and tells a changed checkout from
    /// two probes that did not answer alike. Mutations caught: comparing with
    /// `>`, counting an equal time, counting a probe that stopped answering
    /// as a change, and taking a moved index for one.
    #[test]
    fn the_last_look_tells_a_change_from_a_probe_that_answered_differently() {
        let at = |metadata_ms, newest_ms, complete| WorktreeActivity {
            metadata_ms,
            status: StatusActivity {
                newest_ms,
                complete,
                ..StatusActivity::default()
            },
        };
        let seen = |claimed: WorktreeActivity, now: WorktreeActivity| {
            let recorder = crate::phase_telemetry::Recorder::default();
            {
                let _installed = crate::phase_telemetry::install(recorder.clone());
                note_close_verdict("landed_close", &claimed, &now);
            }
            recorder
                .attachments()
                .into_keys()
                .filter(|name| name != "worktree_activity_probe_newest")
                .collect::<Vec<_>>()
        };
        let index = ["worktree_activity_index_head_decides"];
        let changed = ["worktree_activity_probe_changed_verdict"];
        let differs = ["worktree_activity_probe_answer_differs"];
        assert!(seen(at(5, Some(8), true), at(5, Some(8), true)).is_empty());
        assert!(seen(at(5, None, false), at(5, Some(4), true)).is_empty());
        assert_eq!(seen(at(5, Some(8), true), at(9, Some(8), true)), index);
        assert_eq!(seen(at(5, None, true), at(4, None, true)), index);
        assert_eq!(seen(at(5, Some(8), true), at(5, Some(9), true)), changed);
        assert_eq!(seen(at(5, Some(8), true), at(5, Some(7), true)), changed);
        assert_eq!(seen(at(5, Some(8), true), at(5, None, true)), changed);
        assert_eq!(seen(at(5, Some(8), true), at(5, None, false)), differs);
        assert_eq!(seen(at(5, None, false), at(5, Some(8), true)), differs);
        assert_eq!(seen(at(5, Some(8), false), at(5, Some(9), true)), differs);
    }

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
        assert_eq!(probed_just_before(worktree), "after_probe");
        // The broker writing its own runtime files is not activity.
        std::fs::create_dir_all(worktree.join(".aethyme/run")).unwrap();
        std::fs::write(worktree.join(".aethyme/run/state.json"), "{}\n").unwrap();
        std::fs::write(worktree.join(".aethyme/broker.db-wal"), "wal\n").unwrap();
        assert_eq!(recent_status_activity(worktree, since), Some(false));
        std::fs::write(worktree.join("src/a/b/c.rs"), "two\n").unwrap();
        set(&worktree.join("src/a/b"), old);
        let recorder = crate::phase_telemetry::Recorder::default();
        {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            assert_eq!(recent_status_activity(worktree, since), Some(true));
        }
        let attached = recorder.attachments();
        for name in [
            "worktree_activity_look",
            "worktree_activity_probe_newest",
            "worktree_activity_probe_changed_verdict",
        ] {
            assert_eq!(attached[name][IDLE_STRIP_LOOK]["count"], 1, "{name}");
        }
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
        let dependency_root = |_: &Path, _: &str| false;
        let walk = |deadline: Instant, cap: usize| {
            recent_mtime_walk(
                worktree,
                since,
                deadline,
                cap,
                &regenerable,
                &dependency_root,
            )
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

    /// An ignored dependency directory without a witness is not walked (its
    /// 100 000 entries ran the walk out of time), yet its own time still
    /// counts, and an unignored directory of the same name is walked.
    #[test]
    fn the_mtime_walk_skips_unwitnessed_dependency_roots_but_reads_their_time() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path();
        std::fs::create_dir_all(worktree.join("apps/web/node_modules/dep")).unwrap();
        std::fs::write(worktree.join("apps/web/node_modules/dep/index.js"), "x\n").unwrap();
        std::fs::create_dir_all(worktree.join("src/node_modules")).unwrap();
        std::fs::write(worktree.join("src/node_modules/mod.js"), "x\n").unwrap();
        let old = SystemTime::now() - Duration::from_secs(10 * 3_600);
        let since = SystemTime::now() - Duration::from_secs(4 * 3_600);
        let never = |_: &Path, _: &str| false;
        let ignored_only_apps = |path: &Path, name: &str| {
            name == "node_modules" && path.starts_with(worktree.join("apps"))
        };
        let far = || Instant::now() + MTIME_WALK_BUDGET;
        let touch = |path: &str| {
            std::fs::File::open(worktree.join(path))
                .unwrap()
                .set_modified(SystemTime::now())
                .unwrap();
        };
        let walk = |dependency: &dyn Fn(&Path, &str) -> bool, cap: usize| {
            recent_mtime_walk(worktree, since, far(), cap, &never, dependency)
        };
        age_tree_for_test(worktree, old);
        touch("apps/web/node_modules/dep/index.js");
        // Skipped: the recent file inside is not seen, and the entries it
        // holds do not count against the cap (the walk finishes at 6).
        assert_eq!(walk(&ignored_only_apps, 6), Some(false));
        // Not skipped when nothing says it is an ignored dependency root.
        assert_eq!(walk(&never, MTIME_WALK_ENTRIES), Some(true));
        // The root's own time is evidence even when its contents are skipped.
        age_tree_for_test(worktree, old);
        touch("apps/web/node_modules");
        assert_eq!(walk(&ignored_only_apps, MTIME_WALK_ENTRIES), Some(true));
        // An unignored directory of the same name is walked.
        age_tree_for_test(worktree, old);
        touch("src/node_modules/mod.js");
        assert_eq!(walk(&ignored_only_apps, MTIME_WALK_ENTRIES), Some(true));
    }
}
