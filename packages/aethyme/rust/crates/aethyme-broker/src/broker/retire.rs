//! Retire a closed session's checkout once, at close: decide, rename, delete.
//!
//! A closed checkout used to wait for a removal lane that re-read every live
//! session and re-resolved every live path per page, kept it for any of
//! some fifteen reasons, paused on memory pressure, and only began after a
//! 24 hour grace. On a busy repository that removed one checkout per 45
//! minutes against dozens created.
//!
//! [`Broker::retire_closed_checkout`] decides once, with hard preconditions
//! only (a broker-owned checkout of a closed session that no open session
//! uses, holding no lease, running no gate or operation, used by no
//! process). A failed precondition keeps the checkout for the next run under
//! a named counter. Then:
//!
//! - a checkout whose commits are all on a delivery target, with no tracked
//!   change, no untracked file and no protected ignored file, is moved into
//!   the trash at once (its content is in Git; ignored leftovers are
//!   catalogued regenerable output, or the plan refuses);
//! - any other checkout gets a recovery archive first (bundle of the
//!   unlanded commits, staged and unstaged patches, untracked files,
//!   protected ignored files), written and read back by the existing
//!   archive writer, which then moves the checkout into the trash. An
//!   archive larger than [`RETIRE_ARCHIVE_MAX_BYTES`] keeps the checkout
//!   and is shown by `broker status` by session id.
//!
//! Moving into the trash is a `renameat(2)` into the pool's private broker
//! trash plus dropping the Git registration, only for a spawned session's
//! checkout inside a broker-owned pool (`checkout_trash`); the host deleter
//! later unlinks only the entries recorded here at rename time.
//! [`Broker::run_checkout_retirement`] is the worker lane: it walks closed
//! sessions that still have a checkout by session-id cursor, with the open
//! sessions' paths resolved once per run, then drains the trash.

use super::*;
use std::time::Instant;

/// Meta key: when the retirement lane is next due (epoch ms). Absent means
/// due, so a repository's existing leftovers are judged on the first run.
pub(crate) const CHECKOUT_RETIREMENT_DUE_KEY: &str = "checkout_retirement.due_at_ms";
/// Meta key: the last session id the backlog sweep judged.
const CHECKOUT_RETIREMENT_CURSOR_KEY: &str = "checkout_retirement.cursor";
/// Meta key: the last cleaned session id the leftover-folder lap judged.
const CHECKOUT_RETIREMENT_CLEANED_CURSOR_KEY: &str = "checkout_retirement.cleaned_cursor";
/// Meta key: the trash entries this repository recorded when it moved a
/// checkout in (JSON: trash directory, then entry name, then device, inode,
/// session, failed deletions and whether the rename is still unconfirmed).
/// The deleter deletes nothing else. Every change is a compare-and-set.
pub(crate) const CHECKOUT_RETIREMENT_TRASH_ROWS_KEY: &str = "checkout_retirement.trash_rows";
/// Meta key: progress of the last runs, for `broker status` (JSON).
pub(crate) const CHECKOUT_RETIREMENT_PROGRESS_KEY: &str = "checkout_retirement.progress";
/// Meta key: sessions whose recovery archive would exceed the cap (JSON
/// array of session ids).
pub(crate) const CHECKOUT_RETIREMENT_TOO_LARGE_KEY: &str = "checkout_retirement.archive_too_large";
/// Meta key: sessions kept because a merge, rebase, cherry-pick, revert or
/// bisect is in progress or the index holds unmerged entries (JSON array).
pub(crate) const CHECKOUT_RETIREMENT_IN_PROGRESS_KEY: &str =
    "checkout_retirement.operation_in_progress";
/// Meta key: remembered keeps (JSON: session id to key, reason, time and
/// attempts), so a kept checkout is not re-planned every lap.
pub(crate) const CHECKOUT_RETIREMENT_KEEPS_KEY: &str = "checkout_retirement.keeps";
/// Meta key: sessions whose checkout the broker confirmed moved into the
/// trash (JSON array), kept after the entry is deleted until the session is
/// recorded as cleaned: the tombstone that lets a lost cleaned record be
/// reconciled.
pub(crate) const CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY: &str =
    "checkout_retirement.trashed_sessions";
/// Meta key: the last pin `prune_retired_pins` checked (its cursor).
const CHECKOUT_RETIREMENT_PIN_CURSOR_KEY: &str = "checkout_retirement.pin_cursor";
/// Pins checked for containment per run, at most.
#[cfg(not(test))]
const RETIRED_PIN_CHECKS_PER_RUN: usize = 64;
#[cfg(test)]
const RETIRED_PIN_CHECKS_PER_RUN: usize = 2;
/// Commits named by one checkout's HEAD reflog, per-worktree refs and
/// bisect state that are checked for pinning, at most; more keeps it.
const RETIRE_PIN_CANDIDATES_MAX: usize = 2_000;
/// The largest recovery archive retirement writes on its own.
pub(crate) const RETIRE_ARCHIVE_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// Closed sessions read per page.
const RETIRE_PAGE: usize = 64;
/// Meta key: closed or cleaned sessions last kept as superseded at their
/// path, with the row that superseded them (JSON: session id to that id).
pub(crate) const CHECKOUT_RETIREMENT_SUPERSEDED_KEY: &str = "checkout_retirement.superseded";
/// Remembered superseded sessions at most; the lowest ids go first.
const RETIRE_SUPERSEDED_MAX: usize = 16_384;
/// The longest a retirement run spends expiring retired-commit pins.
const RETIRE_PIN_PRUNE_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);
/// The least time a retirement run gives the strip pass, the closed lap
/// and the cleaned lap, whatever the phases before them used (bounded by
/// the lane's own deadline).
const RETIRE_STRIP_MIN_SHARE: std::time::Duration = std::time::Duration::from_secs(20);
const RETIRE_LAP_MIN_SHARE: std::time::Duration = std::time::Duration::from_secs(20);
const RETIRE_CLEANED_LAP_MIN_SHARE: std::time::Duration = std::time::Duration::from_secs(10);
/// Next run when nothing is left to do.
const RETIRE_IDLE_RECHECK_MS: i64 = 10 * 60 * 1_000;
/// Next run while a lap or the trash is unfinished.
const RETIRE_BUSY_RECHECK_MS: i64 = 60 * 1_000;
/// Runs in a row without shrinking before `broker status` says so.
pub(crate) const RETIRE_STALLED_RUNS: u32 = 3;
/// A remembered keep is judged again after this long, by attempt: 1 h, then
/// 6 h, then every 24 h. A changed HEAD or index is judged at once.
const RETIRE_KEEP_BACKOFF_MS: [i64; 3] = [3_600_000, 6 * 3_600_000, 24 * 3_600_000];
/// Remembered keeps at most; the oldest go first.
const RETIRE_KEEPS_MAX: usize = 4_096;
/// The longest one process-use observation may take: the run's single one,
/// or the fresh one a caller outside the lane takes before a rename.
const RETIRE_PROCESS_USE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
/// An archive (written, read back, re-planned, then the checkout moved) is
/// started only with at least this much of the lane's deadline left;
/// otherwise the lap stops at that checkout and the next run starts with it.
const RETIRE_ARCHIVE_MIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(90);
/// Keep details are cut to this many bytes.
const RETIRE_KEEP_DETAIL_MAX: usize = 400;
/// Inline retirement after an automatic close runs only with at least this
/// much of the caller's deadline left, and uses at most this much.
const RETIRE_INLINE_MIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);
const RETIRE_INLINE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

thread_local! {
    /// Set while the retirement lane (or inline retirement) runs on this
    /// thread: a refused trash move then keeps the checkout instead of
    /// falling back to a synchronous `git worktree remove`.
    static RETIREMENT_LANE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The open-file observation of the retirement run judging the checkout
    /// on this thread: the rename reuses it instead of taking another.
    static RUN_PROCESS_USE: std::cell::RefCell<Option<std::rc::Rc<crate::reclaim::OpenFileSnapshot>>> =
        const { std::cell::RefCell::new(None) };
    /// The session whose abandoned operation state was just archived: its
    /// move into the trash does not refuse for that state.
    static OPERATION_STATE_ARCHIVED: std::cell::Cell<Option<i64>> =
        const { std::cell::Cell::new(None) };
    /// Live sessions' resolved checkout paths, by session id and recorded
    /// path, while the lane runs on this thread: each live-checkout check of
    /// the lane resolves only sessions it has not seen (a new session, or one
    /// whose recorded path changed) instead of every live path again.
    static LANE_LIVE_PATHS: std::cell::RefCell<
        Option<std::collections::HashMap<(i64, String), crate::auto_cleanup::ResolvedWorktreePath>>,
    > = const { std::cell::RefCell::new(None) };
}

/// Whether the retirement lane runs on this thread.
pub(super) fn in_retirement_lane() -> bool {
    RETIREMENT_LANE.with(std::cell::Cell::get)
}

/// Marks this thread as the retirement lane until dropped.
struct RetirementLaneScope {
    previous: bool,
    installed_live_paths: bool,
    /// The lane archives unknown ignored directories file by file.
    _ignored_directories: crate::cleanup_resolve::ArchiveIgnoredDirectoriesScope,
}

impl RetirementLaneScope {
    fn enter() -> Self {
        let previous = RETIREMENT_LANE.with(|cell| cell.replace(true));
        let installed_live_paths = LANE_LIVE_PATHS.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.is_none() {
                *cache = Some(std::collections::HashMap::new());
                true
            } else {
                false
            }
        });
        Self {
            previous,
            installed_live_paths,
            _ignored_directories: crate::cleanup_resolve::ArchiveIgnoredDirectoriesScope::enter(),
        }
    }
}

impl Drop for RetirementLaneScope {
    fn drop(&mut self) {
        RETIREMENT_LANE.with(|cell| cell.set(self.previous));
        if self.installed_live_paths {
            LANE_LIVE_PATHS.with(|cache| *cache.borrow_mut() = None);
        }
    }
}

/// Resolve the checkout paths of `sessions`, in order, by `deadline`
/// (`None` past it). Inside the retirement lane a path this run already
/// resolved for the same session and recorded path is reused, so the
/// repeated live-checkout checks of one archive cost a database read
/// instead of canonicalizing every live checkout again; outside the lane
/// every path is resolved.
pub(super) fn resolve_live_paths_for_lane(
    sessions: &[Session],
    deadline: Option<Instant>,
) -> Option<Vec<crate::auto_cleanup::ResolvedWorktreePath>> {
    let cached = LANE_LIVE_PATHS.with(|cache| cache.borrow().is_some());
    if !cached {
        return crate::auto_cleanup::resolve_live_worktree_paths(sessions, deadline);
    }
    LANE_LIVE_PATHS.with(|cache| {
        let mut cache = cache.borrow_mut();
        let cache = cache.as_mut()?;
        let missing = sessions
            .iter()
            .filter(|session| !cache.contains_key(&(session.id, session.worktree_path.clone())))
            .cloned()
            .collect::<Vec<_>>();
        let reused = sessions.len() - missing.len();
        // The ones not resolved yet, in parallel (see
        // `resolve_live_worktree_paths`).
        let resolved = crate::auto_cleanup::resolve_live_worktree_paths(&missing, deadline)?;
        for (session, path) in missing.iter().zip(resolved) {
            cache.insert((session.id, session.worktree_path.clone()), path);
        }
        if reused > 0 {
            crate::phase_telemetry::add(
                "checkout_retirement_live_paths_reused",
                std::time::Duration::ZERO,
            );
        }
        sessions
            .iter()
            .map(|session| {
                cache
                    .get(&(session.id, session.worktree_path.clone()))
                    .cloned()
            })
            .collect()
    })
}

thread_local! {
    /// Commits a verified recovery archive's bundle carries for the checkout
    /// being moved on this thread: they, and their ancestors, need no pin.
    static ARCHIVED_COMMITS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Marks commits as carried by a verified archive until dropped.
pub(super) struct ArchivedCommitsScope(Vec<String>);

impl ArchivedCommitsScope {
    pub(super) fn enter(commits: Vec<String>) -> Self {
        Self(ARCHIVED_COMMITS.with(|cell| cell.replace(commits)))
    }
}

impl Drop for ArchivedCommitsScope {
    fn drop(&mut self) {
        let previous = std::mem::take(&mut self.0);
        ARCHIVED_COMMITS.with(|cell| *cell.borrow_mut() = previous);
    }
}

/// Shares the run's open-file observation with the rename until dropped.
struct RunProcessUseScope(Option<std::rc::Rc<crate::reclaim::OpenFileSnapshot>>);

impl RunProcessUseScope {
    fn enter(snapshot: std::rc::Rc<crate::reclaim::OpenFileSnapshot>) -> Self {
        Self(RUN_PROCESS_USE.with(|cell| cell.replace(Some(snapshot))))
    }
}

impl Drop for RunProcessUseScope {
    fn drop(&mut self) {
        let previous = self.0.take();
        RUN_PROCESS_USE.with(|cell| *cell.borrow_mut() = previous);
    }
}

/// Lets `session_id`'s move into the trash pass its archived operation
/// state until dropped.
struct OperationStateArchivedScope(Option<i64>);

impl OperationStateArchivedScope {
    fn enter(session_id: i64) -> Self {
        Self(OPERATION_STATE_ARCHIVED.with(|cell| cell.replace(Some(session_id))))
    }
}

impl Drop for OperationStateArchivedScope {
    fn drop(&mut self) {
        OPERATION_STATE_ARCHIVED.with(|cell| cell.set(self.0));
    }
}

/// `text` cut to [`RETIRE_KEEP_DETAIL_MAX`] bytes on a character boundary.
fn bounded_detail(text: &str) -> String {
    let mut end = text.len().min(RETIRE_KEEP_DETAIL_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
thread_local! {
    /// Test seam: the archive cap.
    static RETIRE_ARCHIVE_MAX_BYTES_FOR_TEST: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
    /// Test seam: runs after the plan, before the lifecycle claim.
    pub(super) static BEFORE_RETIRE_CLAIM: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
    /// Test seam: runs once a retirement run resolved the open paths.
    pub(super) static AFTER_OPEN_PATHS: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Use `cap` as the archive cap on this test thread until the guard drops.
#[cfg(test)]
pub(crate) fn retire_archive_cap_for_test(cap: u64) -> RetireArchiveCapRestore {
    RetireArchiveCapRestore(RETIRE_ARCHIVE_MAX_BYTES_FOR_TEST.with(|cell| cell.replace(Some(cap))))
}

/// Restores the previous archive cap when dropped.
#[cfg(test)]
pub(crate) struct RetireArchiveCapRestore(Option<u64>);

#[cfg(test)]
impl Drop for RetireArchiveCapRestore {
    fn drop(&mut self) {
        RETIRE_ARCHIVE_MAX_BYTES_FOR_TEST.with(|cell| cell.set(self.0));
    }
}

fn retire_archive_max_bytes() -> u64 {
    #[cfg(test)]
    if let Some(cap) = RETIRE_ARCHIVE_MAX_BYTES_FOR_TEST.with(std::cell::Cell::get) {
        return cap;
    }
    RETIRE_ARCHIVE_MAX_BYTES
}

#[cfg(test)]
thread_local! {
    /// Test seam: runs after an operation archive is written, before the
    /// sources are re-checked and the checkout moves.
    pub(super) static BEFORE_OPERATION_MOVE: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn run_before_operation_move() {
    BEFORE_OPERATION_MOVE.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_operation_move() {}

#[cfg(test)]
fn run_before_retire_claim() {
    BEFORE_RETIRE_CLAIM.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_retire_claim() {}

/// How one retirement attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetireOutcome {
    /// Its content is in Git: moved into the trash without an archive.
    RetiredClean,
    /// Archived, verified, then moved into the trash.
    RetiredArchived { bytes: u64 },
    /// A closed session's long-abandoned merge, rebase, cherry-pick, revert
    /// or bisect: its operation state, index and changed files archived and
    /// verified, its commits pinned, then moved into the trash.
    RetiredOperationArchived { bytes: u64 },
    /// Kept for now; the code names the precondition (counter
    /// `checkout_retirement_kept_<code>`).
    Kept(&'static str),
}

impl RetireOutcome {
    fn counter(self) -> &'static str {
        match self {
            Self::RetiredClean => "checkout_retirement_retired_clean",
            Self::RetiredArchived { .. } => "checkout_retirement_retired_archived",
            Self::RetiredOperationArchived { .. } => {
                "checkout_retirement_retired_operation_archived"
            }
            Self::Kept(code) => kept_counter(code),
        }
    }

    fn retired(self) -> bool {
        !matches!(self, Self::Kept(_))
    }
}

/// The phase counter of one keep, by code.
fn kept_counter(code: &'static str) -> &'static str {
    match code {
        "not_closed" => "checkout_retirement_kept_not_closed",
        "adopted" => "checkout_retirement_kept_adopted",
        "checkout_missing" => "checkout_retirement_kept_checkout_missing",
        "not_owned" => "checkout_retirement_kept_not_owned",
        "ownership_unknown" => "checkout_retirement_kept_ownership_unknown",
        "open_session" => "checkout_retirement_kept_open_session",
        "open_session_unknown" => "checkout_retirement_kept_open_session_unknown",
        "lease_held" => "checkout_retirement_kept_lease_held",
        "lease_unknown" => "checkout_retirement_kept_lease_unknown",
        "gate_running" => "checkout_retirement_kept_gate_running",
        "gates_unknown" => "checkout_retirement_kept_gates_unknown",
        "operation_unresolved" => "checkout_retirement_kept_operation_unresolved",
        "operations_unknown" => "checkout_retirement_kept_operations_unknown",
        "operation_in_progress" => "checkout_retirement_kept_operation_in_progress",
        "operation_state_unknown" => "checkout_retirement_kept_operation_state_unknown",
        "process_using_checkout" => "checkout_retirement_kept_process_using_checkout",
        "process_use_unknown" => "checkout_retirement_kept_process_use_unknown",
        "plan_context_unavailable" => "checkout_retirement_kept_plan_context_unavailable",
        "plan_refused" => "checkout_retirement_kept_plan_refused",
        "inspection_failed" => "checkout_retirement_kept_inspection_failed",
        "lifecycle_busy" => "checkout_retirement_kept_lifecycle_busy",
        "changed" => "checkout_retirement_kept_changed",
        "trash_refused" => "checkout_retirement_kept_trash_refused",
        "archive_too_large" => "checkout_retirement_kept_archive_too_large",
        "archive_failed" => "checkout_retirement_kept_archive_failed",
        "archive_cleanup_failed" => "checkout_retirement_kept_archive_cleanup_failed",
        "deadline" => "checkout_retirement_kept_deadline",
        "deferred_to_lane" => "checkout_retirement_kept_deferred_to_lane",
        "archive_deferred" => "checkout_retirement_kept_archive_deferred",
        "operation_archive_failed" => "checkout_retirement_kept_operation_archive_failed",
        "holder_alive" => "checkout_retirement_kept_holder_alive",
        "holder_unknown" => "checkout_retirement_kept_holder_unknown",
        "leftover_superseded" => "checkout_retirement_kept_leftover_superseded",
        _ => "checkout_retirement_kept_other",
    }
}

/// Keeps about the checkout's content or ownership, which only a change to
/// its HEAD or index (or time) lifts: remembered, so the next lap does not
/// plan them again. Every other keep (a process, a lease, a gate, an open
/// session, contention, time) is asked again on the next run.
fn remembered_keep(code: &str) -> bool {
    matches!(
        code,
        "plan_refused"
            | "archive_too_large"
            | "archive_failed"
            | "archive_cleanup_failed"
            | "operation_archive_failed"
            | "operation_in_progress"
            | "not_owned"
            | "adopted"
            | "trash_refused"
    )
}

/// One remembered keep.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RetireKeep {
    /// HEAD and index modification time when it was kept.
    pub key: String,
    pub reason: String,
    pub at_ms: i64,
    pub attempts: u32,
    /// Why, in the words of the step that refused (a plan refusal, an
    /// archive or cleanup error), bounded; shown per session by
    /// `broker gc plan --json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Evidence one retirement run takes once and shares with every session it
/// judges: no per-session re-reading of the live sessions, and one host-wide
/// open-file observation for the whole run.
pub(crate) struct RetireRun {
    open_paths: Vec<(i64, crate::auto_cleanup::ResolvedWorktreePath)>,
    /// The run's open-file observation: `None` until the first checkout
    /// reaches the process-use precondition, then reused for every later
    /// one however old (the deleter re-checks before deleting anything).
    /// An observation that could not be taken completely is the error.
    process_use: Option<Result<std::rc::Rc<crate::reclaim::OpenFileSnapshot>, String>>,
    plan_context: Option<crate::cleanup_resolve::CleanupResolveRetirementContext>,
    deadline: Instant,
    /// The lane's own deadline, which an archive may use past the sweep's.
    archive_deadline: Instant,
    /// Inline retirement at close: only the clean, no-archive path; anything
    /// needing an archive is left to the lane.
    inline: bool,
    /// An operation state older than this (ms) in a closed session's
    /// checkout is abandoned: archived, then the checkout is retired.
    abandoned_operation_after_ms: i64,
    /// The refusing step's own words for the last keep, if it had any.
    keep_detail: Option<String>,
    /// The last inspection this run could not complete (session and the
    /// failing step), retried next run rather than remembered.
    last_inspection_failure: Option<String>,
}

impl RetireRun {
    /// The run's sweep deadline.
    pub(super) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// The keep code when `worktree` overlaps the checkout of an open
    /// session other than `session_id`, or its overlap is unknown.
    pub(super) fn open_session_keep(
        &self,
        session_id: i64,
        worktree: &Path,
    ) -> Option<&'static str> {
        let candidate = crate::auto_cleanup::ResolvedWorktreePath::resolve(worktree);
        for (open_id, open_path) in &self.open_paths {
            if *open_id == session_id {
                continue;
            }
            match crate::auto_cleanup::live_worktree_overlap(&candidate, open_path) {
                crate::auto_cleanup::LiveOverlapEvidence::Disjoint => {}
                crate::auto_cleanup::LiveOverlapEvidence::Overlap => return Some("open_session"),
                crate::auto_cleanup::LiveOverlapEvidence::Unknown => {
                    return Some("open_session_unknown");
                }
            }
        }
        None
    }

    /// The run's open-file observation, taken on first use within
    /// [`RETIRE_PROCESS_USE_BUDGET`] (never past the run's deadline).
    pub(super) fn process_use(
        &mut self,
    ) -> Result<std::rc::Rc<crate::reclaim::OpenFileSnapshot>, String> {
        if self.process_use.is_none() {
            let budget = self
                .deadline
                .saturating_duration_since(Instant::now())
                .min(RETIRE_PROCESS_USE_BUDGET);
            let started = Instant::now();
            let observed = crate::session_holder::retirement_run_process_use_snapshot(budget);
            crate::phase_telemetry::add("checkout_retirement_process_snapshot", started.elapsed());
            self.process_use = Some(match observed {
                crate::reclaim::OpenFileSnapshot::Incomplete { reason, .. } => {
                    crate::phase_telemetry::add(
                        "checkout_retirement_process_snapshot_unavailable",
                        std::time::Duration::ZERO,
                    );
                    Err(reason)
                }
                complete => Ok(std::rc::Rc::new(complete)),
            });
        }
        self.process_use
            .clone()
            .unwrap_or_else(|| Err("process use was not observed".into()))
    }
}

/// What one retirement run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CheckoutRetirementReport {
    pub examined: usize,
    pub retired_clean: usize,
    pub retired_archived: usize,
    /// Checkouts retired through the abandoned-operation archive.
    pub retired_operation_archived: usize,
    pub archive_bytes: u64,
    /// The run's open-file observation could not be taken completely: the
    /// lap stopped at the checkout that needed it, and the run is retried
    /// soon instead of keeping every later checkout as unknown.
    pub process_use_unavailable: bool,
    pub kept: std::collections::BTreeMap<String, usize>,
    /// Kept checkouts not judged again this run (remembered keep, same key).
    pub keep_memo_skips: usize,
    /// Sessions whose checkout is in the trash, recorded as cleaned.
    pub reconciled_trashed: usize,
    pub lap_complete: bool,
    /// The lap over cleaned sessions' leftover folders reached its end.
    pub cleaned_lap_complete: bool,
    /// Of the retired checkouts, leftover folders of sessions already
    /// recorded as cleaned.
    pub retired_cleaned_leftovers: usize,
    pub trash: crate::checkout_trash::TrashDrainReport,
    /// How many times the open sessions' paths were resolved (once per run).
    pub open_path_resolutions: usize,
    /// The strip pass that ran before the judgement lap.
    pub strip: super::strip::CheckoutStripReport,
}

/// Progress kept across runs for `broker status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CheckoutRetirementProgress {
    pub trash_pending: usize,
    pub kept: usize,
    pub stalled_runs: u32,
    pub reported_trash_entries: Vec<String>,
    #[serde(default)]
    pub unrecognized_trash_entries: Vec<String>,
    /// Refs under `refs/aethyme/retired/` still pinning reflog-only commits.
    #[serde(default)]
    pub retired_pins: usize,
    /// Pending trash entries over a day old nothing but `gc apply` confirms.
    #[serde(default)]
    pub stale_pending_trash_entries: Vec<String>,
    /// Closed checkouts retired through the abandoned-operation archive,
    /// across runs.
    #[serde(default)]
    pub abandoned_operations_archived: usize,
    /// The last run could not take its open-file observation.
    #[serde(default)]
    pub process_use_unavailable: bool,
    /// Trash entries the last deleter run left alone because a process
    /// still used them.
    #[serde(default)]
    pub trash_entries_in_use: usize,
    /// The last run's inspection that failed or timed out (session and the
    /// failing Git step), retried on the next run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_inspection_failure: Option<String>,
    /// The last run's strip pass (regenerable directories renamed out of
    /// closed checkouts).
    #[serde(default)]
    pub strip: super::strip::CheckoutStripProgress,
}

/// Whether a plan refusal is an inspection that could not complete (a Git
/// listing that failed or timed out), not a verdict about the checkout.
fn inspection_failed(error: &BrokerOpError) -> bool {
    match error {
        BrokerOpError::CleanupResolveRefused { reason, .. } => {
            reason.contains("is inspection_failed")
                || reason.contains("did not finish within")
                || (reason.contains("git ")
                    && (reason.contains(" timed out after ")
                        || reason.contains(" was not started: the command's deadline")))
        }
        BrokerOpError::Git(crate::git::GitError::TimedOut { .. }) => true,
        _ => false,
    }
}

/// Run Git in `dir`, bounded by the general Git limit, and return stdout.
fn run_git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = crate::git::git_command();
    command.args(args).current_dir(dir);
    match crate::bounded_output::output_limited(
        &mut command,
        crate::git::git_timeout(),
        64 * 1024 * 1024,
    ) {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) => Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(_) => Err(format!("git {}: stopped at its limit", args.join(" "))),
        Err(error) => Err(format!("git {}: {error}", args.join(" "))),
    }
}

/// The key a remembered keep is bound to: the checkout's HEAD and index
/// modification time, read from its files.
fn checkout_keep_key(worktree: &Path) -> Option<String> {
    let git_dir = crate::git::checked_worktree_git_dir(worktree)
        .ok()
        .flatten()?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let index_mtime = std::fs::metadata(git_dir.join("index"))
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    Some(format!("{}:{index_mtime}", head.trim()))
}

impl Broker {
    /// Ask for a retirement run as soon as the worker can take it: a
    /// checkout went into the trash, or a session closed.
    pub(crate) fn request_checkout_retirement_run(&self) {
        crate::warn_unrecorded(
            "request a checkout retirement run",
            self.store_ref().meta_set(
                CHECKOUT_RETIREMENT_DUE_KEY,
                &crate::clock::epoch_ms().to_string(),
            ),
        );
    }

    /// Whether the retirement lane is due at `now_ms`.
    pub(crate) fn checkout_retirement_due(&self, now_ms: i64) -> bool {
        match self.store_ref().meta_get(CHECKOUT_RETIREMENT_DUE_KEY) {
            Ok(Some(value)) => value.parse::<i64>().ok().is_none_or(|due| due <= now_ms),
            Ok(None) => true,
            Err(_) => false,
        }
    }

    pub(super) fn meta_json<T: serde::de::DeserializeOwned + Default>(&self, key: &str) -> T {
        self.store_ref()
            .meta_get(key)
            .ok()
            .flatten()
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default()
    }

    /// Change the JSON value at `key` atomically: read, change, then
    /// compare-and-set, retried when another writer got in between.
    pub(super) fn update_meta_json<T>(
        &self,
        key: &str,
        mut change: impl FnMut(&mut T),
    ) -> Result<(), String>
    where
        T: serde::de::DeserializeOwned + serde::Serialize + Default,
    {
        for _ in 0..32 {
            let current = self
                .store_ref()
                .meta_get(key)
                .map_err(|error| error.to_string())?;
            let mut value = current
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_default();
            change(&mut value);
            let next = serde_json::to_string(&value).map_err(|error| error.to_string())?;
            if self
                .store_ref()
                .meta_compare_and_set(key, current.as_deref(), &next)
                .map_err(|error| error.to_string())?
            {
                return Ok(());
            }
        }
        Err(format!("{key} kept changing under concurrent writers"))
    }

    /// The trash entries this repository recorded.
    pub(crate) fn checkout_trash_rows(&self) -> crate::checkout_trash::TrashRows {
        self.meta_json(CHECKOUT_RETIREMENT_TRASH_ROWS_KEY)
    }

    /// Record a trash entry before the rename (pending).
    pub(crate) fn record_checkout_trash_row(
        &self,
        trash_dir: &Path,
        entry: &str,
        row: crate::checkout_trash::TrashRow,
    ) -> Result<(), String> {
        self.update_meta_json(
            CHECKOUT_RETIREMENT_TRASH_ROWS_KEY,
            |rows: &mut crate::checkout_trash::TrashRows| {
                rows.entry(trash_dir.to_path_buf())
                    .or_default()
                    .insert(entry.to_string(), row);
            },
        )
    }

    /// Confirm a checkout just moved into the trash and ask for a run.
    pub(crate) fn note_checkout_trashed(&self, trashed: &crate::checkout_trash::TrashedCheckout) {
        crate::warn_unrecorded(
            "confirm a checkout trash entry",
            self.record_checkout_trash_row(&trashed.trash_dir, &trashed.entry, trashed.row),
        );
        self.mark_session_in_set(
            CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY,
            trashed.row.session_id,
            true,
        );
        self.request_checkout_retirement_run();
    }

    /// Whether the broker proved it moved `session_id`'s checkout into the
    /// trash: a confirmed row, or the tombstone left after its deletion.
    fn checkout_proven_trashed(&self, session_id: i64) -> bool {
        self.checkout_trash_rows()
            .values()
            .flat_map(|entries| entries.values())
            .any(|row| row.session_id == session_id && !row.pending && !row.stripped)
            || self
                .meta_json::<std::collections::BTreeSet<i64>>(
                    CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY,
                )
                .contains(&session_id)
    }

    /// Record `session_id` as cleaned after its checkout went into the trash;
    /// a failure leaves the tombstone, so the next run reconciles it.
    fn record_trashed_session_cleaned(&mut self, session_id: i64) -> bool {
        match self.mark_removed_checkout_sessions_cleaned(&[session_id], session_id) {
            Ok(()) => {
                self.mark_session_in_set(
                    CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY,
                    session_id,
                    false,
                );
                true
            }
            Err(error) => {
                crate::warn_unrecorded::<(), _>(
                    "record a retired checkout's session as cleaned",
                    Err(error),
                );
                false
            }
        }
    }

    /// Confirm pending trash rows nothing else can confirm (their creator's
    /// start time was unreadable) once they are a day old, when the entry
    /// still has its recorded identity and no open session uses the
    /// session's path: the explicit `gc apply` path. Returns how many.
    pub(crate) fn confirm_stale_pending_trash_rows(&self) -> usize {
        let now = crate::clock::epoch_ms();
        let mut eligible = Vec::new();
        for (dir, entries) in self.checkout_trash_rows() {
            let Some(pool) = dir.parent().map(Path::to_path_buf) else {
                continue;
            };
            for (name, row) in entries {
                if !crate::checkout_trash::stale_unconfirmable(&name, &row, now)
                    || !crate::checkout_trash::entry_matches(&pool, &name, &row)
                {
                    continue;
                }
                let Ok(session) = self.store_ref().session(row.session_id) else {
                    continue;
                };
                let open_elsewhere = self
                    .store_ref()
                    .other_open_session_at_path(row.session_id, &session.worktree_path)
                    .unwrap_or(true);
                if session.cleanup_state == crate::SessionCleanupState::Open || open_elsewhere {
                    continue;
                }
                eligible.push((dir.clone(), name, row));
            }
        }
        if eligible.is_empty() {
            return 0;
        }
        let applied = self.update_meta_json(
            CHECKOUT_RETIREMENT_TRASH_ROWS_KEY,
            |rows: &mut crate::checkout_trash::TrashRows| {
                for (dir, name, row) in &eligible {
                    if let Some(stored) =
                        rows.get_mut(dir).and_then(|entries| entries.get_mut(name))
                        && stored.pending
                        && (stored.device, stored.inode) == (row.device, row.inode)
                    {
                        stored.pending = false;
                    }
                }
            },
        );
        match applied {
            Ok(()) => {
                for (_, _, row) in eligible.iter().filter(|(_, _, row)| !row.stripped) {
                    self.mark_session_in_set(
                        CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY,
                        row.session_id,
                        true,
                    );
                }
                eligible.len()
            }
            Err(error) => {
                crate::warn_unrecorded::<(), _>("confirm stale trash entries", Err(error));
                0
            }
        }
    }

    /// Whether a trash row names `session_id`'s checkout itself (a stripped
    /// regenerable directory is not the checkout).
    fn checkout_in_trash(&self, session_id: i64) -> bool {
        self.checkout_trash_rows()
            .values()
            .flat_map(|entries| entries.values())
            .any(|row| row.session_id == session_id && !row.stripped)
    }

    /// Progress of the last retirement runs.
    pub(crate) fn checkout_retirement_progress(&self) -> CheckoutRetirementProgress {
        self.meta_json(CHECKOUT_RETIREMENT_PROGRESS_KEY)
    }

    /// Sessions whose recovery archive would exceed the cap.
    pub(crate) fn checkout_retirement_too_large(&self) -> std::collections::BTreeSet<i64> {
        self.meta_json(CHECKOUT_RETIREMENT_TOO_LARGE_KEY)
    }

    /// Sessions kept for an operation in progress in their checkout.
    pub(crate) fn checkout_retirement_in_progress(&self) -> std::collections::BTreeSet<i64> {
        self.meta_json(CHECKOUT_RETIREMENT_IN_PROGRESS_KEY)
    }

    fn mark_session_in_set(&self, key: &str, session_id: i64, member: bool) {
        let current: std::collections::BTreeSet<i64> = self.meta_json(key);
        if current.contains(&session_id) == member {
            return;
        }
        crate::warn_unrecorded(
            "remember a retirement keep by session",
            self.update_meta_json(key, |sessions: &mut std::collections::BTreeSet<i64>| {
                if member {
                    sessions.insert(session_id);
                } else {
                    sessions.remove(&session_id);
                }
                while sessions.len() > RETIRE_KEEPS_MAX {
                    sessions.pop_first();
                }
            }),
        );
    }

    /// The remembered keeps.
    pub(crate) fn checkout_retirement_keeps(&self) -> std::collections::BTreeMap<i64, RetireKeep> {
        self.meta_json(CHECKOUT_RETIREMENT_KEEPS_KEY)
    }

    /// The remembered keeps as `broker gc plan --json` shows them.
    pub(crate) fn checkout_retirement_keep_report(
        &self,
    ) -> Vec<crate::retention::GcRetirementKeep> {
        self.checkout_retirement_keeps()
            .into_iter()
            .map(|(session_id, keep)| crate::retention::GcRetirementKeep {
                session_id,
                reason: keep.reason,
                detail: keep.detail,
                kept_at_ms: keep.at_ms,
                attempts: keep.attempts,
            })
            .collect()
    }

    /// Whether the checkout retirement lane owns `session_id`: it kept it on
    /// purpose or moved its checkout into the trash. The old removal lanes
    /// skip such sessions, so old and new never act on one checkout.
    pub(crate) fn checkout_retirement_owns(&self, session_id: i64) -> bool {
        self.checkout_retirement_keeps().contains_key(&session_id)
            || self.checkout_in_trash(session_id)
    }

    /// Whether a remembered keep still holds for `session` (same HEAD and
    /// index, backoff not elapsed).
    fn remembered_keep_holds(&self, session: &Session, now_ms: i64) -> bool {
        let Some(keep) = self.checkout_retirement_keeps().remove(&session.id) else {
            return false;
        };
        let backoff = RETIRE_KEEP_BACKOFF_MS[usize::try_from(keep.attempts.saturating_sub(1))
            .unwrap_or(2)
            .min(2)];
        checkout_keep_key(Path::new(&session.worktree_path)).as_deref() == Some(keep.key.as_str())
            && now_ms.saturating_sub(keep.at_ms) < backoff
    }

    fn remember_keep(&self, session: &Session, reason: &'static str, detail: Option<String>) {
        let key = checkout_keep_key(Path::new(&session.worktree_path));
        let detail = detail.as_deref().map(bounded_detail);
        crate::warn_unrecorded(
            "remember a checkout retirement keep",
            self.update_meta_json(
                CHECKOUT_RETIREMENT_KEEPS_KEY,
                |keeps: &mut std::collections::BTreeMap<i64, RetireKeep>| match key.as_ref() {
                    Some(key) => {
                        let attempts = keeps
                            .get(&session.id)
                            .filter(|keep| &keep.key == key && keep.reason == reason)
                            .map_or(1, |keep| keep.attempts.saturating_add(1));
                        keeps.insert(
                            session.id,
                            RetireKeep {
                                key: key.clone(),
                                reason: reason.to_string(),
                                at_ms: crate::clock::epoch_ms(),
                                attempts,
                                detail: detail.clone(),
                            },
                        );
                        while keeps.len() > RETIRE_KEEPS_MAX {
                            let Some((&oldest, _)) =
                                keeps.iter().min_by_key(|(_, keep)| keep.at_ms)
                            else {
                                break;
                            };
                            keeps.remove(&oldest);
                        }
                    }
                    None => {
                        keeps.remove(&session.id);
                    }
                },
            ),
        );
    }

    fn forget_keep(&self, session_id: i64) {
        if self.checkout_retirement_keeps().contains_key(&session_id) {
            crate::warn_unrecorded(
                "forget a checkout retirement keep",
                self.update_meta_json(
                    CHECKOUT_RETIREMENT_KEEPS_KEY,
                    |keeps: &mut std::collections::BTreeMap<i64, RetireKeep>| {
                        keeps.remove(&session_id);
                    },
                ),
            );
        }
    }

    /// What must hold immediately before a checkout is moved into the trash,
    /// checked under the caller's claims: no merge, rebase, cherry-pick,
    /// revert or bisect in progress and no unmerged index entry (that state
    /// lives in the registration the move removes), unless the retirement
    /// lane just archived exactly that state; no process in the checkout,
    /// by the retirement run's observation when one runs on this thread
    /// (a process arriving after it keeps a now-trashed directory, which the
    /// deleter never deletes while it is in use), otherwise by a fresh one;
    /// and commits only the checkout's reflog reaches are pinned under
    /// `refs/aethyme/retired/<session>/`.
    pub(super) fn prepare_checkout_for_trash(
        &self,
        session: &Session,
        worktree: &Path,
    ) -> Result<(), BrokerOpError> {
        let refuse = |reason: String| BrokerOpError::DirtyWorktree {
            id: session.id,
            reason,
        };
        let operation_archived =
            OPERATION_STATE_ARCHIVED.with(std::cell::Cell::get) == Some(session.id);
        // Inside the retirement lane the checkout was judged before this
        // claim: a `start --reuse` or a new session may have reopened or
        // recreated the path since. Ask again here, under the claim (the
        // holder too where the lane asked it: leftovers of cleaned sessions
        // and abandoned operations).
        if in_retirement_lane() {
            let check_holder =
                operation_archived || session.cleanup_state == crate::SessionCleanupState::Cleaned;
            if let Some(code) = self.reused_path_keep(session, worktree, check_holder) {
                crate::phase_telemetry::add(
                    "checkout_retirement_reused_path_kept",
                    std::time::Duration::ZERO,
                );
                return Err(refuse(format!(
                    "the checkout's path may have been reused since it was judged ({code})"
                )));
            }
        }
        match GitRepo::discover(worktree).and_then(|checkout| checkout.operation_in_progress()) {
            Ok(None) => {}
            Ok(Some(_)) if operation_archived => {}
            Ok(Some(_)) => {
                self.mark_session_in_set(CHECKOUT_RETIREMENT_IN_PROGRESS_KEY, session.id, true);
                return Err(refuse(
                    "a merge, rebase, cherry-pick, revert or bisect is in progress or the index holds unmerged entries".into(),
                ));
            }
            Err(error) => return Err(refuse(format!("cannot read the operation state: {error}"))),
        }
        if let Some(snapshot) = RUN_PROCESS_USE.with(|cell| cell.borrow().clone()) {
            if let Some(reason) =
                crate::reclaim::open_file_snapshot_blocker_for_worktree(&snapshot, worktree)
            {
                return Err(refuse(format!("a process uses the checkout: {reason}")));
            }
            crate::phase_telemetry::add(
                "checkout_trash_run_process_use_reused",
                std::time::Duration::ZERO,
            );
            return self
                .pin_reflog_only_commits(session.id, worktree)
                .map_err(|reason| refuse(format!("reflog commits could not be pinned: {reason}")));
        }
        let mut fresh = crate::session_holder::RecordedHolderProcessTable::default();
        match fresh.process_use_snapshot_for_retirement(
            Path::new("/"),
            Some(Instant::now() + RETIRE_PROCESS_USE_BUDGET),
        ) {
            Some(snapshot) => {
                if let Some(reason) =
                    crate::reclaim::open_file_snapshot_blocker_for_worktree(snapshot, worktree)
                {
                    return Err(refuse(format!("a process uses the checkout: {reason}")));
                }
            }
            None => return Err(refuse("process use of the checkout is unknown".into())),
        }
        self.pin_reflog_only_commits(session.id, worktree)
            .map_err(|reason| refuse(format!("reflog commits could not be pinned: {reason}")))
    }

    /// Pin, as `refs/aethyme/retired/<session_id>/<commit>`, every commit
    /// only `worktree` itself refers to, so dropping its registration (its
    /// HEAD reflog, its per-worktree refs, its bisect state) loses none: its
    /// current `HEAD` and `ORIG_HEAD`, the commits its HEAD reflog names
    /// (old and new ids; a missing reflog is recorded), the commits its
    /// per-worktree refs (`refs/bisect/*`, `refs/worktree/*`,
    /// `refs/rewritten/*`) point at, and those `BISECT_LOG`/`BISECT_START`
    /// name, each unless already reachable from the default branch, a
    /// remote-tracking ref, an existing pin, or (on the archive path) a
    /// commit the verified archive's bundle carries. Reachability is decided in one
    /// `rev-list` for at most [`RETIRE_PIN_CANDIDATES_MAX`] candidates; more
    /// refuses (the caller keeps the checkout). Pins expire by the rule of
    /// [`Self::prune_retired_pins`].
    fn pin_reflog_only_commits(&self, session_id: i64, worktree: &Path) -> Result<(), String> {
        let _span = crate::phase_telemetry::span("checkout_trash_worktree_commit_pins");
        let git_dir = crate::git::checked_worktree_git_dir(worktree)
            .map_err(|error| format!("cannot find the checkout's Git directory: {error}"))?
            .ok_or("the checkout has no linked Git directory")?;
        let mut candidates = std::collections::BTreeSet::new();
        match std::fs::read_to_string(git_dir.join("logs/HEAD")) {
            Ok(reflog) => {
                for line in reflog.lines() {
                    candidates.extend(line.split_whitespace().take(2).map(str::to_string));
                }
            }
            Err(error) => {
                // No reflog (logAllRefUpdates off, expired, removed): HEAD
                // and ORIG_HEAD below still count; say so instead of
                // silently finding nothing.
                crate::phase_telemetry::add(
                    "checkout_trash_worktree_reflog_missing",
                    std::time::Duration::ZERO,
                );
                crate::phase_telemetry::attach(
                    "checkout_trash_worktree_reflog",
                    serde_json::json!({
                        "missing_session_id": session_id,
                        "error": error.kind().to_string(),
                    }),
                );
            }
        }
        // The worktree's current HEAD and ORIG_HEAD, whatever its reflog
        // says: a detached HEAD's commit is otherwise named nowhere else.
        for name in ["HEAD", "ORIG_HEAD"] {
            if let Ok(commit) = run_git_text(
                worktree,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{name}^{{commit}}"),
                ],
            ) {
                candidates.insert(commit.trim().to_string());
            }
        }
        let per_worktree_refs = run_git_text(
            worktree,
            &[
                "for-each-ref",
                "--format=%(objectname)",
                "refs/bisect/",
                "refs/worktree/",
                "refs/rewritten/",
            ],
        )?;
        candidates.extend(
            per_worktree_refs
                .lines()
                .map(|line| line.trim().to_string()),
        );
        for name in ["BISECT_LOG", "BISECT_START"] {
            if let Ok(text) = std::fs::read_to_string(git_dir.join(name)) {
                candidates.extend(text.split_whitespace().map(str::to_string));
            }
        }
        let candidates = candidates
            .into_iter()
            .filter(|id| {
                matches!(id.len(), 40 | 64)
                    && id.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && id.bytes().any(|byte| byte != b'0')
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(());
        }
        if candidates.len() > RETIRE_PIN_CANDIDATES_MAX {
            return Err(format!(
                "the checkout's reflog, per-worktree refs and bisect state name {} commits, more than the {RETIRE_PIN_CANDIDATES_MAX} checked per checkout; it is kept",
                candidates.len()
            ));
        }
        crate::phase_telemetry::add(
            "checkout_trash_worktree_commit_candidates",
            std::time::Duration::ZERO,
        );
        let scratch = tempfile::NamedTempFile::new()
            .map_err(|error| format!("cannot create a scratch file: {error}"))?;
        std::fs::write(scratch.path(), format!("{}\n", candidates.join("\n")))
            .map_err(|error| format!("cannot write a scratch file: {error}"))?;
        // Only commits that still exist: an old reflog id may be gone.
        let typed = String::from_utf8_lossy(&super::retire_operation::git_bytes_with_input(
            self.main_root(),
            &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
            scratch.path(),
        )?)
        .into_owned();
        let commits = typed
            .lines()
            .filter_map(|line| line.strip_suffix(" commit"))
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        if commits.is_empty() {
            return Ok(());
        }
        // What already keeps a commit: the default branch, remote-tracking
        // refs and existing pins (the rule pins expire by).
        let mut kept_by = vec![
            "refs/remotes/".to_string(),
            "refs/aethyme/retired/".to_string(),
        ];
        if let Some((upstream_ref, _)) = self.repo_handle().upstream_default()
            && let Some((_, branch)) = upstream_ref
                .trim_start_matches("refs/remotes/")
                .split_once('/')
        {
            kept_by.push(format!("refs/heads/{branch}"));
        }
        let mut args = vec!["for-each-ref", "--format=%(objectname)"];
        args.extend(kept_by.iter().map(String::as_str));
        let excluded = run_git_text(self.main_root(), &args)?;
        let mut input = commits
            .iter()
            .map(|commit| format!("{commit}\n"))
            .collect::<String>();
        // A verified archive's bundle also keeps what it carries.
        let archived = ARCHIVED_COMMITS.with(|cell| cell.borrow().clone());
        for id in excluded
            .lines()
            .map(str::trim)
            .chain(archived.iter().map(String::as_str))
            .filter(|id| {
                matches!(id.len(), 40 | 64) && id.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            input.push_str(&format!("^{id}\n"));
        }
        std::fs::write(scratch.path(), input)
            .map_err(|error| format!("cannot write a scratch file: {error}"))?;
        let unreachable = String::from_utf8_lossy(&super::retire_operation::git_bytes_with_input(
            self.main_root(),
            &["rev-list", "--stdin"],
            scratch.path(),
        )?)
        .lines()
        .map(str::to_string)
        .filter(|commit| commits.contains(commit))
        .collect::<Vec<_>>();
        if unreachable.is_empty() {
            return Ok(());
        }
        let updates = unreachable
            .iter()
            .map(|commit| format!("update refs/aethyme/retired/{session_id}/{commit} {commit}\n"))
            .collect::<String>();
        std::fs::write(scratch.path(), updates)
            .map_err(|error| format!("cannot write a scratch file: {error}"))?;
        super::retire_operation::git_bytes_with_input(
            self.main_root(),
            &["update-ref", "--stdin"],
            scratch.path(),
        )?;
        for _ in &unreachable {
            crate::phase_telemetry::add(
                "checkout_trash_reflog_commits_pinned",
                std::time::Duration::ZERO,
            );
        }
        Ok(())
    }

    /// Expire `refs/aethyme/retired/<session>/<commit>` pins: a pin goes once
    /// its commit is contained in a delivery target (the default branch or
    /// its remote-tracking ref) or another remote-tracking ref, which a
    /// session branch or scratch ref deleted later cannot undo; or once its
    /// session has been cleaned for `recovery_archive_days` (0 keeps pins
    /// forever). Never otherwise. At most `RETIRED_PIN_CHECKS_PER_RUN` pins
    /// are checked per run, from a cursor. Returns the pins left.
    pub(crate) fn prune_retired_pins(&self) -> Result<usize, String> {
        self.prune_retired_pins_until(None)
    }

    /// [`Self::prune_retired_pins`], stopping at `deadline` (the cursor
    /// keeps the place). Cheapest evidence first: the session's age (a
    /// database read), then ancestry of the default branch's tip (one
    /// `merge-base`), and only then the scan of every remote-tracking ref,
    /// which on the busiest repository (4,466 remote refs) made this step
    /// 140 to 457 s of each retirement run.
    pub(crate) fn prune_retired_pins_until(
        &self,
        deadline: Option<Instant>,
    ) -> Result<usize, String> {
        let root = self.main_root().to_path_buf();
        let listing = run_git_text(
            &root,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/aethyme/retired/",
            ],
        )?;
        let pins = listing
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(refname, commit)| (refname.to_string(), commit.to_string()))
            .collect::<Vec<_>>();
        if pins.is_empty() {
            return Ok(0);
        }
        let mut targets = vec!["refs/remotes/".to_string()];
        let upstream = self.repo_handle().upstream_default();
        if let Some((upstream_ref, _)) = upstream.as_ref()
            && let Some((_, branch)) = upstream_ref
                .trim_start_matches("refs/remotes/")
                .split_once('/')
        {
            targets.push(format!("refs/heads/{branch}"));
        }
        let days = crate::load_retention_policy(&root)
            .map(|policy| policy.recovery_archive_days)
            .unwrap_or(30);
        let now = crate::clock::epoch_ms();
        let cursor = self
            .store_ref()
            .meta_get(CHECKOUT_RETIREMENT_PIN_CURSOR_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        let start = pins
            .iter()
            .position(|(refname, _)| refname.as_str() > cursor.as_str())
            .unwrap_or(0);
        let mut removed = 0;
        let mut last = cursor.clone();
        for (refname, commit) in pins
            .iter()
            .cycle()
            .skip(start)
            .take(RETIRED_PIN_CHECKS_PER_RUN.min(pins.len()))
        {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                crate::phase_telemetry::add(
                    "checkout_retirement_pins_stopped_deadline",
                    std::time::Duration::ZERO,
                );
                break;
            }
            last.clone_from(refname);
            let session_cleaned_long_ago = days > 0
                && refname
                    .strip_prefix("refs/aethyme/retired/")
                    .and_then(|rest| rest.split('/').next())
                    .and_then(|id| id.parse::<i64>().ok())
                    .and_then(|id| self.store_ref().session(id).ok())
                    .is_some_and(|session| {
                        session.cleanup_state == crate::SessionCleanupState::Cleaned
                            && session.cleanup_completed_at.is_some_and(|cleaned| {
                                now.saturating_sub(cleaned) > i64::from(days) * 86_400_000
                            })
                    });
            let contained = session_cleaned_long_ago
                || upstream.as_ref().is_some_and(|(_, upstream_tip)| {
                    matches!(
                        self.repo_handle()
                            .is_ancestor_checked(commit.as_str(), upstream_tip.as_str()),
                        Ok(true)
                    )
                })
                || {
                    crate::phase_telemetry::add(
                        "checkout_retirement_pins_remote_scan",
                        std::time::Duration::ZERO,
                    );
                    let mut args = vec![
                        "for-each-ref",
                        "--count=1",
                        "--contains",
                        commit.as_str(),
                        "--format=%(refname)",
                    ];
                    args.extend(targets.iter().map(String::as_str));
                    !run_git_text(&root, &args)?.trim().is_empty()
                };
            if contained || session_cleaned_long_ago {
                run_git_text(&root, &["update-ref", "-d", refname, commit])?;
                removed += 1;
                crate::phase_telemetry::add(
                    "checkout_retirement_pins_expired",
                    std::time::Duration::ZERO,
                );
            }
        }
        crate::warn_unrecorded(
            "advance the retired-pin cursor",
            self.store_ref()
                .meta_set(CHECKOUT_RETIREMENT_PIN_CURSOR_KEY, &last),
        );
        Ok(pins.len() - removed)
    }

    /// Take the evidence one retirement run shares: the open sessions'
    /// checkout paths, resolved once. `None` when they could not be resolved
    /// before `deadline`.
    pub(crate) fn begin_checkout_retirement_run(
        &self,
        deadline: Instant,
    ) -> Result<Option<RetireRun>, BrokerOpError> {
        let open = self
            .store_ref()
            .live_sessions()?
            .into_iter()
            .filter(|session| session.cleanup_state == crate::SessionCleanupState::Open)
            .collect::<Vec<_>>();
        let _span = crate::phase_telemetry::span("checkout_retirement_open_paths");
        let Some(paths) = resolve_live_paths_for_lane(&open, Some(deadline)) else {
            return Ok(None);
        };
        Ok(Some(RetireRun {
            open_paths: open.iter().map(|session| session.id).zip(paths).collect(),
            process_use: None,
            plan_context: None,
            deadline,
            archive_deadline: deadline,
            inline: false,
            abandoned_operation_after_ms: crate::auto_cleanup::abandoned_operation_after_ms(
                self.main_root(),
            ),
            keep_detail: None,
            last_inspection_failure: None,
        }))
    }

    /// Retire the checkout of the closed session `session_id`, judged with
    /// the evidence of `run`. See the module documentation for the rules.
    pub(crate) fn retire_closed_checkout(
        &mut self,
        session_id: i64,
        run: &mut RetireRun,
    ) -> Result<RetireOutcome, BrokerOpError> {
        let _span = crate::phase_telemetry::span("checkout_retirement_judge");
        let _lane = RetirementLaneScope::enter();
        run.keep_detail = None;
        let outcome = self.retire_closed_checkout_inner(session_id, run)?;
        crate::phase_telemetry::add(outcome.counter(), std::time::Duration::ZERO);
        match outcome {
            RetireOutcome::Kept(code) if remembered_keep(code) => {
                if let Ok(session) = self.store_ref().session(session_id) {
                    self.remember_keep(&session, code, run.keep_detail.take());
                }
            }
            RetireOutcome::Kept(_) => {}
            RetireOutcome::RetiredClean
            | RetireOutcome::RetiredArchived { .. }
            | RetireOutcome::RetiredOperationArchived { .. } => {
                self.forget_keep(session_id);
                self.mark_session_in_set(CHECKOUT_RETIREMENT_TOO_LARGE_KEY, session_id, false);
                self.mark_session_in_set(CHECKOUT_RETIREMENT_IN_PROGRESS_KEY, session_id, false);
            }
        }
        Ok(outcome)
    }

    /// Retire `session_id`'s checkout right after an automatic close, when
    /// the caller has time left (a budgeted maintenance pass). Anything the
    /// inline attempt keeps is retried by the lane, which a close requests
    /// anyway.
    pub(crate) fn retire_inline_after_close(&mut self, session_id: i64, deadline: Option<Instant>) {
        let Some(deadline) = deadline else {
            return;
        };
        if deadline.saturating_duration_since(Instant::now()) < RETIRE_INLINE_MIN_BUDGET {
            return;
        }
        let inline_deadline = deadline.min(Instant::now() + RETIRE_INLINE_BUDGET);
        let outcome = self
            .begin_checkout_retirement_run(inline_deadline)
            .and_then(|run| match run {
                Some(mut run) => {
                    run.inline = true;
                    self.retire_closed_checkout(session_id, &mut run).map(Some)
                }
                None => Ok(None),
            });
        match outcome {
            Ok(Some(outcome)) if outcome.retired() => {
                crate::phase_telemetry::add(
                    "checkout_retirement_inline_retired",
                    std::time::Duration::ZERO,
                );
            }
            Ok(_) => {}
            Err(error) => {
                crate::warn_unrecorded::<(), _>("retire a checkout at close", Err(error));
            }
        }
    }

    fn retire_closed_checkout_inner(
        &mut self,
        session_id: i64,
        run: &mut RetireRun,
    ) -> Result<RetireOutcome, BrokerOpError> {
        use RetireOutcome::Kept;
        if Instant::now() >= run.deadline {
            return Ok(Kept("deadline"));
        }
        let git_deadline = crate::git::limit_git_until(run.deadline);
        let session = self.store_ref().session(session_id)?;
        let leftover = session.cleanup_state == crate::SessionCleanupState::Cleaned;
        if !session.status.is_closed()
            || !matches!(
                session.cleanup_state,
                crate::SessionCleanupState::Closed | crate::SessionCleanupState::Cleaned
            )
        {
            return Ok(Kept("not_closed"));
        }
        if session.origin != crate::SessionOrigin::Spawned {
            return Ok(Kept("adopted"));
        }
        let worktree = PathBuf::from(&session.worktree_path);
        match std::fs::symlink_metadata(&worktree) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => return Ok(Kept("checkout_missing")),
        }
        // Judged only through the row that owns the folder at its path
        // (`path_superseded_by`): a cleaned session's leftover only when
        // every row recorded there is cleaned, a closed session's checkout
        // only when no newer row there is closed. Asked before the plan, so
        // a superseded row never pays for one.
        match self.path_superseded(&session) {
            Ok(false) => {}
            Ok(true) => return Ok(Kept("leftover_superseded")),
            Err(_) => return Ok(Kept("open_session_unknown")),
        }
        if leftover {
            // A folder left behind by a session already recorded as
            // cleaned: only once that session's recorded holder is gone or
            // was never bound.
            let mut holders = crate::session_holder::RecordedHolderProcessTable::default();
            match crate::session_holder::recorded_holder_liveness(
                self.store_ref(),
                session.id,
                &mut holders,
                true,
            )
            .liveness
            {
                crate::session_holder::RecordedHolderLiveness::Unbound
                | crate::session_holder::RecordedHolderLiveness::Gone => {}
                crate::session_holder::RecordedHolderLiveness::Alive => {
                    return Ok(Kept("holder_alive"));
                }
                crate::session_holder::RecordedHolderLiveness::Unknown => {
                    return Ok(Kept("holder_unknown"));
                }
            }
        }
        {
            let _phase = crate::phase_telemetry::span("checkout_retirement_phase_ownership");
            match self.is_broker_owned_worktree_checked(&session, &worktree) {
                Ok(true) => {}
                Ok(false) => return Ok(Kept("not_owned")),
                Err(_) => return Ok(Kept("ownership_unknown")),
            }
        }
        let sessions_phase = crate::phase_telemetry::span("checkout_retirement_phase_sessions");
        if let Some(code) = run.open_session_keep(session_id, &worktree) {
            return Ok(Kept(code));
        }
        match self
            .store_ref()
            .unreleased_unexpired_leases_for_sessions_at(&[session_id], crate::clock::epoch_ms())
        {
            Ok(leases) if leases.is_empty() => {}
            Ok(_) => return Ok(Kept("lease_held")),
            Err(_) => return Ok(Kept("lease_unknown")),
        }
        let deadline = run.deadline;
        match crate::gates::checked_running_session_gates_until(
            self.main_root(),
            session_id,
            || Instant::now() >= deadline,
            256,
        ) {
            Some(gates) if gates.is_empty() => {}
            Some(_) => return Ok(Kept("gate_running")),
            None => return Ok(Kept("gates_unknown")),
        }
        match self.session_operations_unresolved_since_boot(session_id) {
            Ok(false) => {}
            Ok(true) => return Ok(Kept("operation_unresolved")),
            Err(_) => return Ok(Kept("operations_unknown")),
        }
        drop(sessions_phase);
        // A merge, rebase, cherry-pick, revert or bisect in progress, or
        // unmerged index entries: that state lives in the registration and
        // is neither archived nor in Git. A young one keeps the checkout
        // (asked again under the claim); one older than the abandonment
        // threshold in this closed session is archived below, state and all.
        let abandoned_operation = {
            let _phase = crate::phase_telemetry::span("checkout_retirement_phase_operation_state");
            match GitRepo::discover(&worktree).and_then(|checkout| checkout.operation_in_progress())
            {
                Ok(None) => false,
                Ok(Some(_)) => {
                    // Abandoned only when nothing touched the operation (its
                    // markers, index, reflog or any changed working file)
                    // and the broker saw no activity for the session within
                    // the threshold.
                    let now_ms = crate::clock::epoch_ms();
                    let last_activity_ms =
                        session.last_activity_at.max(session.closed_at.unwrap_or(0));
                    let abandoned = !run.inline
                        && now_ms.saturating_sub(last_activity_ms)
                            >= run.abandoned_operation_after_ms
                        && super::retire_operation::operation_state_age_ms(&worktree)
                            .is_some_and(|age| age >= run.abandoned_operation_after_ms);
                    if !abandoned {
                        self.mark_session_in_set(
                            CHECKOUT_RETIREMENT_IN_PROGRESS_KEY,
                            session_id,
                            true,
                        );
                        return Ok(Kept("operation_in_progress"));
                    }
                    true
                }
                Err(_) => return Ok(Kept("operation_state_unknown")),
            }
        };
        // One observation for the whole run. Could not look is not a
        // verdict about this checkout: the caller stops the lap here and
        // retries soon instead of keeping every later checkout as unknown.
        let run_process_use = {
            let _phase = crate::phase_telemetry::span("checkout_retirement_phase_process_use");
            match run.process_use() {
                Ok(snapshot) => snapshot,
                Err(reason) => {
                    run.keep_detail = Some(reason);
                    return Ok(Kept("process_use_unknown"));
                }
            }
        };
        if crate::reclaim::open_file_snapshot_blocker_for_worktree(&run_process_use, &worktree)
            .is_some()
        {
            return Ok(Kept("process_using_checkout"));
        }
        let _run_process_use = RunProcessUseScope::enter(run_process_use);
        if abandoned_operation {
            if run
                .archive_deadline
                .saturating_duration_since(Instant::now())
                < RETIRE_ARCHIVE_MIN_BUDGET
            {
                return Ok(Kept("archive_deferred"));
            }
            drop(git_deadline);
            let _archive_git_deadline = crate::git::limit_git_until(run.archive_deadline);
            let _phase =
                crate::phase_telemetry::span("checkout_retirement_phase_operation_archive");
            return self.retire_abandoned_operation(&session, &worktree, run);
        }
        let plan = {
            let _phase = crate::phase_telemetry::span("checkout_retirement_phase_plan");
            if run.plan_context.is_none() {
                match self.cleanup_resolve_retirement_context_until(Some(run.deadline)) {
                    Ok(context) => run.plan_context = Some(context),
                    Err(error) => {
                        run.keep_detail = Some(error.to_string());
                        return Ok(Kept("plan_context_unavailable"));
                    }
                }
            }
            let Some(context) = run.plan_context.as_ref() else {
                return Ok(Kept("plan_context_unavailable"));
            };
            match self.cleanup_resolve_retirement_plan_with_context(session_id, false, context) {
                Ok(plan) => plan,
                Err(error) => {
                    let detail = error.to_string();
                    run.keep_detail = Some(detail.clone());
                    // An inspection that failed or ran out of time is not a
                    // verdict about the checkout: retried next run, never
                    // remembered as a refusal for an hour. The detail names
                    // the Git step that failed.
                    if inspection_failed(&error) || Instant::now() >= run.deadline {
                        crate::phase_telemetry::add(
                            "checkout_retirement_inspection_retried",
                            std::time::Duration::ZERO,
                        );
                        run.last_inspection_failure =
                            Some(format!("session {session_id}: {}", bounded_detail(&detail)));
                        return Ok(Kept("inspection_failed"));
                    }
                    return Ok(Kept("plan_refused"));
                }
            }
        };
        let preserves = !plan.unlanded_commits.is_empty()
            || plan.staged_patch_bytes > 0
            || plan.unstaged_patch_bytes > 0
            || !plan.untracked_files.is_empty()
            || !plan.ignored_files.is_empty();
        if preserves && run.inline {
            // The archive writer is not bounded by a close's pass.
            return Ok(Kept("deferred_to_lane"));
        }
        run_before_retire_claim();
        if !preserves {
            // Everything is in Git: no archive. Re-check under the
            // lifecycle claim that nothing changed (ignored files included),
            // then move it away. The cleanup request's grace hours are
            // ignored on purpose: the work is in Git, so the directory gets
            // no grace.
            let Some(_claim) = self.try_claim_worktree_lifecycle("retire")? else {
                return Ok(Kept("lifecycle_busy"));
            };
            let current = self.store_ref().session(session_id)?;
            if !current.status.is_closed()
                || current.cleanup_state != session.cleanup_state
                || current.worktree_path != session.worktree_path
            {
                return Ok(Kept("changed"));
            }
            let head = GitRepo::discover(&worktree)
                .ok()
                .and_then(|checkout| checkout.head_commit().ok());
            if head.as_deref() != Some(plan.head.as_str()) {
                return Ok(Kept("changed"));
            }
            let clean = crate::auto_cleanup::AutoCleanupPolicy::load(self.main_root())
                .ok()
                .and_then(|policy| {
                    crate::auto_cleanup::unclean_reason(
                        &worktree,
                        Some(current.branch.as_str()),
                        self.main_root(),
                        &policy,
                    )
                    .ok()
                })
                .is_some_and(|reason| reason.is_none());
            if !clean {
                return Ok(Kept("changed"));
            }
            let moved = {
                let _phase = crate::phase_telemetry::span("checkout_retirement_phase_move");
                self.remove_checkout_through_trash(&current, &worktree, true)
            };
            match moved {
                Ok(()) if !worktree.exists() => {}
                Ok(()) => return Ok(Kept("trash_refused")),
                Err(error) => {
                    run.keep_detail = Some(error.to_string());
                    let in_progress = GitRepo::discover(&worktree)
                        .and_then(|checkout| checkout.operation_in_progress())
                        .is_ok_and(|state| state.is_some());
                    return Ok(Kept(if in_progress {
                        "operation_in_progress"
                    } else {
                        "trash_refused"
                    }));
                }
            }
            // The checkout is in the trash either way; a failed record is
            // reconciled by the next run from the tombstone.
            self.record_trashed_session_cleaned(session_id);
            return Ok(RetireOutcome::RetiredClean);
        }
        let bytes = plan
            .staged_patch_bytes
            .saturating_add(plan.unstaged_patch_bytes)
            .saturating_add(plan.untracked_bytes)
            .saturating_add(plan.ignored_bytes);
        if bytes > retire_archive_max_bytes() {
            self.mark_session_in_set(CHECKOUT_RETIREMENT_TOO_LARGE_KEY, session_id, true);
            return Ok(Kept("archive_too_large"));
        }
        // Too little of the lane's time left for an archive: the lap stops
        // here and the next run (a minute later) starts with this checkout,
        // instead of an archive that a deadline cuts off after it was
        // written, every run.
        if run
            .archive_deadline
            .saturating_duration_since(Instant::now())
            < RETIRE_ARCHIVE_MIN_BUDGET
        {
            return Ok(Kept("archive_deferred"));
        }
        let digest = plan.digest.clone();
        drop(git_deadline);
        let _archive_git_deadline = crate::git::limit_git_until(run.archive_deadline);
        let _phase = crate::phase_telemetry::span("checkout_retirement_phase_archive");
        // The archive writer writes and reads back the archive, re-plans
        // under the lifecycle claim, and only then moves the checkout into
        // the trash (with the checks of `prepare_checkout_for_trash`); any
        // failure keeps the checkout and the archive. A failure the lane's
        // deadline caused is retried next run (the verified archive is
        // reused), not remembered for an hour.
        let applied = self.cleanup_resolve_retirement_apply(session_id, &digest, false);
        let out_of_time = Instant::now() >= run.archive_deadline;
        match applied {
            Ok(outcome) if outcome.worktree_removed => Ok(RetireOutcome::RetiredArchived { bytes }),
            Ok(outcome) => {
                run.keep_detail = outcome.cleanup_error;
                Ok(Kept(if out_of_time {
                    "deadline"
                } else {
                    "archive_cleanup_failed"
                }))
            }
            Err(error) => {
                run.keep_detail = Some(error.to_string());
                Ok(Kept(if out_of_time {
                    "deadline"
                } else {
                    "archive_failed"
                }))
            }
        }
    }

    /// Retire the checkout of a closed session whose merge, rebase,
    /// cherry-pick, revert or bisect was abandoned: archive the operation
    /// state, the raw index, every changed or untracked file and the
    /// protected ignored files, verified by reading them back; pin every
    /// commit the state names; then move the checkout into the trash under
    /// the lifecycle claim. Any failure keeps the checkout.
    fn retire_abandoned_operation(
        &mut self,
        session: &Session,
        worktree: &Path,
        run: &mut RetireRun,
    ) -> Result<RetireOutcome, BrokerOpError> {
        use RetireOutcome::Kept;
        // No live holder may be bound to the session: an agent that still
        // holds it may come back to the operation.
        let mut holders = crate::session_holder::RecordedHolderProcessTable::default();
        match crate::session_holder::recorded_holder_liveness(
            self.store_ref(),
            session.id,
            &mut holders,
            true,
        )
        .liveness
        {
            crate::session_holder::RecordedHolderLiveness::Unbound
            | crate::session_holder::RecordedHolderLiveness::Gone => {}
            crate::session_holder::RecordedHolderLiveness::Alive => {
                return Ok(Kept("holder_alive"));
            }
            crate::session_holder::RecordedHolderLiveness::Unknown => {
                return Ok(Kept("holder_unknown"));
            }
        }
        run_before_retire_claim();
        let Some(_claim) = self.try_claim_worktree_lifecycle("retire")? else {
            return Ok(Kept("lifecycle_busy"));
        };
        let current = self.store_ref().session(session.id)?;
        if !current.status.is_closed()
            || current.cleanup_state != session.cleanup_state
            || current.worktree_path != session.worktree_path
        {
            return Ok(Kept("changed"));
        }
        let policy = match crate::auto_cleanup::AutoCleanupPolicy::load(self.main_root()) {
            Ok(policy) => policy,
            Err(error) => {
                run.keep_detail = Some(error);
                return Ok(Kept("operation_archive_failed"));
            }
        };
        let archive_root = self.recovery_archive_root()?;
        let archived = super::retire_operation::write_operation_archive(
            &super::retire_operation::OperationArchiveRequest {
                session_id: session.id,
                branch: &current.branch,
                task: current.task.as_deref(),
                worktree,
                archive_root: &archive_root,
                policy: &policy,
                max_bytes: retire_archive_max_bytes(),
                deadline: run.archive_deadline,
            },
        );
        let archived = match archived {
            Ok(archived) => archived,
            Err(super::retire_operation::OperationArchiveError::TooLarge(reason)) => {
                run.keep_detail = Some(reason);
                self.mark_session_in_set(CHECKOUT_RETIREMENT_TOO_LARGE_KEY, session.id, true);
                return Ok(Kept("archive_too_large"));
            }
            Err(super::retire_operation::OperationArchiveError::Refused(reason)) => {
                run.keep_detail = Some(reason);
                return Ok(Kept(if Instant::now() >= run.archive_deadline {
                    "deadline"
                } else {
                    "operation_archive_failed"
                }));
            }
        };
        for commit in &archived.commits {
            if let Err(reason) = run_git_text(
                self.main_root(),
                &[
                    "update-ref",
                    &format!("refs/aethyme/retired/{}/{commit}", session.id),
                    commit,
                ],
            ) {
                run.keep_detail = Some(format!(
                    "the operation's commits could not be pinned (archive {} kept): {reason}",
                    archived.path.display()
                ));
                return Ok(Kept("operation_archive_failed"));
            }
        }
        run_before_operation_move();
        // Someone who worked in the checkout after the copy keeps it: an
        // edit that reached only the trash would be deleted with it.
        if let Err(reason) = super::retire_operation::sources_unchanged(&archived, worktree) {
            run.keep_detail = Some(format!(
                "{reason} (archive {} kept)",
                archived.path.display()
            ));
            self.forget_keep(session.id);
            return Ok(Kept("changed"));
        }
        let moved = {
            let _phase = crate::phase_telemetry::span("checkout_retirement_phase_move");
            let _archived_state = OperationStateArchivedScope::enter(session.id);
            self.remove_checkout_through_trash(&current, worktree, true)
        };
        match moved {
            Ok(()) if !worktree.exists() => {}
            Ok(()) => return Ok(Kept("trash_refused")),
            Err(error) => {
                run.keep_detail = Some(format!(
                    "{error} (archive {} kept)",
                    archived.path.display()
                ));
                return Ok(Kept("trash_refused"));
            }
        }
        crate::phase_telemetry::add(
            "checkout_retirement_abandoned_operation_archived",
            std::time::Duration::ZERO,
        );
        self.record_trashed_session_cleaned(session.id);
        Ok(RetireOutcome::RetiredOperationArchived {
            bytes: archived.bytes,
        })
    }

    /// The worker lane: judge closed sessions that still have a checkout,
    /// by session-id cursor, until `deadline` (two thirds of the time), then
    /// drain the trash with the rest.
    pub(crate) fn run_checkout_retirement(
        &mut self,
        deadline: Instant,
    ) -> Result<CheckoutRetirementReport, BrokerOpError> {
        let _span = crate::phase_telemetry::span("checkout_retirement_run");
        let _lane = RetirementLaneScope::enter();
        // One `git worktree list` for the whole run, not one per checkout.
        let _inventory = crate::git::WorktreeInventoryRunScope::enter();
        let started = Instant::now();
        let window = deadline.saturating_duration_since(started);
        let sweep_deadline = started + window * 2 / 3;
        let mut report = CheckoutRetirementReport::default();
        let mut last_inspection_failure = None;
        // Resolving the open sessions' paths may use a third of the window,
        // never the phases after it.
        if let Some(mut run) = self.begin_checkout_retirement_run(started + window / 3)? {
            run.archive_deadline = deadline;
            report.open_path_resolutions = 1;
            #[cfg(test)]
            AFTER_OPEN_PATHS.with(|hook| {
                if let Some(hook) = hook.borrow_mut().as_mut() {
                    hook();
                }
            });
            // Each phase gets a share of what is left of the sweep window,
            // never less than its floor (within the lane's own deadline), so
            // a slow phase before it cannot leave it nothing: on the busiest
            // repository 0.8.122's laps got 14 ms and 4 ms after the open
            // paths and the strip pass used the window.
            let share = |part: u32, of: u32, floor: std::time::Duration| {
                let left = sweep_deadline.saturating_duration_since(Instant::now());
                (Instant::now() + (left * part / of).max(floor)).min(deadline)
            };
            // Strip regenerable directories out of closed checkouts first:
            // O(1) renames, no walk, capped per run, so the judgement laps
            // that follow see small trees.
            let strip_deadline = share(2, 5, RETIRE_STRIP_MIN_SHARE);
            run.deadline = strip_deadline;
            report.strip = self.strip_closed_checkouts(
                &mut run,
                strip_deadline,
                super::strip::STRIP_CHECKOUTS_PER_RUN,
            );
            // Closed sessions first, then the leftover folders of sessions
            // already recorded as cleaned, each lap with its own cursor and
            // its own share so neither starves the other.
            let closed_deadline = share(1, 2, RETIRE_LAP_MIN_SHARE);
            run.deadline = closed_deadline;
            report.lap_complete =
                self.retirement_lap(&mut run, &mut report, false, closed_deadline)?;
            if !report.process_use_unavailable {
                let cleaned_deadline = share(1, 1, RETIRE_CLEANED_LAP_MIN_SHARE);
                run.deadline = cleaned_deadline;
                report.cleaned_lap_complete =
                    self.retirement_lap(&mut run, &mut report, true, cleaned_deadline)?;
            }
            last_inspection_failure = run.last_inspection_failure.take();
        } else {
            crate::phase_telemetry::add(
                "checkout_retirement_open_paths_unresolved",
                std::time::Duration::ZERO,
            );
        }
        report.trash = self.drain_checkout_trash(deadline);
        if let Ok(root) = self.recovery_archive_root() {
            let _span = crate::phase_telemetry::span("checkout_retirement_sweep_partials");
            super::retire_operation::sweep_stale_partials(&root);
        }
        let prune_span = crate::phase_telemetry::span("checkout_retirement_prune_pins");
        let retired_pins =
            match self.prune_retired_pins_until(Some(Instant::now() + RETIRE_PIN_PRUNE_BUDGET)) {
                Ok(remaining) => remaining,
                Err(error) => {
                    crate::warn_unrecorded::<(), _>("expire retired-commit pins", Err(error));
                    self.checkout_retirement_progress().retired_pins
                }
            };
        drop(prune_span);
        let _record_span = crate::phase_telemetry::span("checkout_retirement_record_progress");
        let kept = report.kept.values().sum::<usize>();
        let previous = self.checkout_retirement_progress();
        let shrinking =
            report.retired_clean + report.retired_archived + report.retired_operation_archived > 0
                || report.strip.directories_stripped > 0
                || report.trash.entries_removed > 0
                || (report.trash.entries_pending == 0 && kept == 0);
        let progress = CheckoutRetirementProgress {
            trash_pending: report.trash.entries_pending,
            kept,
            stalled_runs: if shrinking {
                0
            } else {
                previous.stalled_runs.saturating_add(1)
            },
            reported_trash_entries: report.trash.reported.clone(),
            unrecognized_trash_entries: report.trash.unrecognized_names.clone(),
            retired_pins,
            stale_pending_trash_entries: report.trash.stale_pending.clone(),
            abandoned_operations_archived: previous
                .abandoned_operations_archived
                .saturating_add(report.retired_operation_archived),
            process_use_unavailable: report.process_use_unavailable
                || report.trash.process_use_unknown,
            trash_entries_in_use: report.trash.entries_in_use,
            last_inspection_failure,
            strip: self.next_checkout_strip_progress(&previous.strip, &report),
        };
        if progress.strip.backlog.observed_at_ms > 0
            && let Some((host_state, _)) = crate::host_state::resolve_host_state_dir()
        {
            let backlog = &progress.strip.backlog;
            let today = progress.strip.days.last_key_value().map(|(_, day)| day);
            crate::warn_unrecorded(
                "record checkout counts in the storage ledger",
                crate::storage_ledger::record_checkout_counts(
                    &host_state,
                    &self.main_root().to_string_lossy(),
                    crate::storage_ledger::CheckoutCounts {
                        folders: backlog.folders,
                        closed_folders: backlog.closed_folders,
                        closed_with_regenerable: backlog.closed_with_regenerable,
                        bytes_freed_by_strip: progress.strip.bytes_freed_total,
                        last_checkouts_stripped: progress.strip.last_checkouts_stripped,
                        last_directories_stripped: progress.strip.last_directories_stripped,
                        observed_at_ms: backlog.observed_at_ms,
                        sessions_created_today: today.map_or(0, |day| day.sessions_created),
                        checkouts_retired_today: today.map_or(0, |day| day.checkouts_retired),
                    },
                ),
            );
        }
        crate::warn_unrecorded(
            "record checkout retirement progress",
            self.store_ref().meta_set(
                CHECKOUT_RETIREMENT_PROGRESS_KEY,
                &serde_json::to_string(&progress).unwrap_or_default(),
            ),
        );
        let busy = !report.lap_complete
            || !report.cleaned_lap_complete
            || report.trash.entries_pending > 0
            || report.strip.more_remaining;
        let next = crate::clock::epoch_ms().saturating_add(if busy {
            RETIRE_BUSY_RECHECK_MS
        } else {
            RETIRE_IDLE_RECHECK_MS
        });
        crate::warn_unrecorded(
            "schedule the next checkout retirement run",
            self.store_ref()
                .meta_set(CHECKOUT_RETIREMENT_DUE_KEY, &next.to_string()),
        );
        Ok(report)
    }

    /// One retirement lap from its cursor until `deadline`: over closed
    /// sessions, or (`cleaned`) over sessions already recorded as cleaned
    /// whose checkout folder is still on disk. `Ok(true)` when the lap
    /// reached its end (the cursor wraps).
    fn retirement_lap(
        &mut self,
        run: &mut RetireRun,
        report: &mut CheckoutRetirementReport,
        cleaned: bool,
        deadline: Instant,
    ) -> Result<bool, BrokerOpError> {
        let _span = crate::phase_telemetry::span(if cleaned {
            "checkout_retirement_cleaned_lap"
        } else {
            "checkout_retirement_closed_lap"
        });
        let cursor_key = if cleaned {
            CHECKOUT_RETIREMENT_CLEANED_CURSOR_KEY
        } else {
            CHECKOUT_RETIREMENT_CURSOR_KEY
        };
        let mut complete = false;
        let superseded: std::collections::BTreeMap<i64, i64> =
            self.meta_json(CHECKOUT_RETIREMENT_SUPERSEDED_KEY);
        let mut newly_superseded = std::collections::BTreeMap::new();
        let mut listing = super::strip::PoolListing::default();
        let mut cursor = self
            .store_ref()
            .meta_get(cursor_key)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        'lap: loop {
            if Instant::now() >= deadline {
                break;
            }
            let page = if cleaned {
                self.store_ref()
                    .cleaned_spawned_sessions_after(cursor, RETIRE_PAGE)?
            } else {
                self.store_ref()
                    .closed_spawned_sessions_after(cursor, RETIRE_PAGE)?
            };
            if page.is_empty() {
                cursor = 0;
                complete = true;
                break;
            }
            for session in page {
                if Instant::now() >= deadline {
                    break 'lap;
                }
                let previous_cursor = cursor;
                cursor = session.id;
                // A row superseded at its path by the same row as last time:
                // no filesystem call, no judgement (0.8.123 re-judged 105 of
                // 112 every run). A new superseder, or none, judges again.
                let superseder = self
                    .store_ref()
                    .sessions_at_worktree_path(&session.worktree_path)
                    .ok()
                    .and_then(|rows| super::strip::superseder_of(&session, &rows));
                if superseder.is_some() && superseded.get(&session.id) == superseder.as_ref() {
                    crate::phase_telemetry::add(
                        "checkout_retirement_superseded_remembered",
                        std::time::Duration::ZERO,
                    );
                    continue;
                }
                // Existence from one listing per pool directory (entry
                // types, no call per row); a pool that cannot be listed is
                // asked per folder.
                let exists = match listing.exists(&session.worktree_path, deadline) {
                    Some(exists) => exists,
                    None => Path::new(&session.worktree_path).exists(),
                };
                if cleaned && !exists {
                    // The usual cleaned session: nothing left on disk.
                    continue;
                }
                if !exists {
                    // Moved into the trash but the cleaned record was
                    // lost: record it now. Any other missing checkout
                    // is the missing-checkout lane's, which proves the
                    // absence first.
                    if self.checkout_proven_trashed(session.id)
                        && self.record_trashed_session_cleaned(session.id)
                    {
                        report.reconciled_trashed += 1;
                        crate::phase_telemetry::add(
                            "checkout_retirement_reconciled_trashed",
                            std::time::Duration::ZERO,
                        );
                    }
                    continue;
                }
                if self.remembered_keep_holds(&session, crate::clock::epoch_ms()) {
                    report.keep_memo_skips += 1;
                    crate::phase_telemetry::add(
                        "checkout_retirement_keep_memo_skips",
                        std::time::Duration::ZERO,
                    );
                    continue;
                }
                report.examined += 1;
                let outcome = self.retire_closed_checkout(session.id, run)?;
                if cleaned && outcome.retired() {
                    report.retired_cleaned_leftovers += 1;
                    crate::phase_telemetry::add(
                        "checkout_retirement_cleaned_leftover_retired",
                        std::time::Duration::ZERO,
                    );
                }
                match outcome {
                    RetireOutcome::RetiredClean => report.retired_clean += 1,
                    RetireOutcome::RetiredArchived { bytes } => {
                        report.retired_archived += 1;
                        report.archive_bytes = report.archive_bytes.saturating_add(bytes);
                    }
                    RetireOutcome::RetiredOperationArchived { bytes } => {
                        report.retired_operation_archived += 1;
                        report.archive_bytes = report.archive_bytes.saturating_add(bytes);
                    }
                    RetireOutcome::Kept(code)
                        if code == "process_use_unknown"
                            || (code == "archive_deferred" && report.examined > 1) =>
                    {
                        // Not a verdict about this checkout: the run
                        // could not look, or has too little time left
                        // for its archive. The next run, due soon,
                        // starts with it. (An archive deferred as the
                        // run's first judgement is passed over instead,
                        // so a slow start can never wedge the lap.)
                        *report.kept.entry(code.to_string()).or_default() += 1;
                        report.process_use_unavailable |= code == "process_use_unknown";
                        cursor = previous_cursor;
                        break 'lap;
                    }
                    RetireOutcome::Kept(code) => {
                        *report.kept.entry(code.to_string()).or_default() += 1;
                        if code == "leftover_superseded"
                            && let Some(superseder) = superseder
                        {
                            newly_superseded.insert(session.id, superseder);
                        }
                    }
                }
            }
        }
        if !newly_superseded.is_empty() {
            crate::warn_unrecorded(
                "remember superseded checkouts",
                self.update_meta_json(
                    CHECKOUT_RETIREMENT_SUPERSEDED_KEY,
                    |stored: &mut std::collections::BTreeMap<i64, i64>| {
                        stored.extend(newly_superseded.iter().map(|(id, by)| (*id, *by)));
                        while stored.len() > RETIRE_SUPERSEDED_MAX {
                            stored.pop_first();
                        }
                    },
                ),
            );
        }
        crate::warn_unrecorded(
            "advance the checkout retirement cursor",
            self.store_ref().meta_set(cursor_key, &cursor.to_string()),
        );
        Ok(complete)
    }

    /// Drain this repository's recorded trash entries until `deadline`, as
    /// the one host-wide deleter (the lock lives in the host state
    /// directory). When that lock is held or cannot be taken, this run
    /// deletes nothing: there is never a second deleter.
    pub(crate) fn drain_checkout_trash(
        &self,
        deadline: Instant,
    ) -> crate::checkout_trash::TrashDrainReport {
        let lock_path = crate::host_state::resolve_host_state_dir()
            .map(|(dir, _)| dir.join(crate::checkout_trash::DELETER_LOCK_FILE));
        self.drain_checkout_trash_with_lock(lock_path.as_deref(), deadline)
    }

    pub(crate) fn drain_checkout_trash_with_lock(
        &self,
        lock_path: Option<&Path>,
        deadline: Instant,
    ) -> crate::checkout_trash::TrashDrainReport {
        let mut rows = self.checkout_trash_rows();
        let pending = rows.values().map(std::collections::BTreeMap::len).sum();
        let Some(first) = rows.keys().next().cloned() else {
            return crate::checkout_trash::TrashDrainReport::default();
        };
        let lock =
            lock_path.and_then(|path| crate::checkout_trash::try_deleter_lock(path).ok().flatten());
        let Some(_lock) = lock else {
            crate::phase_telemetry::add("checkout_trash_deleter_busy", std::time::Duration::ZERO);
            return crate::checkout_trash::TrashDrainReport {
                entries_pending: pending,
                ..crate::checkout_trash::TrashDrainReport::default()
            };
        };
        let before = rows.clone();
        let pace = crate::checkout_trash::DeleterPace::for_host(
            self.checkout_trash_below_headroom(&first),
            crate::checkout_trash::host_loaded(),
        );
        let mut report = crate::checkout_trash::drain_trash(&mut rows, deadline, pace);
        // Entries recorded by another process during the drain are kept:
        // only what this run removed, forgot, confirmed or counted is
        // applied, atomically.
        let applied = self.update_meta_json(
            CHECKOUT_RETIREMENT_TRASH_ROWS_KEY,
            |current: &mut crate::checkout_trash::TrashRows| {
                for (dir, entries) in &before {
                    for name in entries.keys() {
                        match rows.get(dir).and_then(|after| after.get(name)) {
                            Some(row) => {
                                if let Some(stored) =
                                    current.get_mut(dir).and_then(|stored| stored.get_mut(name))
                                {
                                    stored.failures = row.failures;
                                    stored.pending = row.pending;
                                }
                            }
                            None => {
                                if let Some(stored) = current.get_mut(dir) {
                                    stored.remove(name);
                                }
                            }
                        }
                    }
                }
                current.retain(|_, entries| !entries.is_empty());
            },
        );
        crate::warn_unrecorded("record the checkout trash drain", applied);
        report.entries_pending = self
            .checkout_trash_rows()
            .values()
            .map(std::collections::BTreeMap::len)
            .sum();
        report
    }

    /// Whether the volume holding `trash` is below the artifact-sweep
    /// headroom (bytes or inodes): the deleter then runs unpaced.
    fn checkout_trash_below_headroom(&self, trash: &Path) -> bool {
        let required = crate::load_retention_policy(self.main_root())
            .map(|policy| policy.artifact_sweep_headroom_bytes)
            .unwrap_or(0)
            .max(crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES);
        let headroom =
            crate::disk_headroom::available_headroom_at_or_above_for(self.main_root(), trash);
        crate::disk_headroom::sweep_urgency_with_inodes(
            headroom.map(|value| value.bytes),
            required,
            headroom.and_then(|value| value.inodes),
            crate::disk_headroom::MIN_GATE_HEADROOM_INODES,
        ) == crate::disk_headroom::SweepUrgency::Pressured
    }

    /// Recreate the checkout of a session whose checkout was retired, at its
    /// recorded `path`, from the session branch, so `start --reuse` can go on
    /// there. `Ok(false)` when no retired session names `path`. When a
    /// recovery archive holds leftovers of that session, nothing is created
    /// and the error names the archive and its restore steps.
    pub(crate) fn recreate_retired_checkout(&self, path: &Path) -> Result<bool, BrokerOpError> {
        let Some(session) = self
            .store_ref()
            .latest_session_for_worktree_any_state(&path.to_string_lossy())?
        else {
            return Ok(false);
        };
        if session.cleanup_state != crate::SessionCleanupState::Cleaned
            || session.origin != crate::SessionOrigin::Spawned
            || path.exists()
        {
            return Ok(false);
        }
        let refuse = |reason: String| BrokerOpError::DirtyWorktree {
            id: session.id,
            reason,
        };
        let Some(_claim) = self.try_claim_worktree_lifecycle("retire")? else {
            return Err(refuse(
                "another checkout lifecycle operation is running; retry in a moment".into(),
            ));
        };
        if let Ok(root) = self.recovery_archive_root()
            && let Ok(listing) = std::fs::read_dir(&root)
        {
            let prefix = format!("{}-", session.id);
            if let Some(archive) = listing
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|archive| {
                    archive
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with(&prefix) && !name.ends_with(".partial")
                        })
                })
            {
                return Err(refuse(format!(
                    "the checkout was retired with a recovery archive; recreate it with `git -C {} worktree add {} {}` and restore its leftovers as {} describes",
                    self.main_root().display(),
                    path.display(),
                    session.branch,
                    archive.join("README.md").display()
                )));
            }
        }
        if self
            .repo_handle()
            .resolve_ref(&format!("refs/heads/{}", session.branch))
            .is_none()
        {
            return Err(refuse(format!(
                "the checkout was retired and its branch {} no longer exists",
                session.branch
            )));
        }
        let mut command = crate::git::git_command();
        command
            .args(["worktree", "add"])
            .arg(path)
            .arg(&session.branch)
            .current_dir(self.main_root());
        match crate::bounded_output::output_limited(
            &mut command,
            crate::git::git_timeout(),
            1024 * 1024,
        ) {
            Ok(crate::bounded_output::LimitedOutput::Completed(output))
                if output.status.success() =>
            {
                crate::phase_telemetry::add(
                    "checkout_retirement_recreated_for_reuse",
                    std::time::Duration::ZERO,
                );
                Ok(true)
            }
            Ok(crate::bounded_output::LimitedOutput::Completed(output)) => Err(refuse(format!(
                "cannot recreate the retired checkout: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
            Ok(_) => Err(refuse(
                "cannot recreate the retired checkout in time".into(),
            )),
            Err(error) => Err(refuse(format!(
                "cannot recreate the retired checkout: {error}"
            ))),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    fn git(root: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn git_status(root: &Path, args: &[&str]) -> bool {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        command
            .args(args)
            .current_dir(root)
            .output()
            .unwrap()
            .status
            .success()
    }

    fn set_hook(hook: Option<Box<dyn FnMut()>>) {
        super::BEFORE_RETIRE_CLAIM.with(|cell| *cell.borrow_mut() = hook);
    }

    /// A repository with a local `origin` whose default branch is known, no
    /// closed-checkout grace, an ignore rule for a file nothing regenerates,
    /// and no process using any checkout.
    fn fixture() -> (
        tempfile::TempDir,
        crate::test_storage::TestStorage,
        crate::Broker,
    ) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Retire test"]);
        git(&root, &["config", "user.email", "retire@example.invalid"]);
        std::fs::write(root.join(".gitignore"), "/.aethyme/\nsecrets.local\n").unwrap();
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        git(&root, &["add", ".gitignore", "base.txt"]);
        git(&root, &["commit", "-qm", "fixture"]);
        aethyme_testkit::ensure_origin(&root);
        git(&root, &["remote", "set-head", "origin", "main"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 72\nclosed_worktree_grace_hours = 0\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&root).unwrap();
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
        (owner, storage, broker)
    }

    /// Start a session, let `setup` work in its checkout, then close it.
    fn closed(
        broker: &mut crate::Broker,
        label: &str,
        setup: impl FnOnce(&Path),
    ) -> (crate::Session, PathBuf) {
        let session = broker.start_worktree(label, None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        setup(&worktree);
        broker
            .close_abandoning_unpushed(session.id, "retire fixture")
            .unwrap();
        (session, worktree)
    }

    fn run(broker: &mut crate::Broker) -> CheckoutRetirementReport {
        broker
            .run_checkout_retirement(Instant::now() + Duration::from_secs(600))
            .unwrap()
    }

    fn is_cleaned(broker: &crate::Broker, session_id: i64) -> bool {
        broker
            .store_ref()
            .session(session_id)
            .unwrap()
            .cleanup_state
            == crate::SessionCleanupState::Cleaned
    }

    /// The archives written for `session_id`.
    fn archives(broker: &crate::Broker, session_id: i64) -> Vec<PathBuf> {
        let root = broker.recovery_archive_root().unwrap();
        let Ok(listing) = std::fs::read_dir(&root) else {
            return Vec::new();
        };
        listing
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with(&format!("{session_id}-"))
                                && !name.ends_with(".partial")
                        })
            })
            .collect()
    }

    /// A clean checkout whose commits are all on the default branch is
    /// retired without an archive: renamed into the trash, drained, the row
    /// cleaned and the branch ref kept.
    #[test]
    fn a_clean_contained_checkout_is_retired_without_an_archive() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "clean", |_| {});

        let report = run(&mut broker);

        assert_eq!(report.retired_clean, 1, "{report:#?}");
        assert!(!worktree.exists());
        assert!(is_cleaned(&broker, session.id));
        assert!(
            archives(&broker, session.id).is_empty(),
            "no archive needed"
        );
        assert_eq!(report.trash.entries_pending, 0, "{report:#?}");
        let root = broker.main_root().to_path_buf();
        git(
            &root,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", session.branch),
            ],
        );
    }

    /// Only a spawned session's checkout inside a broker-owned pool may go
    /// into a trash; a checkout anywhere else is removed by Git as before.
    #[test]
    fn a_checkout_outside_a_broker_pool_is_never_trashed() {
        let (owner, _storage, mut broker) = fixture();
        let session = broker.start_worktree("pooled", None).unwrap();
        let elsewhere = owner.path().join("elsewhere");
        let root = broker.main_root().to_path_buf();
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "elsewhere",
                elsewhere.to_str().unwrap(),
            ],
        );
        let mut outside = session.clone();
        outside.worktree_path = elsewhere.to_string_lossy().into_owned();

        assert!(broker.trash_pool_for(&outside, &elsewhere).is_none());
        assert!(
            broker
                .trash_pool_for(&session, Path::new(&session.worktree_path))
                .is_some()
        );
        let mut adopted = session.clone();
        adopted.origin = crate::SessionOrigin::Adopted;
        assert!(
            broker
                .trash_pool_for(&adopted, Path::new(&session.worktree_path))
                .is_none()
        );
    }

    /// A dirty checkout (a tracked change, an untracked file, an ignored
    /// file nothing regenerates) is archived, the archive read back, and
    /// only then removed; the archive holds every file.
    #[test]
    fn a_dirty_checkout_is_archived_then_retired() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "dirty", |worktree| {
            std::fs::write(worktree.join("base.txt"), "changed, not committed\n").unwrap();
            std::fs::write(worktree.join("notes.txt"), "untracked notes\n").unwrap();
            std::fs::write(worktree.join("secrets.local"), "only here\n").unwrap();
        });

        let report = run(&mut broker);

        assert_eq!(report.retired_archived, 1, "{report:#?}");
        assert!(!worktree.exists());
        assert!(is_cleaned(&broker, session.id));
        let archive = archives(&broker, session.id)
            .pop()
            .expect("a verified archive was kept");
        assert_eq!(
            std::fs::read_to_string(archive.join("untracked/notes.txt")).unwrap(),
            "untracked notes\n"
        );
        assert_eq!(
            std::fs::read_to_string(archive.join("ignored/secrets.local")).unwrap(),
            "only here\n"
        );
        let patch = std::fs::read_to_string(archive.join("unstaged.patch")).unwrap();
        assert!(patch.contains("+changed, not committed"), "{patch}");
    }

    /// A commit no delivery target holds is bundled; the bundle restores it.
    #[test]
    fn an_unlanded_commit_is_bundled_then_retired() {
        let (_owner, _storage, mut broker) = fixture();
        let mut head = String::new();
        let (session, worktree) = closed(&mut broker, "unlanded", |worktree| {
            std::fs::write(worktree.join("work.txt"), "unlanded\n").unwrap();
            git(worktree, &["add", "work.txt"]);
            git(worktree, &["commit", "-qm", "unlanded work"]);
            head = git(worktree, &["rev-parse", "HEAD"]).trim().to_string();
        });

        let report = run(&mut broker);

        assert_eq!(report.retired_archived, 1, "{report:#?}");
        assert!(!worktree.exists());
        let archive = archives(&broker, session.id).pop().expect("an archive");
        let bundle = archive.join("commits.bundle");
        let root = broker.main_root().to_path_buf();
        git(&root, &["bundle", "verify", bundle.to_str().unwrap()]);
        let heads = git(&root, &["bundle", "list-heads", bundle.to_str().unwrap()]);
        assert!(heads.contains(&head), "{heads}");
    }

    /// An archive over the cap keeps the checkout and is shown by session id.
    #[test]
    fn an_archive_over_the_cap_keeps_the_checkout() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "too large", |worktree| {
            std::fs::write(worktree.join("notes.txt"), "more than one byte\n").unwrap();
        });
        let _cap = retire_archive_cap_for_test(1);

        let report = run(&mut broker);

        assert_eq!(
            report.kept.get("archive_too_large"),
            Some(&1),
            "{report:#?}"
        );
        assert!(worktree.join("notes.txt").exists(), "kept untouched");
        assert!(broker.checkout_retirement_too_large().contains(&session.id));
        let view = broker.status(crate::clock::epoch_ms()).unwrap();
        let advice = view
            .advice
            .iter()
            .find(|advice| advice.id == "cleanup.retire-archive-too-large")
            .expect("shown");
        assert_eq!(advice.session_id, Some(session.id));
        let shown = serde_json::to_string(advice).unwrap();
        assert!(!shown.contains(&session.worktree_path), "{shown}");
    }

    /// An open session's checkout is never touched, and a closed one a
    /// process still uses is kept under that precondition.
    #[test]
    fn open_or_used_checkouts_are_never_touched() {
        let (_owner, _storage, mut broker) = fixture();
        let open = broker.start_worktree("still open", None).unwrap();
        let (used, used_worktree) = closed(&mut broker, "in use", |_| {});
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "build".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: std::fs::canonicalize(&used_worktree).unwrap(),
            }]),
        ));

        let report = run(&mut broker);

        assert_eq!(
            report.kept.get("process_using_checkout"),
            Some(&1),
            "{report:#?}"
        );
        assert!(used_worktree.exists());
        assert!(!is_cleaned(&broker, used.id));
        assert!(Path::new(&open.worktree_path).join("base.txt").exists());
        assert_eq!(report.examined, 1, "the open session is never judged");
    }

    /// A backlog of closed checkouts is cleared in one run, with the open
    /// sessions' paths resolved once for the whole run.
    #[test]
    fn a_backlog_is_cleared_in_one_run_resolving_open_paths_once() {
        let (_owner, _storage, mut broker) = fixture();
        let _open = broker.start_worktree("open meanwhile", None).unwrap();
        let backlog = (0..5)
            .map(|index| closed(&mut broker, &format!("backlog {index}"), |_| {}))
            .collect::<Vec<_>>();
        let recorder = crate::phase_telemetry::Recorder::default();

        let report = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            run(&mut broker)
        };

        assert_eq!(report.examined, 5, "{report:#?}");
        assert_eq!(report.retired_clean, 5, "{report:#?}");
        assert!(report.lap_complete);
        assert_eq!(report.open_path_resolutions, 1);
        assert_eq!(
            recorder
                .totals()
                .1
                .get("checkout_retirement_open_paths")
                .copied(),
            Some(1),
            "open paths resolved once per run, not per page or session"
        );
        let counts = recorder.totals().1;
        assert_eq!(
            counts.get("checkout_retirement_process_snapshot").copied(),
            Some(1),
            "one open-file observation for the whole run"
        );
        assert_eq!(
            counts.get("checkout_trash_run_process_use_reused").copied(),
            Some(5),
            "no rename took another"
        );
        for phase in [
            "checkout_retirement_phase_ownership",
            "checkout_retirement_phase_sessions",
            "checkout_retirement_phase_operation_state",
            "checkout_retirement_phase_process_use",
            "checkout_retirement_phase_plan",
            "checkout_retirement_phase_move",
        ] {
            assert_eq!(counts.get(phase).copied(), Some(5), "{phase}");
        }
        for (session, worktree) in backlog {
            assert!(!worktree.exists());
            assert!(is_cleaned(&broker, session.id));
        }
    }

    /// A rebase stopped on conflicts keeps the checkout: its state lives in
    /// the registration and is neither in Git nor archived.
    #[test]
    fn a_checkout_mid_rebase_is_kept_untouched() {
        let (_owner, _storage, mut broker) = fixture();
        let root = broker.main_root().to_path_buf();
        let (session, worktree) = closed(&mut broker, "mid rebase", |worktree| {
            std::fs::write(worktree.join("base.txt"), "session side\n").unwrap();
            git(worktree, &["commit", "-qam", "session side"]);
            std::fs::write(root.join("base.txt"), "main side\n").unwrap();
            git(&root, &["commit", "-qam", "main side"]);
            assert!(
                !git_status(worktree, &["rebase", "main"]),
                "the rebase conflicts"
            );
        });

        let report = run(&mut broker);

        assert_eq!(
            report.kept.get("operation_in_progress"),
            Some(&1),
            "{report:#?}"
        );
        assert!(worktree.join("base.txt").exists());
        assert!(!is_cleaned(&broker, session.id));
        assert!(
            broker
                .checkout_retirement_in_progress()
                .contains(&session.id)
        );
        let view = broker.status(crate::clock::epoch_ms()).unwrap();
        assert!(
            view.advice
                .iter()
                .any(|advice| advice.id == "cleanup.retire-operation-in-progress"
                    && advice.session_id == Some(session.id))
        );
    }

    /// An ignored file nothing regenerates, written after the plan, keeps
    /// the checkout: the re-check under the claim sees ignored files.
    #[test]
    fn an_ignored_file_written_after_the_plan_keeps_the_checkout() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "late ignored file", |_| {});
        let late = worktree.clone();
        set_hook(Some(Box::new(move || {
            std::fs::write(late.join("secrets.local"), "written late\n").unwrap();
        })));
        let report = run(&mut broker);
        set_hook(None);

        assert_eq!(report.kept.get("changed"), Some(&1), "{report:#?}");
        assert!(worktree.join("secrets.local").exists());
        assert!(!is_cleaned(&broker, session.id));
    }

    /// The run's one observation decides: a process arriving after it does
    /// not cost a second system-wide observation before the rename. The
    /// rename goes ahead (on Unix a process keeps a working directory that
    /// moved), and the deleter is what never deletes an entry in use.
    #[test]
    fn a_process_arriving_after_the_run_observation_does_not_stop_the_rename() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "late process", |_| {});
        let used = std::fs::canonicalize(&worktree).unwrap();
        set_hook(Some(Box::new(move || {
            crate::session_holder::set_process_use_snapshot_for_tests(Some(
                crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                    pid: std::process::id(),
                    command: "build".into(),
                    descriptor: "cwd".into(),
                    file_type: Some("DIR".into()),
                    path: used.clone(),
                }]),
            ));
        })));
        let recorder = crate::phase_telemetry::Recorder::default();
        let report = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            run(&mut broker)
        };
        set_hook(None);

        assert_eq!(report.retired_clean, 1, "{report:#?}");
        assert!(!worktree.exists());
        assert!(is_cleaned(&broker, session.id));
        let counts = recorder.totals().1;
        assert_eq!(
            counts.get("checkout_retirement_process_snapshot").copied(),
            Some(1)
        );
        assert_eq!(
            counts.get("checkout_trash_run_process_use_reused").copied(),
            Some(1),
            "the rename reused the run's observation"
        );
    }

    /// The deleter leaves an entry alone while a process still has its
    /// working directory inside, and deletes it once the process is gone.
    #[test]
    fn the_deleter_never_deletes_an_entry_a_process_still_uses() {
        let (owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "still inside", |_| {});
        let current = broker.store_ref().session(session.id).unwrap();
        {
            let _lane = super::RetirementLaneScope::enter();
            broker
                .remove_checkout_through_trash(&current, &worktree, true)
                .unwrap();
        }
        let (trash_dir, entry) = broker
            .checkout_trash_rows()
            .into_iter()
            .flat_map(|(dir, entries)| entries.into_keys().map(move |name| (dir.clone(), name)))
            .next()
            .unwrap();
        let inside = std::fs::canonicalize(trash_dir.join(&entry)).unwrap();
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "shell".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: inside.clone(),
            }]),
        ));
        let lock = owner.path().join(crate::checkout_trash::DELETER_LOCK_FILE);
        let far = Instant::now() + Duration::from_secs(600);

        let held = broker.drain_checkout_trash_with_lock(Some(&lock), far);
        assert_eq!(
            (
                held.entries_removed,
                held.entries_in_use,
                held.entries_pending
            ),
            (0, 1, 1),
            "{held:?}"
        );
        assert!(inside.exists());

        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Incomplete {
                reason: "lsof timed out".into(),
                observed: Vec::new(),
            },
        ));
        let unknown = broker.drain_checkout_trash_with_lock(Some(&lock), far);
        assert!(unknown.process_use_unknown, "{unknown:?}");
        assert_eq!(unknown.entries_removed, 0);
        assert!(inside.exists());

        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
        let drained = broker.drain_checkout_trash_with_lock(Some(&lock), far);
        assert_eq!(drained.entries_removed, 1, "{drained:?}");
        assert!(!inside.exists());
    }

    /// An open-file observation the run cannot take completely is not a
    /// verdict about any checkout: the lap stops where it needed it, keeps
    /// nothing in memory, says so, and the next run retires it.
    #[test]
    fn an_unavailable_process_observation_stops_the_lap_and_retries() {
        let (_owner, _storage, mut broker) = fixture();
        let (first, first_path) = closed(&mut broker, "first", |_| {});
        let (second, second_path) = closed(&mut broker, "second", |_| {});
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Incomplete {
                reason: "lsof exceeded its time limit".into(),
                observed: Vec::new(),
            },
        ));

        let stalled = run(&mut broker);

        assert!(stalled.process_use_unavailable, "{stalled:#?}");
        assert_eq!(
            stalled.examined, 1,
            "the lap stopped at the first: {stalled:#?}"
        );
        assert_eq!(stalled.kept.get("process_use_unknown"), Some(&1));
        assert!(!stalled.lap_complete);
        assert!(first_path.exists() && second_path.exists());
        assert!(broker.checkout_retirement_keeps().is_empty());
        assert!(
            broker
                .checkout_retirement_progress()
                .process_use_unavailable
        );

        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
        let resumed = run(&mut broker);
        assert_eq!(resumed.retired_clean, 2, "{resumed:#?}");
        assert!(is_cleaned(&broker, first.id) && is_cleaned(&broker, second.id));
        assert!(
            !broker
                .checkout_retirement_progress()
                .process_use_unavailable
        );
    }

    /// What an abandoned-merge fixture left behind.
    struct AbandonedMerge {
        session: crate::Session,
        worktree: PathBuf,
        session_head: String,
        staged_blob: String,
    }

    /// Set `path`'s modification time two days back.
    fn two_days_old(path: &Path) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(48 * 3_600))
            .unwrap();
    }

    /// A closed session left mid-merge: a conflicted file, an untracked
    /// file, and a file staged then edited again (its staged version lives
    /// only in the index). Every marker, the index, the reflog and the
    /// changed files are two days old, and so is the session's activity.
    fn abandoned_merge(broker: &mut crate::Broker) -> AbandonedMerge {
        let root = broker.main_root().to_path_buf();
        let mut session_head = String::new();
        let mut staged_blob = String::new();
        let (session, worktree) = closed(broker, "abandoned merge", |worktree| {
            std::fs::write(worktree.join("base.txt"), "session side\n").unwrap();
            git(worktree, &["commit", "-qam", "session side"]);
            session_head = git(worktree, &["rev-parse", "HEAD"]).trim().to_string();
            std::fs::write(root.join("base.txt"), "main side\n").unwrap();
            git(&root, &["commit", "-qam", "main side"]);
            assert!(
                !git_status(worktree, &["merge", "main"]),
                "the merge conflicts"
            );
            std::fs::write(worktree.join("scratch.txt"), "untracked notes\n").unwrap();
            std::fs::write(worktree.join("extra.txt"), "staged version\n").unwrap();
            git(worktree, &["add", "extra.txt"]);
            staged_blob = git(worktree, &["rev-parse", ":extra.txt"])
                .trim()
                .to_string();
            std::fs::write(worktree.join("extra.txt"), "edited after staging\n").unwrap();
        });
        let git_dir = crate::git::checked_worktree_git_dir(&worktree)
            .unwrap()
            .unwrap();
        for path in [
            git_dir.join("MERGE_HEAD"),
            git_dir.join("index"),
            git_dir.join("logs/HEAD"),
            worktree.join("base.txt"),
            worktree.join("scratch.txt"),
            worktree.join("extra.txt"),
        ] {
            if path.exists() {
                two_days_old(&path);
            }
        }
        let old = crate::clock::epoch_ms() - 48 * 3_600_000;
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET last_activity_at = ?1, closed_at = ?1 WHERE id = ?2",
                rusqlite::params![old, session.id],
            )
            .unwrap();
        AbandonedMerge {
            session,
            worktree,
            session_head,
            staged_blob,
        }
    }

    /// The operation archives written for `session_id`.
    fn operation_archive(broker: &crate::Broker, session_id: i64) -> Option<PathBuf> {
        archives(broker, session_id).into_iter().find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("-operation-"))
        })
    }

    /// A closed session's merge abandoned past the threshold is archived
    /// (operation state, raw index, conflicted files, and a pack of every
    /// object only the index held) and its checkout retired. After `git gc`
    /// prunes everything unreachable, the archive restores every staged and
    /// unmerged blob.
    #[test]
    fn an_abandoned_merge_is_archived_with_its_state_then_retired() {
        use std::os::unix::fs::PermissionsExt;
        let (_owner, _storage, mut broker) = fixture();
        let root = broker.main_root().to_path_buf();
        let fixture = abandoned_merge(&mut broker);
        let (session, worktree) = (&fixture.session, &fixture.worktree);

        let report = run(&mut broker);

        assert_eq!(report.retired_operation_archived, 1, "{report:#?}");
        assert!(!worktree.exists());
        assert!(is_cleaned(&broker, session.id));
        let archive = operation_archive(&broker, session.id).expect("an operation archive");
        assert!(archive.join("git-state/MERGE_HEAD").is_file());
        assert!(archive.join("git-state/index").is_file());
        assert!(
            std::fs::read_to_string(archive.join("worktree/base.txt"))
                .unwrap()
                .contains("<<<<<<<"),
            "the conflicted file is archived as it was"
        );
        assert_eq!(
            std::fs::read_to_string(archive.join("worktree/extra.txt")).unwrap(),
            "edited after staging\n"
        );
        assert!(archive.join("README.md").is_file());
        assert!(archive.join("manifest.json").is_file());
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&archive), 0o700);
        assert_eq!(
            mode(archive.parent().unwrap()),
            0o700,
            "the archive root is private"
        );
        assert_eq!(mode(&archive.join("worktree/scratch.txt")), 0o600);
        // The session's own commit is pinned.
        git(
            &root,
            &[
                "rev-parse",
                "--verify",
                &format!(
                    "refs/aethyme/retired/{}/{}",
                    session.id, fixture.session_head
                ),
            ],
        );
        assert_eq!(
            broker
                .checkout_retirement_progress()
                .abandoned_operations_archived,
            1
        );
        assert!(
            !broker
                .checkout_retirement_in_progress()
                .contains(&session.id)
        );
        let view = broker.status(crate::clock::epoch_ms()).unwrap();
        assert!(
            view.advice
                .iter()
                .any(|advice| advice.id == "cleanup.retire-abandoned-operations-archived")
        );

        // Nothing but the archive holds the staged version once Git prunes.
        git(&root, &["reflog", "expire", "--expire=now", "--all"]);
        git(&root, &["gc", "--prune=now", "-q"]);
        assert!(
            !git_status(&root, &["cat-file", "-e", &fixture.staged_blob]),
            "pruned from the repository"
        );
        let pack_dir = root.join(".git/objects/pack");
        for entry in std::fs::read_dir(archive.join("objects")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            if name.ends_with(".pack") || name.ends_with(".idx") {
                std::fs::copy(&path, pack_dir.join(&name)).unwrap();
            }
        }
        assert!(git_status(&root, &["cat-file", "-e", &fixture.staged_blob]));
        let mut listing = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                listing.env_remove(key);
            }
        }
        let output = listing
            .env("GIT_INDEX_FILE", archive.join("git-state/index"))
            .args(["ls-files", "--stage"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(output.status.success());
        let entries = String::from_utf8(output.stdout).unwrap();
        let stages = entries
            .lines()
            .map(|line| line.split_whitespace().nth(1).unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(
            stages.len() >= 4,
            "three conflict stages and the staged file: {entries}"
        );
        for id in stages {
            assert!(
                git_status(&root, &["cat-file", "-e", &id]),
                "{id} reads back"
            );
        }
    }

    /// A working file edited recently keeps the checkout, however old the
    /// merge markers are: someone may be resolving the conflicts.
    #[test]
    fn a_recently_edited_working_file_keeps_an_old_merge() {
        let (_owner, _storage, mut broker) = fixture();
        let fixture = abandoned_merge(&mut broker);
        std::fs::write(fixture.worktree.join("scratch.txt"), "still working\n").unwrap();

        let report = run(&mut broker);

        assert_eq!(
            report.kept.get("operation_in_progress"),
            Some(&1),
            "{report:#?}"
        );
        assert!(fixture.worktree.join("scratch.txt").is_file());
        assert!(operation_archive(&broker, fixture.session.id).is_none());
    }

    /// An edit between the copy and the move keeps the checkout (the edit
    /// would otherwise reach only the trash) and forgets the keep.
    #[test]
    fn an_edit_between_the_copy_and_the_move_keeps_the_checkout() {
        let (_owner, _storage, mut broker) = fixture();
        let fixture = abandoned_merge(&mut broker);
        let scratch = fixture.worktree.join("scratch.txt");
        let edited = scratch.clone();
        super::BEFORE_OPERATION_MOVE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::write(&edited, "edited after the copy\n").unwrap();
            }));
        });
        let report = run(&mut broker);
        super::BEFORE_OPERATION_MOVE.with(|hook| *hook.borrow_mut() = None);

        assert_eq!(report.kept.get("changed"), Some(&1), "{report:#?}");
        assert_eq!(
            std::fs::read_to_string(&scratch).unwrap(),
            "edited after the copy\n"
        );
        assert!(!is_cleaned(&broker, fixture.session.id));
        assert!(
            !broker
                .checkout_retirement_keeps()
                .contains_key(&fixture.session.id)
        );
    }

    /// A partial operation archive older than a day whose creator is gone
    /// is swept; a recent one and a finished archive are left alone, and
    /// the storage ledger never reads either partial as an archive.
    #[test]
    fn stale_partial_operation_archives_are_swept_once_their_creator_is_gone() {
        let root = tempfile::tempdir().unwrap();
        let now = crate::clock::epoch_ms();
        let gone_pid = i32::MAX;
        let stale = root.path().join(format!(
            ".partial-operation-7-{}-{gone_pid}-1-ab",
            now - 48 * 3_600_000
        ));
        let fresh = root
            .path()
            .join(format!(".partial-operation-7-{now}-{gone_pid}-1-cd"));
        let finished = root.path().join("7-operation-0123456789ab-ef");
        for directory in [&stale, &fresh, &finished] {
            std::fs::create_dir_all(directory).unwrap();
            std::fs::write(directory.join("ids.txt"), "x\n").unwrap();
        }

        assert_eq!(
            super::super::retire_operation::sweep_stale_partials(root.path()),
            1
        );
        assert!(!stale.exists());
        assert!(fresh.exists() && finished.exists());
        assert!(crate::storage_ledger::is_partial_archive_name(
            fresh.file_name().unwrap().to_str().unwrap()
        ));
        assert!(crate::storage_ledger::is_partial_archive_name(
            "12.retirement-v6.partial"
        ));
        assert!(!crate::storage_ledger::is_partial_archive_name(
            "7-operation-0123456789ab-ef"
        ));
    }

    /// A plan refusal is remembered with the refusing step's own words,
    /// which `broker gc plan --json` shows per session.
    #[test]
    fn a_plan_refusal_keeps_its_reason_for_the_gc_plan() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "nested repository", |worktree| {
            let nested = worktree.join("nested");
            std::fs::create_dir_all(&nested).unwrap();
            git(&nested, &["init", "-q"]);
        });

        let report = run(&mut broker);

        assert_eq!(report.kept.get("plan_refused"), Some(&1), "{report:#?}");
        assert!(worktree.exists());
        let keeps = broker.checkout_retirement_keep_report();
        let keep = keeps
            .iter()
            .find(|keep| keep.session_id == session.id)
            .expect("remembered");
        assert_eq!(keep.reason, "plan_refused");
        assert!(
            keep.detail
                .as_deref()
                .is_some_and(|detail| !detail.trim().is_empty()),
            "{keep:?}"
        );
    }

    /// An ignored directory nothing catalogues as regenerable no longer
    /// refuses the plan: its files are archived and the checkout retired.
    #[test]
    fn an_unknown_ignored_directory_is_archived_then_retired() {
        let (_owner, _storage, mut broker) = fixture();
        let root = broker.main_root().to_path_buf();
        std::fs::create_dir_all(root.join(".git/info")).unwrap();
        std::fs::write(root.join(".git/info/exclude"), "/tool-notes/\n").unwrap();
        let (session, worktree) = closed(&mut broker, "ignored directory", |worktree| {
            std::fs::create_dir_all(worktree.join("tool-notes/quality")).unwrap();
            std::fs::write(worktree.join("tool-notes/quality/index.json"), "{}\n").unwrap();
        });

        let report = run(&mut broker);

        assert_eq!(report.retired_archived, 1, "{report:#?}");
        assert!(!worktree.exists());
        let archive = archives(&broker, session.id).into_iter().next().unwrap();
        assert_eq!(
            std::fs::read_to_string(archive.join("ignored/tool-notes/quality/index.json")).unwrap(),
            "{}\n"
        );
    }

    /// A worktree its user locked is kept, never removed synchronously by
    /// Git inside the lane.
    #[test]
    fn a_refused_trash_move_keeps_the_checkout() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "user locked", |_| {});
        let root = broker.main_root().to_path_buf();
        git(
            &root,
            &[
                "worktree",
                "lock",
                "--reason",
                "mine",
                worktree.to_str().unwrap(),
            ],
        );
        let report = run(&mut broker);
        assert_eq!(report.kept.get("trash_refused"), Some(&1), "{report:#?}");
        assert!(worktree.join("base.txt").exists());
        assert!(!is_cleaned(&broker, session.id));
        assert!(
            broker.checkout_retirement_owns(session.id),
            "the old lanes skip it"
        );
    }

    /// A commit only the checkout's reflog reaches is pinned under a ref
    /// before the registration (and its reflog) goes.
    #[test]
    fn a_reflog_only_commit_is_pinned_before_retirement() {
        let (_owner, _storage, mut broker) = fixture();
        let mut lost = String::new();
        let (session, worktree) = closed(&mut broker, "reflog only", |worktree| {
            std::fs::write(worktree.join("work.txt"), "only in the reflog\n").unwrap();
            git(worktree, &["add", "work.txt"]);
            git(worktree, &["commit", "-qm", "soon unreachable"]);
            lost = git(worktree, &["rev-parse", "HEAD"]).trim().to_string();
            git(worktree, &["reset", "-q", "--hard", "HEAD~1"]);
        });

        let report = run(&mut broker);

        assert_eq!(report.retired_clean, 1, "{report:#?}");
        assert!(!worktree.exists());
        let root = broker.main_root().to_path_buf();
        let pinned = git(
            &root,
            &[
                "rev-parse",
                &format!("refs/aethyme/retired/{}/{lost}", session.id),
            ],
        );
        assert_eq!(pinned.trim(), lost);
    }

    /// A commit reset away in the checkout (only its HEAD reflog names it)
    /// and a commit only a per-worktree bisect ref points at both survive
    /// retirement, `reflog expire` and `gc --prune=now`: they are pinned.
    #[test]
    fn reflog_and_bisect_ref_commits_survive_retirement_and_gc() {
        let (_owner, _storage, mut broker) = fixture();
        let mut reset_away = String::new();
        let mut bisect_only = String::new();
        let (session, worktree) = closed(&mut broker, "reflog and bisect", |worktree| {
            std::fs::write(worktree.join("work.txt"), "reset away\n").unwrap();
            git(worktree, &["add", "work.txt"]);
            git(worktree, &["commit", "-qm", "soon reset away"]);
            reset_away = git(worktree, &["rev-parse", "HEAD"]).trim().to_string();
            git(worktree, &["reset", "-q", "--hard", "HEAD~1"]);
            let tree = git(worktree, &["rev-parse", "HEAD^{tree}"])
                .trim()
                .to_string();
            bisect_only = git(
                worktree,
                &[
                    "commit-tree",
                    "-p",
                    "HEAD",
                    "-m",
                    "only a bisect ref",
                    &tree,
                ],
            )
            .trim()
            .to_string();
            git(worktree, &["update-ref", "refs/bisect/bad", &bisect_only]);
        });

        let report = run(&mut broker);

        assert_eq!(report.retired_clean, 1, "{report:#?}");
        assert!(!worktree.exists());
        let root = broker.main_root().to_path_buf();
        git(&root, &["reflog", "expire", "--expire=now", "--all"]);
        git(&root, &["gc", "--prune=now", "-q"]);
        for commit in [&reset_away, &bisect_only] {
            assert!(
                git_status(&root, &["cat-file", "-e", &format!("{commit}^{{commit}}")]),
                "{commit} survived"
            );
            git(
                &root,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("refs/aethyme/retired/{}/{commit}", session.id),
                ],
            );
        }
    }

    /// A detached HEAD's commit is pinned even when the worktree has no
    /// HEAD reflog, and the missing reflog is recorded.
    #[test]
    fn a_detached_head_without_a_reflog_is_pinned() {
        let (_owner, _storage, mut broker) = fixture();
        let mut detached = String::new();
        let (session, worktree) = closed(&mut broker, "detached no reflog", |worktree| {
            git(worktree, &["checkout", "-q", "--detach"]);
            std::fs::write(worktree.join("detached.txt"), "only HEAD\n").unwrap();
            git(worktree, &["add", "detached.txt"]);
            git(worktree, &["commit", "-qm", "detached work"]);
            detached = git(worktree, &["rev-parse", "HEAD"]).trim().to_string();
        });
        let git_dir = crate::git::checked_worktree_git_dir(&worktree)
            .unwrap()
            .unwrap();
        std::fs::remove_file(git_dir.join("logs/HEAD")).unwrap();
        let recorder = crate::phase_telemetry::Recorder::default();
        {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            broker
                .pin_reflog_only_commits(session.id, &worktree)
                .unwrap();
        }
        assert_eq!(
            recorder
                .totals()
                .1
                .get("checkout_trash_worktree_reflog_missing")
                .copied(),
            Some(1)
        );
        git(
            broker.main_root(),
            &[
                "rev-parse",
                "--verify",
                &format!("refs/aethyme/retired/{}/{detached}", session.id),
            ],
        );
    }

    /// More candidates than the per-checkout cap keeps the checkout, with
    /// the reason remembered for `broker gc plan --json`.
    #[test]
    fn too_many_pin_candidates_keep_the_checkout_with_the_reason() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "huge reflog", |_| {});
        let git_dir = crate::git::checked_worktree_git_dir(&worktree)
            .unwrap()
            .unwrap();
        let mut reflog = std::fs::read_to_string(git_dir.join("logs/HEAD")).unwrap_or_default();
        for index in 0..=RETIRE_PIN_CANDIDATES_MAX {
            let id = format!("{:040x}", index + 1);
            reflog.push_str(&format!(
                "{id} {id} Retire test <r@example.invalid> 0 +0000\tfake\n"
            ));
        }
        std::fs::write(git_dir.join("logs/HEAD"), reflog).unwrap();

        let report = run(&mut broker);

        assert_eq!(report.kept.get("trash_refused"), Some(&1), "{report:#?}");
        assert!(worktree.join("base.txt").exists());
        let keep = broker
            .checkout_retirement_keep_report()
            .into_iter()
            .find(|keep| keep.session_id == session.id)
            .expect("remembered");
        assert!(
            keep.detail
                .as_deref()
                .is_some_and(|detail| detail.contains(&RETIRE_PIN_CANDIDATES_MAX.to_string())),
            "{keep:?}"
        );
    }

    /// A kept checkout is not planned again every lap; a change to its
    /// index lifts the remembered keep at once.
    #[test]
    fn a_remembered_keep_skips_until_the_checkout_changes() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "remembered", |worktree| {
            std::fs::write(worktree.join("notes.txt"), "more than one byte\n").unwrap();
        });
        let _cap = retire_archive_cap_for_test(1);
        let first = run(&mut broker);
        assert_eq!(first.kept.get("archive_too_large"), Some(&1), "{first:#?}");

        let second = run(&mut broker);
        assert_eq!(second.keep_memo_skips, 1, "{second:#?}");
        assert_eq!(second.examined, 0);

        git(&worktree, &["add", "notes.txt"]);
        let third = run(&mut broker);
        assert_eq!(third.keep_memo_skips, 0, "{third:#?}");
        assert_eq!(third.examined, 1);
        assert!(broker.checkout_retirement_owns(session.id));
    }

    /// A session whose checkout is in the trash but whose cleaned record was
    /// lost is recorded as cleaned, not skipped forever.
    #[test]
    fn a_trashed_session_missing_its_cleaned_record_is_reconciled() {
        let (owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "lost record", |_| {});
        std::fs::rename(&worktree, owner.path().join("gone")).unwrap();
        broker
            .record_checkout_trash_row(
                &owner.path().join("pool/.aethyme-checkout-trash"),
                "0000000000001-1-1-1",
                crate::checkout_trash::TrashRow {
                    device: 1,
                    inode: 1,
                    session_id: session.id,
                    failures: 0,
                    pending: false,
                    creator_pid: 0,
                    creator_started_ms: 0,
                    stripped: false,
                },
            )
            .unwrap();
        let report = run(&mut broker);
        assert_eq!(report.reconciled_trashed, 1, "{report:#?}");
        assert!(is_cleaned(&broker, session.id));
    }

    /// The deleter never runs without the host-wide lock: held elsewhere,
    /// or unavailable, it deletes nothing (no weaker fallback lock).
    #[test]
    fn the_deleter_never_runs_without_the_host_lock() {
        let (owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "locked out", |_| {});
        let current = broker.store_ref().session(session.id).unwrap();
        let _lane = super::RetirementLaneScope::enter();
        broker
            .remove_checkout_through_trash(&current, &worktree, true)
            .unwrap();
        let lock = owner.path().join(crate::checkout_trash::DELETER_LOCK_FILE);
        let held = crate::checkout_trash::try_deleter_lock(&lock)
            .unwrap()
            .unwrap();
        let far = Instant::now() + Duration::from_secs(600);

        let busy = broker.drain_checkout_trash_with_lock(Some(&lock), far);
        let unavailable = broker.drain_checkout_trash_with_lock(None, far);
        drop(held);
        let drained = broker.drain_checkout_trash_with_lock(Some(&lock), far);

        assert_eq!((busy.entries_removed, busy.entries_pending), (0, 1));
        assert_eq!(
            (unavailable.entries_removed, unavailable.entries_pending),
            (0, 1)
        );
        assert_eq!(drained.entries_removed, 1, "{drained:?}");
    }

    /// An automatic close with time left retires the checkout inline.
    #[test]
    fn a_close_with_time_left_retires_inline() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "inline", |_| {});
        broker
            .retire_inline_after_close(session.id, Some(Instant::now() + Duration::from_secs(600)));
        assert!(!worktree.exists());
        assert!(is_cleaned(&broker, session.id));
    }

    /// `start --reuse` on a retired checkout recreates it from the session
    /// branch; one with archived leftovers is refused with the archive.
    #[test]
    fn a_retired_checkout_is_recreated_for_reuse() {
        let (_owner, _storage, mut broker) = fixture();
        let (clean, clean_path) = closed(&mut broker, "reuse clean", |_| {});
        let (dirty, dirty_path) = closed(&mut broker, "reuse dirty", |worktree| {
            std::fs::write(worktree.join("notes.txt"), "leftover\n").unwrap();
        });
        let report = run(&mut broker);
        assert_eq!(
            report.retired_clean + report.retired_archived,
            2,
            "{report:#?}"
        );
        assert!(is_cleaned(&broker, clean.id) && is_cleaned(&broker, dirty.id));

        assert!(broker.recreate_retired_checkout(&clean_path).unwrap());
        assert!(clean_path.join("base.txt").exists());
        let error = broker
            .recreate_retired_checkout(&dirty_path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("README.md"), "{error}");
        assert!(!dirty_path.exists());
    }

    /// Recreating a retired checkout takes the lifecycle claim: while
    /// another holder has it, nothing is created.
    #[test]
    fn recreating_a_retired_checkout_takes_the_lifecycle_claim() {
        let (_owner, _storage, mut broker) = fixture();
        let (_session, path) = closed(&mut broker, "claimed", |_| {});
        assert_eq!(run(&mut broker).retired_clean, 1);
        let held = broker
            .try_claim_worktree_lifecycle("retire")
            .unwrap()
            .unwrap();
        let error = broker
            .recreate_retired_checkout(&path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("lifecycle"), "{error}");
        assert!(!path.exists());
        drop(held);
        assert!(broker.recreate_retired_checkout(&path).unwrap());
    }

    /// A retired-commit pin expires once another ref holds its commit, is
    /// counted in status meanwhile, and is never pruned otherwise.
    #[test]
    fn retired_commit_pins_expire_only_when_held_elsewhere_or_old() {
        let (_owner, _storage, mut broker) = fixture();
        let mut lost = String::new();
        let (session, _) = closed(&mut broker, "pinned", |worktree| {
            std::fs::write(worktree.join("work.txt"), "only in the reflog\n").unwrap();
            git(worktree, &["add", "work.txt"]);
            git(worktree, &["commit", "-qm", "soon unreachable"]);
            lost = git(worktree, &["rev-parse", "HEAD"]).trim().to_string();
            git(worktree, &["reset", "-q", "--hard", "HEAD~1"]);
        });
        assert_eq!(run(&mut broker).retired_clean, 1);
        let pin = format!("refs/aethyme/retired/{}/{lost}", session.id);
        let root = broker.main_root().to_path_buf();
        assert_eq!(
            broker.prune_retired_pins().unwrap(),
            1,
            "kept while only it holds the commit"
        );
        assert_eq!(broker.checkout_retirement_progress().retired_pins, 1);
        let view = broker.status(crate::clock::epoch_ms()).unwrap();
        assert!(
            view.advice
                .iter()
                .any(|advice| advice.id == "cleanup.retired-pins")
        );

        // Another local branch (a session's, deleted later) is not enough.
        git(&root, &["branch", "keeps-it-for-now", &lost]);
        assert_eq!(broker.prune_retired_pins().unwrap(), 1);
        assert!(git_status(&root, &["rev-parse", "--verify", &pin]));
        // A remote-tracking ref holds it: the pin expires.
        git(
            &root,
            &["update-ref", "refs/remotes/origin/keeps-it", &lost],
        );
        assert_eq!(broker.prune_retired_pins().unwrap(), 0);
        assert!(!git_status(&root, &["rev-parse", "--verify", &pin]));
    }

    /// Inline retirement at close takes only the clean path: a checkout that
    /// needs an archive is left to the lane.
    #[test]
    fn inline_retirement_leaves_archives_to_the_lane() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "needs an archive", |worktree| {
            std::fs::write(worktree.join("notes.txt"), "leftover\n").unwrap();
        });
        broker
            .retire_inline_after_close(session.id, Some(Instant::now() + Duration::from_secs(600)));
        assert!(worktree.join("notes.txt").exists());
        assert!(!is_cleaned(&broker, session.id));
        assert!(archives(&broker, session.id).is_empty());
        let report = run(&mut broker);
        assert_eq!(report.retired_archived, 1, "the lane archives it");
    }

    /// Pins are checked a bounded number per run, from a cursor: every pin
    /// is reached across runs.
    #[test]
    fn retired_pins_are_checked_in_bounded_batches() {
        let (_owner, _storage, broker) = fixture();
        let root = broker.main_root().to_path_buf();
        let base = git(&root, &["rev-parse", "origin/main"]).trim().to_string();
        for session in [901, 902, 903] {
            git(
                &root,
                &[
                    "update-ref",
                    &format!("refs/aethyme/retired/{session}/{base}"),
                    &base,
                ],
            );
        }
        assert_eq!(
            broker.prune_retired_pins().unwrap(),
            1,
            "two checked this run"
        );
        assert_eq!(
            broker.prune_retired_pins().unwrap(),
            0,
            "the cursor reaches the third"
        );
    }

    /// A pending row nothing could confirm (its mover's start time unknown)
    /// is confirmed by the explicit gc path once a day old, when its entry
    /// still matches and no open session uses the path; then it is drained.
    #[test]
    fn gc_confirms_a_stale_unconfirmable_trash_row() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "unconfirmable", |_| {});
        let pool = worktree.parent().unwrap().to_path_buf();
        let common = broker.main_git_common_dir().unwrap();
        let repo = crate::git::GitRepo::discover(broker.main_root()).unwrap();
        let mut pending = None;
        let trashed = crate::checkout_trash::move_worktree_to_trash(
            &repo,
            &common,
            &pool,
            &worktree,
            session.id,
            &mut |_, _, row| {
                pending = Some(row);
                Ok(())
            },
        )
        .unwrap();
        let mut row = pending.unwrap();
        row.creator_started_ms = 0;
        let old = format!("0000000000001-{}-1-1", session.id);
        std::fs::rename(trashed.path(), trashed.trash_dir.join(&old)).unwrap();
        broker
            .record_checkout_trash_row(&trashed.trash_dir, &old, row)
            .unwrap();

        let report = broker.drain_checkout_trash(Instant::now() + Duration::from_secs(600));
        assert_eq!(report.stale_pending, vec![old.clone()], "{report:?}");
        assert_eq!(report.entries_removed, 0);

        assert_eq!(broker.confirm_stale_pending_trash_rows(), 1);
        let report = broker.drain_checkout_trash(Instant::now() + Duration::from_secs(600));
        assert_eq!(report.entries_removed, 1, "{report:?}");
    }

    /// A session whose cleaned record was lost after its checkout was
    /// trashed and drained is reconciled from the tombstone.
    #[test]
    fn a_lost_cleaned_record_is_reconciled_after_the_drain() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "tombstone", |_| {});
        let first = run(&mut broker);
        assert_eq!(first.retired_clean, 1, "{first:#?}");
        assert!(!worktree.exists());
        assert!(broker.checkout_trash_rows().is_empty(), "drained");
        // The cleaned record is lost, as a database error would leave it.
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET cleanup_state = 'closed', cleanup_completed_at = NULL WHERE id = ?1",
                [session.id],
            )
            .unwrap();
        broker.mark_session_in_set(CHECKOUT_RETIREMENT_TRASHED_SESSIONS_KEY, session.id, true);

        let second = run(&mut broker);
        assert_eq!(second.reconciled_trashed, 1, "{second:#?}");
        assert!(is_cleaned(&broker, session.id));
    }

    /// Ignore `node_modules/` for every checkout of the fixture without a
    /// tracked change (the common directory's `info/exclude`).
    fn exclude_node_modules(broker: &crate::Broker) {
        let info = broker.main_root().join(".git/info");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::write(info.join("exclude"), "node_modules/\n").unwrap();
    }

    /// A closed checkout holding an ignored top-level and a nested
    /// `node_modules`, an unignored `vendor` directory and an untracked note.
    fn closed_with_dependencies(
        broker: &mut crate::Broker,
        label: &str,
    ) -> (crate::Session, PathBuf) {
        exclude_node_modules(broker);
        // Populated after the close: closing already reclaims build output
        // and strips; these tests exercise the lane's pass.
        let (session, worktree) = closed(broker, label, |_| {});
        for dir in ["node_modules/left-pad", "packages/app/node_modules/dep"] {
            std::fs::create_dir_all(worktree.join(dir)).unwrap();
            std::fs::write(worktree.join(dir).join("index.js"), "module.exports = 1;\n").unwrap();
        }
        for dir in ["node_modules", "packages/app/node_modules"] {
            std::fs::write(worktree.join(dir).join(".package-lock.json"), "{}\n").unwrap();
        }
        std::fs::create_dir_all(worktree.join("vendor")).unwrap();
        std::fs::write(worktree.join("vendor/keep.js"), "kept\n").unwrap();
        (session, worktree)
    }

    fn strip_pass(broker: &crate::Broker) -> super::super::strip::CheckoutStripReport {
        let far = Instant::now() + Duration::from_secs(600);
        let mut run = broker
            .begin_checkout_retirement_run(far)
            .unwrap()
            .expect("open paths resolve");
        broker.strip_closed_checkouts(&mut run, far, super::super::strip::STRIP_CHECKOUTS_PER_RUN)
    }

    /// Regenerable directories leave a closed checkout by rename, top-level
    /// and nested, while tracked and unignored content stays; the entries
    /// are recorded as stripped (never as the checkout itself) and the
    /// deleter removes them and counts their bytes.
    #[test]
    fn a_closed_checkout_is_stripped_of_regenerable_directories() {
        let (owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed_with_dependencies(&mut broker, "strip");

        let report = strip_pass(&broker);

        assert_eq!(report.directories_stripped, 2, "{report:#?}");
        assert_eq!(report.checkouts_stripped, 1, "{report:#?}");
        assert!(!worktree.join("node_modules").exists());
        assert!(!worktree.join("packages/app/node_modules").exists());
        assert!(worktree.join("packages/app").is_dir());
        assert!(
            worktree.join("vendor/keep.js").exists(),
            "unignored content stays"
        );
        assert!(worktree.join("base.txt").exists(), "tracked content stays");
        let rows = broker.checkout_trash_rows();
        let stripped = rows
            .values()
            .flat_map(|entries| entries.values())
            .filter(|row| row.session_id == session.id && row.stripped && !row.pending)
            .count();
        assert_eq!(stripped, 2, "{rows:?}");
        assert!(
            !broker.checkout_retirement_owns(session.id),
            "a stripped directory never stands for the checkout"
        );
        for dir in rows.keys() {
            assert!(
                !dir.starts_with(&worktree),
                "the trash is outside the checkout"
            );
        }
        let lock = owner.path().join(crate::checkout_trash::DELETER_LOCK_FILE);
        let drained = broker
            .drain_checkout_trash_with_lock(Some(&lock), Instant::now() + Duration::from_secs(600));
        assert_eq!(drained.stripped_entries_removed, 2, "{drained:?}");
        assert!(drained.stripped_bytes > 0, "{drained:?}");
        // The pass remembers the checkout and does not list it again.
        let again = strip_pass(&broker);
        assert_eq!(again.checkouts_examined, 0, "{again:#?}");
    }

    /// An open session's checkout is never stripped, nor a closed one a
    /// process still uses.
    #[test]
    fn open_or_used_checkouts_are_never_stripped() {
        let (_owner, _storage, mut broker) = fixture();
        exclude_node_modules(&broker);
        let open = broker.start_worktree("still open", None).unwrap();
        let open_worktree = PathBuf::from(&open.worktree_path);
        std::fs::create_dir_all(open_worktree.join("node_modules/dep")).unwrap();
        std::fs::write(open_worktree.join("node_modules/dep/index.js"), "x\n").unwrap();
        let (_used, used_worktree) = closed_with_dependencies(&mut broker, "in use");
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "node".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: std::fs::canonicalize(&used_worktree).unwrap(),
            }]),
        ));

        let report = strip_pass(&broker);

        assert_eq!(report.directories_stripped, 0, "{report:#?}");
        assert_eq!(
            report.refused.get("process_using_checkout"),
            Some(&1),
            "{report:#?}"
        );
        assert!(open_worktree.join("node_modules/dep/index.js").exists());
        assert!(
            used_worktree
                .join("node_modules/left-pad/index.js")
                .exists()
        );
    }

    /// The retirement run strips first, then judges the small tree that is
    /// left: the checkout retires clean in the same run.
    #[test]
    fn a_run_strips_then_retires_the_remainder() {
        let (_owner, _storage, mut broker) = fixture();
        exclude_node_modules(&broker);
        let (session, worktree) = closed(&mut broker, "strip then retire", |_| {});
        std::fs::create_dir_all(worktree.join("node_modules/dep")).unwrap();
        std::fs::write(worktree.join("node_modules/dep/index.js"), "x\n").unwrap();
        std::fs::write(
            worktree.join("node_modules/.modules.yaml"),
            "layoutVersion: 5\n",
        )
        .unwrap();

        let report = run(&mut broker);

        assert_eq!(report.strip.directories_stripped, 1, "{report:#?}");
        assert_eq!(report.retired_clean, 1, "{report:#?}");
        assert!(!worktree.exists());
        assert!(is_cleaned(&broker, session.id));
        let progress = broker.checkout_strip_progress();
        assert_eq!(progress.last_directories_stripped, 1, "{progress:#?}");
        assert!(progress.backlog.observed_at_ms > 0, "{progress:#?}");
    }

    /// `finish` strips the closed checkout through the same rules.
    #[test]
    fn finish_strips_a_closed_checkout() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed_with_dependencies(&mut broker, "finish strip");

        let stripped = broker.strip_checkout_at_finish(session.id).unwrap();

        assert_eq!(stripped, 2);
        assert!(!worktree.join("node_modules").exists());
        assert!(worktree.join("vendor/keep.js").exists());
    }

    /// An inspection that failed or timed out is retried on the next run,
    /// never remembered as a plan refusal.
    #[test]
    fn an_inspection_failure_is_retried_not_remembered() {
        let timed_out = BrokerOpError::CleanupResolveRefused {
            id: 7,
            reason: "its worktree is inspection_failed (cleanup inspection failed: git -c core.fsmonitor=false -c core.untrackedCache=false status --porcelain=v1 did not finish within 3s and was killed), which resolve does not archive".into(),
        };
        let refused = BrokerOpError::CleanupResolveRefused {
            id: 7,
            reason: "ignored path x is a special file, which an archive cannot preserve".into(),
        };
        assert!(inspection_failed(&timed_out));
        assert!(!inspection_failed(&refused));
        let enriched = BrokerOpError::CleanupResolveRefused {
            id: 7,
            reason: "its worktree is checked (git status timed out after 3.0 s (limit 3.0 s, outer command deadline) in a/b)".into(),
        };
        assert!(inspection_failed(&enriched));
        assert!(!remembered_keep("inspection_failed"));
        assert!(remembered_keep("plan_refused"));
        assert_eq!(
            kept_counter("inspection_failed"),
            "checkout_retirement_kept_inspection_failed"
        );
    }

    /// One `git worktree list` serves every lookup of a run.
    #[test]
    fn a_run_lists_worktrees_once() {
        let (_owner, _storage, broker) = fixture();
        let repo = broker.repo_handle();
        let scope = crate::git::WorktreeInventoryRunScope::enter();
        let first = repo.worktree_inventory_for_run().unwrap();
        let root = broker.main_root().to_path_buf();
        git(&root, &["worktree", "add", "-q", "-b", "later", "../later"]);
        let second = repo.worktree_inventory_for_run().unwrap();
        assert_eq!(first, second, "the run reuses its listing");
        drop(scope);
        assert_eq!(repo.worktree_inventory().unwrap().len(), first.len() + 1);
        assert_eq!(
            repo.worktree_inventory_for_run().unwrap().len(),
            first.len() + 1,
            "outside a run scope nothing is cached"
        );
    }

    /// A folder left behind by a session already recorded as cleaned is
    /// stripped, judged and retired by the leftover lap, and counted.
    #[test]
    fn a_cleaned_sessions_leftover_folder_is_retired() {
        let (_owner, _storage, mut broker) = fixture();
        exclude_node_modules(&broker);
        let (session, worktree) = closed(&mut broker, "leftover", |_| {});
        std::fs::create_dir_all(worktree.join("node_modules/dep")).unwrap();
        std::fs::write(worktree.join("node_modules/dep/index.js"), "x\n").unwrap();
        std::fs::write(
            worktree.join("node_modules/.modules.yaml"),
            "layoutVersion: 5\n",
        )
        .unwrap();
        broker
            .store()
            .set_session_status(session.id, crate::SessionStatus::Cleaned, None)
            .unwrap();
        assert!(is_cleaned(&broker, session.id));
        assert!(worktree.exists(), "the folder outlived its cleaned record");

        let report = run(&mut broker);

        assert_eq!(report.strip.directories_stripped, 1, "{report:#?}");
        assert_eq!(report.retired_cleaned_leftovers, 1, "{report:#?}");
        assert!(!worktree.exists());
        let progress = broker.checkout_strip_progress();
        let today = progress.days.values().last().expect("a day recorded");
        assert_eq!(today.cleaned_leftovers_retired, 1, "{progress:#?}");
        assert!(today.sessions_created >= 1, "{progress:#?}");
        assert!(today.checkouts_retired >= 1, "{progress:#?}");
    }

    /// Under the claim, a path another open session now names keeps the
    /// checkout.
    #[test]
    fn a_path_reopened_by_another_session_is_kept() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "reopened", |_| {});
        // Cleaned, so the path is free for another live registration (the
        // live-worktree index is partial on cleaned rows).
        broker
            .store()
            .set_session_status(session.id, crate::SessionStatus::Cleaned, None)
            .unwrap();
        let current = broker.store_ref().session(session.id).unwrap();
        assert_eq!(broker.reused_path_keep(&current, &worktree, true), None);
        let other = broker.start_worktree("other", None).unwrap();
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET worktree_path = ?1 WHERE id = ?2",
                rusqlite::params![session.worktree_path, other.id],
            )
            .unwrap();
        assert_eq!(
            broker.reused_path_keep(&current, &worktree, true),
            Some("open_session")
        );
    }

    /// A folder born after the session closed is not the one it left.
    #[test]
    fn a_folder_created_after_the_close_is_kept() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "recreated", |_| {});
        let old = crate::clock::epoch_ms() - 10 * 60_000;
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET created_at = ?1, closed_at = ?2 WHERE id = ?3",
                rusqlite::params![old, old + 1_000, session.id],
            )
            .unwrap();
        let current = broker.store_ref().session(session.id).unwrap();
        assert_eq!(
            broker.reused_path_keep(&current, &worktree, false),
            Some("created_after_close")
        );
    }

    /// A start holding the folder's reservation keeps it, in the strip pass
    /// too (the reservation is asked only under the claim).
    #[test]
    fn a_held_start_reservation_keeps_the_folder() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed_with_dependencies(&mut broker, "reserved");
        let name = worktree.file_name().unwrap().to_str().unwrap().to_string();
        // A start in another process holding its real reservation.
        let held = crate::start_reservation::hold_foreign_reservation(
            broker.main_root(),
            &name,
            &worktree,
            &format!("agent/{name}"),
            "base",
        );
        let current = broker.store_ref().session(session.id).unwrap();
        assert_eq!(
            broker.reused_path_keep(&current, &worktree, false),
            Some("start_reserved")
        );

        let report = strip_pass(&broker);

        assert_eq!(
            report.refused.get("start_reserved"),
            Some(&1),
            "{report:#?}"
        );
        assert!(worktree.join("node_modules/left-pad/index.js").exists());
        drop(held);
        assert_eq!(broker.reused_path_keep(&current, &worktree, false), None);
    }

    /// Record another session at `session`'s path, in `cleanup_state`, with
    /// the same times: a path reused by a later task.
    fn record_again_at_path(
        broker: &crate::Broker,
        session: &crate::Session,
        cleanup_state: &str,
    ) -> i64 {
        let connection = broker.store_ref().connection();
        connection
            .execute(
                "INSERT INTO sessions (worktree_path, branch, origin, status, created_at,
                    updated_at, last_activity_at, cleanup_state, closed_at, cleanup_completed_at)
                 SELECT worktree_path, branch || '-again', origin, 'cleaned', created_at,
                    updated_at, last_activity_at, ?2, closed_at, cleanup_completed_at
                 FROM sessions WHERE id = ?1",
                rusqlite::params![session.id, cleanup_state],
            )
            .unwrap();
        connection.last_insert_rowid()
    }

    /// The strip spends its budget on folders that exist: thousands of
    /// recorded sessions whose folders are gone (and closed earlier) cost
    /// one pool listing, and the backlog is counted only after every
    /// checkout was stripped, so counting never eats the stripping budget
    /// (on the busiest repository the count took 7 to 65 s and ran first,
    /// and no pass ever reached a checkout).
    #[test]
    fn the_strip_starts_from_existing_folders_and_counts_the_backlog_last() {
        let (_owner, _storage, mut broker) = fixture();
        let mut worktrees = Vec::new();
        for index in 0..5 {
            let (_session, worktree) =
                closed_with_dependencies(&mut broker, &format!("pool member {index}"));
            worktrees.push(worktree);
        }
        let pool = worktrees[0]
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        broker
            .store()
            .connection()
            .execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000)
                 INSERT INTO sessions (worktree_path, branch, origin, status, created_at,
                    updated_at, last_activity_at, cleanup_state, closed_at, cleanup_completed_at)
                 SELECT ?1 || '/gone-' || i, 'agent/gone-' || i, 'spawned', 'cleaned', 1000,
                    1000, 1000, 'cleaned', 1000 + i, 1000 + i FROM n",
                rusqlite::params![pool],
            )
            .unwrap();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(None));
        let hook_seen = seen.clone();
        let hook_worktrees = worktrees.clone();
        super::super::strip::set_before_backlog_hook(Some(Box::new(move || {
            *hook_seen.borrow_mut() = Some(
                hook_worktrees
                    .iter()
                    .filter(|worktree| worktree.join("node_modules").exists())
                    .count(),
            );
        })));

        let report = strip_pass(&broker);
        super::super::strip::set_before_backlog_hook(None);

        assert_eq!(
            *seen.borrow(),
            Some(0),
            "every checkout was stripped before the backlog count began"
        );
        assert_eq!(report.folders_found, 5, "{report:#?}");
        assert_eq!(report.checkouts_examined, 5, "{report:#?}");
        assert_eq!(report.checkouts_stripped, 5, "{report:#?}");
        assert_eq!(report.directories_stripped, 10, "{report:#?}");
        assert_eq!(report.backlog.closed_folders, 5, "{report:#?}");
        assert!(!report.more_remaining, "{report:#?}");
    }

    /// A sub-project's virtual environment (`backend/.venv`) is found and
    /// stripped with its witness, a marker file the repository declared
    /// under `[cleanup] venv_markers` included. An undeclared dotfile keeps
    /// its environment, and so does `.env` whatever is declared; the names
    /// (only the names) are reported for `broker status`.
    #[test]
    fn a_nested_sub_project_venv_is_stripped_with_its_witness() {
        let (_owner, _storage, mut broker) = fixture();
        let info = broker.main_root().join(".git/info");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::write(info.join("exclude"), ".venv/\n").unwrap();
        std::fs::write(
            broker.main_root().join(".aethyme/config.toml"),
            "[cleanup]\nvenv_markers = [\".env-check.json\"]\n",
        )
        .unwrap();
        let (_session, worktree) = closed(&mut broker, "nested venv", |_| {});
        for project in ["backend", "worker", "api"] {
            let venv = worktree.join(project).join(".venv");
            std::fs::create_dir_all(venv.join("lib/python3.12/site-packages/dep")).unwrap();
            std::fs::create_dir_all(venv.join("bin")).unwrap();
            std::fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
            std::fs::write(venv.join(".gitignore"), "*\n").unwrap();
            std::fs::write(venv.join(".env-check.json"), "{}\n").unwrap();
        }
        std::fs::write(worktree.join("worker/.venv/.tool-state"), "x\n").unwrap();
        std::fs::write(worktree.join("api/.venv/.env"), "TOKEN=secret\n").unwrap();

        let report = strip_pass(&broker);

        assert_eq!(report.candidates_found, 3, "{report:#?}");
        assert_eq!(report.directories_stripped, 1, "{report:#?}");
        assert_eq!(
            report.refused.get("unexpected_content"),
            Some(&2),
            "{report:#?}"
        );
        assert!(!worktree.join("backend/.venv").exists());
        assert!(worktree.join("backend").is_dir());
        assert!(worktree.join("worker/.venv/.tool-state").exists());
        assert!(worktree.join("api/.venv/.env").exists());
        assert_eq!(
            report.venv_unexpected.iter().cloned().collect::<Vec<_>>(),
            [".env", ".tool-state"],
            "{report:#?}"
        );
        let progress = broker.next_checkout_strip_progress(
            &super::super::strip::CheckoutStripProgress::default(),
            &CheckoutRetirementReport {
                strip: report.clone(),
                ..CheckoutRetirementReport::default()
            },
        );
        assert_eq!(
            progress.venv_unexpected.keys().cloned().collect::<Vec<_>>(),
            [".env", ".tool-state"]
        );
    }

    /// A checkout kept only for a transient reason (here every Git probe
    /// fails) is backed off: the next pass skips it without counting it
    /// toward the cap, so a newer checkout is still stripped; once the
    /// backoff has passed and the probe works, it is stripped and the
    /// backoff cleared.
    #[test]
    fn a_transient_keep_backs_off_without_using_the_cap() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed_with_dependencies(&mut broker, "probe fails");
        super::super::strip::set_probes_fail_for_tests(true);
        let far = Instant::now() + Duration::from_secs(600);
        let pass = |broker: &crate::Broker, cap: usize| {
            let mut run = broker
                .begin_checkout_retirement_run(far)
                .unwrap()
                .expect("open paths resolve");
            broker.strip_closed_checkouts(&mut run, far, cap)
        };

        let first = pass(&broker, 1);

        assert_eq!(first.refused.get("probe_failed"), Some(&2), "{first:#?}");
        assert_eq!(first.backoff_set, 1, "{first:#?}");
        let backoff: std::collections::BTreeMap<i64, super::super::strip::StripBackoff> =
            broker.meta_json(super::super::strip::CHECKOUT_STRIP_BACKOFF_KEY);
        assert_eq!(backoff[&session.id].failures, 1, "{backoff:?}");

        super::super::strip::set_probes_fail_for_tests(false);
        let (_newer, newer_worktree) = closed_with_dependencies(&mut broker, "newer");
        let second = pass(&broker, 1);

        assert_eq!(second.skipped_backoff, 1, "{second:#?}");
        assert_eq!(second.checkouts_stripped, 1, "{second:#?}");
        assert!(!newer_worktree.join("node_modules").exists());
        assert!(worktree.join("node_modules").exists());

        broker
            .update_meta_json(
                super::super::strip::CHECKOUT_STRIP_BACKOFF_KEY,
                |stored: &mut std::collections::BTreeMap<
                    i64,
                    super::super::strip::StripBackoff,
                >| {
                    for entry in stored.values_mut() {
                        entry.until_ms = 0;
                    }
                },
            )
            .unwrap();
        let third = pass(&broker, 1);

        assert_eq!(third.checkouts_stripped, 1, "{third:#?}");
        assert!(!worktree.join("node_modules").exists());
        let backoff: std::collections::BTreeMap<i64, super::super::strip::StripBackoff> =
            broker.meta_json(super::super::strip::CHECKOUT_STRIP_BACKOFF_KEY);
        assert!(backoff.is_empty(), "{backoff:?}");
    }

    /// Every outcome of a pass is a phase counter by reason, beside the
    /// discovery, proof and move timings, so the next reading shows what
    /// the lane did and where its time went.
    #[test]
    fn every_strip_outcome_is_counted_by_reason() {
        let (_owner, _storage, mut broker) = fixture();
        let (_session, _worktree) = closed_with_dependencies(&mut broker, "counted");
        let (_kept, kept_worktree) = closed(&mut broker, "counted kept", |_| {});
        std::fs::create_dir_all(kept_worktree.join("node_modules/dep")).unwrap();
        let (_empty, _empty_worktree) = closed(&mut broker, "counted empty", |_| {});
        let recorder = crate::phase_telemetry::Recorder::default();
        let installed = crate::phase_telemetry::install(recorder.clone());

        let report = strip_pass(&broker);
        drop(installed);

        let (_millis, counts) = recorder.totals();
        let count = |name: &str| counts.get(name).copied().unwrap_or(0);
        assert_eq!(count("checkout_strip_examined"), 3, "{counts:?}");
        assert_eq!(count("checkout_strip_stripped_checkout"), 1, "{counts:?}");
        assert_eq!(count("checkout_strip_stripped_directory"), 2, "{counts:?}");
        assert_eq!(
            count("checkout_strip_stripped_node_modules"),
            2,
            "{counts:?}"
        );
        assert_eq!(count("checkout_strip_found_node_modules"), 3, "{counts:?}");
        assert_eq!(
            count("checkout_strip_refused_witness_missing"),
            1,
            "{counts:?}"
        );
        assert_eq!(count("checkout_strip_nothing_found"), 1, "{counts:?}");
        for span in [
            "checkout_strip_folder_listing",
            "checkout_strip_preconditions",
            "checkout_strip_discover",
            "checkout_strip_prove",
            "checkout_strip_move",
            "checkout_strip_backlog",
        ] {
            assert!(count(span) > 0, "{span}: {counts:?}");
        }
        assert_eq!(
            report.refused.get("witness_missing"),
            Some(&1),
            "{report:#?}"
        );
        let attached = recorder.attachments();
        assert_eq!(
            attached["checkout_strip"]["directories_stripped"],
            serde_json::json!(2),
            "{attached:?}"
        );
    }

    /// A folder whose recorded sessions are all cleaned is retired through
    /// any of them: no row is kept as superseded by another cleaned one.
    #[test]
    fn a_path_with_several_cleaned_rows_is_retired() {
        let (_owner, _storage, mut broker) = fixture();
        exclude_node_modules(&broker);
        let (session, worktree) = closed(&mut broker, "reused cleaned", |_| {});
        std::fs::create_dir_all(worktree.join("node_modules/dep")).unwrap();
        std::fs::write(
            worktree.join("node_modules/.modules.yaml"),
            "layoutVersion: 5\n",
        )
        .unwrap();
        broker
            .store()
            .set_session_status(session.id, crate::SessionStatus::Cleaned, None)
            .unwrap();
        let again = record_again_at_path(&broker, &session, "cleaned");
        let first = broker.store_ref().session(session.id).unwrap();
        let second = broker.store_ref().session(again).unwrap();
        assert_eq!(broker.reused_path_keep(&first, &worktree, true), None);
        assert_eq!(broker.reused_path_keep(&second, &worktree, true), None);

        let report = run(&mut broker);

        assert_eq!(report.strip.directories_stripped, 1, "{report:#?}");
        assert_eq!(report.retired_cleaned_leftovers, 1, "{report:#?}");
        assert_eq!(report.kept.get("leftover_superseded"), None, "{report:#?}");
        assert!(!worktree.exists());
    }

    /// A cleaned row never takes a folder from a closed row at the same
    /// path: the closed session owns it and its own judgement (plan and
    /// archive) retires it.
    #[test]
    fn a_path_with_a_closed_row_keeps_the_cleaned_leftover() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "reused closed", |_| {});
        let again = record_again_at_path(&broker, &session, "cleaned");
        let closed_row = broker.store_ref().session(session.id).unwrap();
        let cleaned_row = broker.store_ref().session(again).unwrap();
        assert_eq!(closed_row.cleanup_state, crate::SessionCleanupState::Closed);
        assert_eq!(
            broker.reused_path_keep(&cleaned_row, &worktree, false),
            Some("leftover_superseded")
        );
        assert_eq!(broker.reused_path_keep(&closed_row, &worktree, false), None);

        let report = run(&mut broker);

        assert_eq!(report.retired_clean, 1, "{report:#?}");
        assert_eq!(report.retired_cleaned_leftovers, 0, "{report:#?}");
        assert!(!worktree.exists());
        assert!(is_cleaned(&broker, session.id));
    }

    /// Open paths that take most of the window (0.8.122 on the busiest
    /// repository: 89 s) no longer leave the laps nothing: each phase gets a
    /// floor of the lane's time, and the closed lap still retires.
    #[test]
    fn a_slow_open_path_resolution_leaves_the_laps_their_share() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "slow open paths", |_| {});
        super::AFTER_OPEN_PATHS.with(|cell| {
            *cell.borrow_mut() = Some(Box::new(|| {
                std::thread::sleep(Duration::from_secs(7));
            }));
        });

        // A 12-second lane whose open paths take 7 s: the sweep window ends
        // at 8 s, so the old shares (strip to 4 s, closed lap to 6 s) had
        // passed before the laps began.
        let report = broker
            .run_checkout_retirement(Instant::now() + Duration::from_secs(12))
            .unwrap();
        super::AFTER_OPEN_PATHS.with(|cell| *cell.borrow_mut() = None);

        assert_eq!(report.retired_clean, 1, "{report:#?}");
        assert!(!worktree.exists());
        assert!(is_cleaned(&broker, session.id));
    }

    /// The strip pass goes on from where the last one stopped: a checkout
    /// kept every pass (a process uses it) does not take the cap from the
    /// ones after it.
    #[test]
    fn the_strip_pass_resumes_from_its_cursor() {
        let (_owner, _storage, mut broker) = fixture();
        let (_used, used_worktree) = closed_with_dependencies(&mut broker, "cursor used");
        let (_next, next_worktree) = closed_with_dependencies(&mut broker, "cursor next");
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "node".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: std::fs::canonicalize(&used_worktree).unwrap(),
            }]),
        ));
        let far = Instant::now() + Duration::from_secs(600);
        let pass = |broker: &crate::Broker| {
            let mut run = broker
                .begin_checkout_retirement_run(far)
                .unwrap()
                .expect("open paths resolve");
            broker.strip_closed_checkouts(&mut run, far, 1)
        };

        let first = pass(&broker);
        let second = pass(&broker);

        assert_eq!(
            first.refused.get("process_using_checkout"),
            Some(&1),
            "{first:#?}"
        );
        assert_eq!(second.checkouts_stripped, 1, "{second:#?}");
        assert!(!next_worktree.join("node_modules").exists());
        assert!(used_worktree.join("node_modules").exists());
    }

    /// At `finish`, the session's recorded holder is the agent finishing
    /// it: that agent does not keep the checkout it is leaving (0.8.122
    /// refused 3 of 3 as `holder_alive`). Any other live holder still does.
    #[test]
    fn the_finishing_agent_is_not_a_holder_that_keeps_its_checkout() {
        let Some(table) = crate::session_holder::ProcessTable::snapshot() else {
            return;
        };
        let Some(me) = table.process(i64::from(std::process::id())) else {
            return;
        };
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed_with_dependencies(&mut broker, "finishing holder");
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_bound_payload(
                    &me, "fixture", None,
                )),
            )
            .unwrap();

        let other = crate::session_holder::AgentProcess {
            pid: me.pid,
            started: format!("{} (another start)", me.started),
            command: me.command.clone(),
        };
        let kept = broker.strip_checkout_at_finish_as(session.id, Some(&other));
        assert!(
            kept.as_ref()
                .is_err_and(|error| error.contains("holder_alive")),
            "{kept:?}"
        );
        assert!(worktree.join("node_modules").exists());

        let stripped = broker.strip_checkout_at_finish_as(session.id, Some(&me));

        assert_eq!(stripped, Ok(2));
        assert!(!worktree.join("node_modules").exists());
    }
}
