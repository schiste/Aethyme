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
/// Meta key: sessions whose checkout was retired through a recovery
/// archive, with that archive (JSON: session id to its path); see
/// `Broker::session_restorable_archive`.
pub(crate) const ARCHIVED_SESSIONS_KEY: &str = "checkout_retirement.archived_sessions";
/// Recorded archived sessions at most; the lowest ids go first.
const ARCHIVED_SESSIONS_MAX: usize = 65_536;
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
const RETIRE_IDLE_STRIP_MIN_SHARE: std::time::Duration = std::time::Duration::from_secs(15);
/// Next run when nothing is left to do.
const RETIRE_IDLE_RECHECK_MS: i64 = 10 * 60 * 1_000;
/// Next run while a lap or the trash is unfinished.
const RETIRE_BUSY_RECHECK_MS: i64 = 60 * 1_000;
/// Runs in a row without shrinking before `broker status` says so.
pub(crate) const RETIRE_STALLED_RUNS: u32 = 3;
/// Runs in a row in which the trash grew before `broker status` says so.
pub(crate) const TRASH_GROWING_RUNS_ADVICE_AT: u32 = 3;
/// The least time the recovery-archive bound gets in a retirement run, even
/// when the phases before it used the lane (it runs before the drain).
const RETIRE_ARCHIVE_BOUND_RESERVE: std::time::Duration = std::time::Duration::from_secs(20);
/// How long a retirement drain waits for the host-wide deleter lock: a
/// little more than one catch-up slice ([`TRASH_CATCHUP_SLICE`]), which
/// releases it between slices.
const DELETER_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(35);
/// How often that wait tries the lock again: well under the catch-up
/// lane's gap between slices.
const DELETER_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(100);
/// One catch-up slice: the deleter lock is held this long at most, then
/// released for [`TRASH_CATCHUP_GAP`] so another drain can take it.
pub(crate) const TRASH_CATCHUP_SLICE: std::time::Duration = std::time::Duration::from_secs(30);
/// The gap between catch-up slices.
const TRASH_CATCHUP_GAP: std::time::Duration = std::time::Duration::from_secs(1);
/// How long the catch-up lane keeps retrying a deleter lock held elsewhere
/// before it ends.
const TRASH_CATCHUP_BUSY_MAX: std::time::Duration = std::time::Duration::from_secs(120);
/// How long a starting lane retries the lane lock, so a retirement run's
/// momentary probe of it never turns a lane away.
const TRASH_CATCHUP_LANE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
/// Back-to-back laps one worker (or one `gc trash drain`) runs at most,
/// each up to `checkout_trash::TRASH_CATCHUP_MAX`.
pub(crate) const TRASH_CATCHUP_MAX_LAPS: usize = 3;
/// How often a worker's lane retries a host lane held by another
/// repository's lane.
const TRASH_CATCHUP_LANE_RETRY: std::time::Duration = std::time::Duration::from_secs(45);
/// File beside the lane lock in which a waiting lane names its repository.
pub(crate) const TRASH_CATCHUP_WAITER_FILE: &str = "checkout-trash-catchup.waiting";
/// A waiter note older than this is ignored (a waiter refreshes it at
/// every retry).
const TRASH_CATCHUP_WAITER_FRESH: std::time::Duration = std::time::Duration::from_secs(120);
/// File name of the host-wide lock that keeps a second catch-up lane out.
pub(crate) const TRASH_CATCHUP_LOCK_FILE: &str = "checkout-trash-catchup.lock";

/// How long, and how many laps, catch-up runs back to back.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TrashCatchupBounds {
    /// Wall time for all laps together.
    pub(crate) total: std::time::Duration,
    /// Wall time for one lap.
    pub(crate) lap_max: std::time::Duration,
    pub(crate) max_laps: usize,
    /// Keep draining while any entry is deletable (the operator's command),
    /// not only while the trash is behind.
    pub(crate) until_empty: bool,
    /// When the host lane is held by another repository's lane, try again
    /// this often within `total` (the worker), instead of ending.
    pub(crate) lane_retry: Option<std::time::Duration>,
}

impl TrashCatchupBounds {
    /// The worker's lane process: up to [`TRASH_CATCHUP_MAX_LAPS`] laps of
    /// `checkout_trash::TRASH_CATCHUP_MAX`, until no entry is deletable
    /// (the worker starts it whenever the trash holds entries, so all of
    /// its trash deletion runs unthrottled), waiting for another
    /// repository's lane every [`TRASH_CATCHUP_LANE_RETRY`].
    pub(crate) fn worker() -> Self {
        let lap_max = crate::checkout_trash::TRASH_CATCHUP_MAX;
        Self {
            total: lap_max * TRASH_CATCHUP_MAX_LAPS as u32,
            lap_max,
            max_laps: TRASH_CATCHUP_MAX_LAPS,
            until_empty: true,
            lane_retry: Some(TRASH_CATCHUP_LANE_RETRY),
        }
    }
}

/// Whose catch-up lane holds the host lane lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatchupLaneState {
    Free,
    ThisRepository,
    OtherRepository,
}

/// Why a continuing catch-up lane ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrashCatchupEnd {
    /// The trash is no longer behind (or empty).
    CaughtUp,
    /// The lane's wall-clock cap was reached.
    Cap,
    /// Critical memory pressure.
    Pressure,
    /// A slice removed nothing although it had time (entries in use or
    /// failing, or an incomplete open-file observation).
    NoProgress,
    /// The deleter lock stayed held elsewhere.
    DeleterBusy,
    /// Another catch-up lane runs on this host, or the host state directory
    /// is unavailable.
    LaneBusy,
}

impl TrashCatchupEnd {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CaughtUp => "caught_up",
            Self::Cap => "cap",
            Self::Pressure => "pressure",
            Self::NoProgress => "no_progress",
            Self::DeleterBusy => "deleter_busy",
            Self::LaneBusy => "lane_busy",
        }
    }
}

/// What one continuing catch-up lane did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrashCatchupReport {
    pub(crate) end: TrashCatchupEnd,
    pub(crate) slices: u32,
    pub(crate) entries_removed: usize,
    pub(crate) unlinks: usize,
    pub(crate) bytes: u64,
    /// Time spent inside drains (the rest is gaps and lock waits).
    pub(crate) drain_time: std::time::Duration,
    pub(crate) elapsed: std::time::Duration,
    /// Slices that found the deleter lock held elsewhere.
    pub(crate) busy_retries: u32,
    /// When the first slice started (`None`: none did).
    pub(crate) first_slice_at: Option<Instant>,
    /// The lap found the host lane held by another repository's lane.
    pub(crate) lane_held_by_other_repository: bool,
}

impl TrashCatchupReport {
    fn new() -> Self {
        Self {
            end: TrashCatchupEnd::CaughtUp,
            slices: 0,
            entries_removed: 0,
            unlinks: 0,
            bytes: 0,
            drain_time: std::time::Duration::ZERO,
            elapsed: std::time::Duration::ZERO,
            busy_retries: 0,
            first_slice_at: None,
            lane_held_by_other_repository: false,
        }
    }
}

/// How one trash drain takes the deleter lock and paces itself.
#[derive(Clone, Copy)]
struct DrainOptions {
    /// How long to wait for the host-wide deleter lock.
    lock_wait: std::time::Duration,
    /// Stop at the first reading of critical memory pressure (the
    /// continuing catch-up lane) instead of slowing to the floor.
    yield_on_memory_critical: bool,
    /// The host-pressure reading.
    sample: fn() -> crate::checkout_trash::HostPressure,
    /// Deleter threads, each over its own entries and at its share of the
    /// rate (1: the sequential drain).
    threads: u32,
}

impl Default for DrainOptions {
    fn default() -> Self {
        Self {
            lock_wait: std::time::Duration::ZERO,
            yield_on_memory_critical: false,
            sample: crate::checkout_trash::sample_host_pressure,
            threads: 1,
        }
    }
}

/// The share of a retirement run's window kept for the trash drain, as
/// `(numerator, denominator)`: a third normally, half while the trash is
/// behind ([`crate::checkout_trash::TrashBacklog::behind`]). The phases
/// before the drain share the rest (each keeps its own floor), so the drain
/// never runs on whatever they left, and they never get less than half.
const fn trash_drain_share(behind: bool) -> (u32, u32) {
    if behind { (1, 2) } else { (1, 3) }
}

/// When the phases before the drain must end: the window less the drain's
/// share.
fn retirement_sweep_deadline(
    started: Instant,
    window: std::time::Duration,
    behind: bool,
) -> Instant {
    let (numerator, denominator) = trash_drain_share(behind);
    started + window - window * numerator / denominator
}

/// The drain's deadline: the lane's own, or its full share from `now` when
/// the phases before it ran past their deadline (a phase's floor may), so
/// the drain always gets its budget and never more than that past the lane.
fn trash_drain_deadline(
    deadline: Instant,
    now: Instant,
    window: std::time::Duration,
    behind: bool,
) -> Instant {
    let (numerator, denominator) = trash_drain_share(behind);
    deadline.max(now + window * numerator / denominator)
}

/// Runs in a row in which the trash grew: one more when `pending` is above
/// `previous_pending`, reset when it fell, kept when unchanged.
fn next_trash_growing_runs(previous_runs: u32, previous_pending: usize, pending: usize) -> u32 {
    match pending.cmp(&previous_pending) {
        std::cmp::Ordering::Greater => previous_runs.saturating_add(1),
        std::cmp::Ordering::Less => 0,
        std::cmp::Ordering::Equal => previous_runs,
    }
}

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

/// Paths named in an `archive_too_large` keep, at most.
const LARGEST_PATHS_NAMED: usize = 5;

/// The keep detail of a checkout whose archive would exceed the cap: the
/// total and its largest untracked or ignored files, by size (paths and
/// sizes only), plus the patches when they are large.
fn largest_paths_detail(plan: &crate::cleanup_resolve::CleanupResolvePlan, bytes: u64) -> String {
    let mut files = plan
        .untracked_files
        .iter()
        .chain(plan.ignored_files.iter())
        .map(|file| (file.bytes, file.path.as_str()))
        .collect::<Vec<_>>();
    files.sort_by(|left, right| right.cmp(left));
    let mut named = files
        .into_iter()
        .take(LARGEST_PATHS_NAMED)
        .map(|(size, path)| format!("{path} ({} MiB)", size / (1024 * 1024)))
        .collect::<Vec<_>>();
    for (label, size) in [
        ("staged changes", plan.staged_patch_bytes),
        ("unstaged changes", plan.unstaged_patch_bytes),
    ] {
        if size >= 1024 * 1024 {
            named.push(format!("{label} ({} MiB)", size / (1024 * 1024)));
        }
    }
    format!(
        "the archive would hold {} MiB, over the {} MiB cap; largest: {}",
        bytes / (1024 * 1024),
        retire_archive_max_bytes() / (1024 * 1024),
        named.join(", ")
    )
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
    /// When [`Self::process_use`] was observed.
    process_use_taken_at: Option<Instant>,
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

    /// How old the run's open-file observation is (`None`: not taken yet).
    pub(super) fn process_use_age(&self) -> Option<std::time::Duration> {
        self.process_use_taken_at.map(|taken| taken.elapsed())
    }

    /// Make the run's open-file observation look `by` older.
    #[cfg(test)]
    pub(super) fn age_process_use_for_test(&mut self, by: std::time::Duration) {
        self.process_use_taken_at = self
            .process_use_taken_at
            .and_then(|taken| taken.checked_sub(by));
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
            self.process_use_taken_at = Some(Instant::now());
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

impl Broker {
    /// After a restore: mark the session's preparation stale for the
    /// outputs its preparation declares (none of them is archived) and leave
    /// the same notice as an idle strip. Returns the prepare command, or
    /// `None` when the repository declares no preparation.
    fn flag_restored_dependencies(&self, path: &Path, session_id: i64) -> Option<String> {
        let declared: Vec<String> = crate::preparation::load_config(path)
            .ok()
            .flatten()?
            .steps
            .iter()
            .flat_map(|step| step.outputs.iter())
            .map(|output| output.trim_end_matches(['/', '\\']).to_string())
            .filter(|output| !output.is_empty())
            .collect();
        if declared.is_empty() {
            return None;
        }
        crate::warn_unrecorded(
            "mark a restored session's preparation stale",
            crate::preparation::mark_stripped(self.main_root(), session_id, &declared)
                .map_err(|error| error.to_string()),
        );
        let notice = super::strip_idle::write_restore_notice(path, session_id, &declared);
        crate::phase_telemetry::add(
            if notice == super::strip_idle::NoticeOutcome::Written {
                "checkout_restore_notice_written"
            } else {
                "checkout_restore_notice_not_written"
            },
            std::time::Duration::ZERO,
        );
        Some(crate::preparation::prepare_command(session_id))
    }
}

/// Whether `archive` is an operation archive (`<id>-operation-…`, schema
/// `aethyme-operation-archive-v1`), which restore does not read.
pub(crate) fn is_operation_archive(archive: &Path) -> bool {
    archive
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.split_once('-'))
        .is_some_and(|(_, rest)| rest.starts_with("operation-"))
}

/// Why nothing is adopted at `path`: a restore there did not complete.
pub(crate) fn restore_incomplete_reason(path: &Path) -> String {
    format!(
        "a restore into {} did not complete and its partial checkout could not be removed; \
         inspect {}, remove it with `git worktree remove --force`, delete {}, then retry",
        path.display(),
        crate::cleanup_resolve::restore_staging_path(path).display(),
        crate::cleanup_resolve::restore_incomplete_marker(path).display()
    )
}

/// What a restore for reuse put back, for the command's output.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct CheckoutRestored {
    pub path: PathBuf,
    pub archive: PathBuf,
    pub branch: String,
    /// The archived HEAD the checkout is at.
    pub head: String,
    pub branch_recreated: bool,
    pub bundle_used: bool,
    /// Set when the branch was not at the archived HEAD: the checkout is
    /// detached there.
    pub detached: Option<DetachedRestore>,
    /// The one command that rebuilds the dependencies a restore never
    /// brings back, when the repository declares its preparation.
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct DetachedRestore {
    pub branch_tip: String,
    pub archived_head: String,
    /// Keeps the restored state on a new branch; compare it with the branch
    /// before choosing.
    pub reconcile_command: String,
    pub compare_command: String,
}

impl CheckoutRestored {
    fn new(
        path: &Path,
        archive: &Path,
        branch: &str,
        restored: &crate::cleanup_resolve::ArchiveRestore,
    ) -> Self {
        let quoted = format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
        Self {
            path: path.to_path_buf(),
            archive: archive.to_path_buf(),
            branch: branch.to_string(),
            head: restored.head.clone(),
            branch_recreated: restored.branch_recreated,
            bundle_used: restored.bundle_used,
            detached: restored
                .detached_branch_tip
                .as_ref()
                .map(|tip| DetachedRestore {
                    branch_tip: tip.clone(),
                    archived_head: restored.head.clone(),
                    reconcile_command: format!("git -C {quoted} switch -c '{branch}-restored'"),
                    compare_command: format!(
                        "git -C {quoted} log --oneline --left-right {}...{tip}",
                        restored.head
                    ),
                }),
            next_action: None,
        }
    }

    /// The lines printed after a restore; the detached case first and loud.
    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(detached) = &self.detached {
            lines.push(format!(
                "WARNING: branch {} moved since the checkout was archived (tip {}); the restored \
                 checkout is DETACHED at the archived HEAD {}.",
                self.branch, detached.branch_tip, detached.archived_head
            ));
            lines.push(format!("  compare:   {}", detached.compare_command));
            lines.push(format!("  reconcile: {}", detached.reconcile_command));
        }
        lines.push(format!(
            "Restored the checkout at {} from {}{}.",
            self.path.display(),
            self.archive.display(),
            if self.branch_recreated {
                format!(" (branch {} recreated)", self.branch)
            } else {
                String::new()
            }
        ));
        if let Some(next) = &self.next_action {
            lines.push(format!(
                "Dependencies are not restored; prepare the session again before building or testing: {next}"
            ));
        }
        lines
    }
}

thread_local! {
    static LAST_CHECKOUT_RESTORE: std::cell::RefCell<Option<CheckoutRestored>> =
        const { std::cell::RefCell::new(None) };
}

/// The restore the last [`Broker::recreate_retired_checkout`] on this
/// thread performed, if any (taken once).
pub(crate) fn take_last_checkout_restore() -> Option<CheckoutRestored> {
    LAST_CHECKOUT_RESTORE.with(|cell| cell.borrow_mut().take())
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
    /// The idle strip of open sessions that ran after it.
    pub idle_strip: super::strip_idle::IdleStripReport,
    /// Session scratch directories retired (moved into the trash) or kept.
    pub scratch: super::scratch::ScratchRetireReport,
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
    /// When the oldest trash entry still recorded after the last run was
    /// moved in (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trash_oldest_entry_ms: Option<i64>,
    /// Runs in a row after which more trash entries were recorded than after
    /// the run before.
    #[serde(default)]
    pub trash_growing_runs: u32,
    /// Names of the trash entries the last deleter run passed over because
    /// a process still used them (at most 256). The next run leaves them out
    /// of its backlog decision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trash_in_use_entries: Vec<String>,
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
    #[cfg(test)]
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
            process_use_taken_at: None,
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
                self.leave_checkout_tombstone(session_id, outcome);
                self.forget_keep(session_id);
                self.mark_session_in_set(CHECKOUT_RETIREMENT_TOO_LARGE_KEY, session_id, false);
                self.mark_session_in_set(CHECKOUT_RETIREMENT_IN_PROGRESS_KEY, session_id, false);
            }
        }
        Ok(outcome)
    }

    /// Whether the tombstone at `path` has done its job: the archive it
    /// names is gone, or it names none and is older than
    /// `recovery_archive_days` (`0` keeps it).
    fn tombstone_expired(&self, path: &Path) -> bool {
        if let Some(archive) = super::tombstone::tombstone_archive(path) {
            return matches!(
                std::fs::symlink_metadata(&archive),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            );
        }
        let days = crate::load_retention_policy(self.main_root())
            .map(|policy| policy.recovery_archive_days)
            .unwrap_or(90);
        days > 0
            && std::fs::metadata(path.join(super::tombstone::TOMBSTONE_FILE))
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age.as_secs() >= u64::from(days) * 86_400)
    }

    /// The recovery archive recorded for `session_id` when its checkout was
    /// retired through one, while it still exists: the session has state a
    /// restore needs (its branch, and the archive). `None` otherwise.
    pub(crate) fn session_restorable_archive(&self, session_id: i64) -> Option<PathBuf> {
        let recorded: std::collections::BTreeMap<i64, String> =
            self.meta_json(ARCHIVED_SESSIONS_KEY);
        recorded
            .get(&session_id)
            .map(PathBuf::from)
            .filter(|archive| {
                archive
                    .join(crate::cleanup_resolve::MANIFEST_FILE)
                    .is_file()
            })
    }

    /// The newest recovery archive written for `session_id` (an archive
    /// directory whose name starts with the session id and holds a
    /// manifest), in either archive directory, if any. Operation archives
    /// (`<id>-operation-…`, another format that restore does not read) are
    /// never returned: see [`Self::newest_operation_archive`].
    pub(crate) fn newest_recovery_archive(&self, session_id: i64) -> Option<PathBuf> {
        // The archive recorded with the session wins over a directory scan.
        if let Some(recorded) = self.session_restorable_archive(session_id)
            && !is_operation_archive(&recorded)
        {
            return Some(recorded);
        }
        self.newest_session_archive(session_id, false)
    }

    /// The newest operation archive written for `session_id`, if any.
    pub(crate) fn newest_operation_archive(&self, session_id: i64) -> Option<PathBuf> {
        self.newest_session_archive(session_id, true)
    }

    fn newest_session_archive(&self, session_id: i64, operation: bool) -> Option<PathBuf> {
        let prefix = format!("{session_id}-");
        let mut archives = self
            .recovery_archive_roots()
            .iter()
            .filter_map(|root| std::fs::read_dir(root).ok())
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|archive| {
                archive
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(&prefix)
                            && !name.ends_with(".partial")
                            && !name.starts_with('.')
                    })
                    && is_operation_archive(archive) == operation
                    && archive
                        .join(crate::cleanup_resolve::MANIFEST_FILE)
                        .is_file()
            })
            .collect::<Vec<_>>();
        archives.sort_by_key(|archive| {
            std::fs::metadata(archive.join(crate::cleanup_resolve::MANIFEST_FILE))
                .and_then(|metadata| metadata.modified())
                .ok()
        });
        archives.pop()
    }

    /// Leave a tombstone where `session_id`'s checkout was, now that it is
    /// retired (see `super::tombstone`).
    fn leave_checkout_tombstone(&self, session_id: i64, outcome: RetireOutcome) {
        let Ok(session) = self.store_ref().session(session_id) else {
            return;
        };
        let worktree = Path::new(&session.worktree_path);
        if std::fs::symlink_metadata(worktree).is_ok() {
            return;
        }
        // Only a checkout retired through an archive: one whose work was
        // all in Git is recreated from its branch and leaves nothing behind.
        let (archive, saved) = match outcome {
            RetireOutcome::RetiredArchived { .. } => {
                let archive = self.newest_recovery_archive(session_id);
                let saved = archive.clone().map_or(
                    super::tombstone::SavedIn::Branch,
                    super::tombstone::SavedIn::Recovery,
                );
                (archive, saved)
            }
            // Restore does not read an operation archive: the tombstone
            // says so and gives the manual steps; reuse recreates the
            // checkout from its branch and leaves the archive in place.
            RetireOutcome::RetiredOperationArchived { .. } => (
                None,
                self.newest_operation_archive(session_id).map_or(
                    super::tombstone::SavedIn::Branch,
                    super::tombstone::SavedIn::Operation,
                ),
            ),
            _ => return,
        };
        // Recorded with the session, so any lane can tell this session has
        // something to restore without reading directory names.
        if let Some(archive) = archive.as_deref() {
            let archive = archive.to_string_lossy().into_owned();
            crate::warn_unrecorded(
                "record a session's recovery archive",
                self.update_meta_json(
                    ARCHIVED_SESSIONS_KEY,
                    |stored: &mut std::collections::BTreeMap<i64, String>| {
                        stored.insert(session_id, archive.clone());
                        while stored.len() > ARCHIVED_SESSIONS_MAX {
                            stored.pop_first();
                        }
                    },
                ),
            );
        }
        match super::tombstone::write_checkout_tombstone(worktree, &session, &saved) {
            Ok(()) => crate::phase_telemetry::add(
                "checkout_retirement_tombstone_written",
                std::time::Duration::ZERO,
            ),
            Err(error) => {
                crate::phase_telemetry::add(
                    "checkout_retirement_tombstone_failed",
                    std::time::Duration::ZERO,
                );
                crate::warn_unrecorded::<(), _>("leave a retired checkout's tombstone", Err(error));
            }
        }
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
            if let Some(code) = self.closed_checkout_holder_keep(&session, &mut holders) {
                return Ok(Kept(code));
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
            // Name the largest paths (never their content) so an owner can
            // decide what to keep, move or delete.
            run.keep_detail = Some(largest_paths_detail(&plan, bytes));
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
        if let Some(code) = self.closed_checkout_holder_keep(session, &mut holders) {
            return Ok(Kept(code));
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
        let archive_root = self.retirement_archive_root()?;
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
    /// by session-id cursor, until `deadline` less the drain's share (two
    /// thirds of the time, half while the trash is behind), then drain the
    /// trash with at least its share ([`trash_drain_deadline`]).
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
        let trash_behind = self
            .checkout_trash_deletable_backlog(&self.checkout_trash_rows())
            .behind(crate::clock::epoch_ms());
        let sweep_deadline = retirement_sweep_deadline(started, window, trash_behind);
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
            // Then the idle open sessions' regenerable directories, with
            // their own share.
            let idle_deadline = share(1, 4, RETIRE_IDLE_STRIP_MIN_SHARE);
            run.deadline = idle_deadline;
            report.idle_strip = self.strip_idle_open_checkouts(
                &mut run,
                idle_deadline,
                super::strip_idle::IDLE_STRIP_CHECKOUTS_PER_RUN,
            );
            // Session scratch directories of closed (or replaced) sessions:
            // O(1) renames into the trash with a small fixed budget, before
            // the laps, so build output left there never waits behind them.
            // The run's open-file observation, when this pass is the first
            // to need it, keeps its full budget: the later laps reuse it.
            let scratch_deadline =
                (Instant::now() + super::scratch::SCRATCH_RETIRE_BUDGET).min(deadline);
            run.deadline = (Instant::now()
                + RETIRE_PROCESS_USE_BUDGET
                + super::scratch::SCRATCH_RETIRE_BUDGET)
                .min(deadline);
            report.scratch = self.retire_session_scratch(&mut run, scratch_deadline);
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
        // The per-repository bound on archive bytes (both locations),
        // recorded in the storage ledger by location. It runs before the
        // drain, which may use the rest of the lane, with at least its own
        // small reserve; it bounds itself further inside.
        self.record_recovery_archive_bound(
            deadline.max(Instant::now() + RETIRE_ARCHIVE_BOUND_RESERVE),
        );
        let drain_deadline = trash_drain_deadline(deadline, Instant::now(), window, trash_behind);
        if drain_deadline > deadline && !self.checkout_trash_rows().is_empty() {
            crate::phase_telemetry::add(
                "checkout_trash_drain_budget_extended",
                drain_deadline.saturating_duration_since(deadline),
            );
        }
        let host_dir = crate::host_state::resolve_host_state_dir().map(|(dir, _)| dir);
        report.trash = self.retirement_trash_drain(host_dir.as_deref(), drain_deadline);
        self.record_session_scratch_usage(report.trash.scratch_bytes);
        // Reclaim-plan review snapshots pile up at the pool root unless a
        // plan runs to sweep them (1,215 on the busiest repository): the
        // worker sweeps the stale ones too, with the writer's bounds.
        if let Ok(root) =
            self.require_bound_repository_worktree_root("reclaim plan snapshot pruning")
        {
            let _span = crate::phase_telemetry::span("checkout_retirement_prune_reclaim_snapshots");
            let removed = crate::reclaim::prune_review_snapshots(
                &root,
                std::time::SystemTime::now(),
                Instant::now() + crate::reclaim::SNAPSHOT_PRUNE_BUDGET,
                crate::reclaim::SNAPSHOT_PRUNE_MAX,
                None,
            );
            if removed > 0 {
                crate::phase_telemetry::add(
                    "reclaim_snapshots_pruned_by_worker",
                    std::time::Duration::ZERO,
                );
                crate::phase_telemetry::attach(
                    "reclaim_snapshots_pruned_by_worker",
                    serde_json::json!(removed),
                );
            }
        }
        {
            let _span = crate::phase_telemetry::span("checkout_retirement_sweep_partials");
            for root in self.recovery_archive_roots() {
                super::retire_operation::sweep_stale_partials(&root);
            }
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
        crate::phase_telemetry::attach(
            "checkout_retirement",
            serde_json::json!({
                "examined": report.examined,
                "retired_clean": report.retired_clean,
                "retired_archived": report.retired_archived,
                "retired_operation_archived": report.retired_operation_archived,
                "retired_cleaned_leftovers": report.retired_cleaned_leftovers,
                "archive_bytes": report.archive_bytes,
                "kept": report.kept,
                "trash_entries_removed": report.trash.entries_removed,
                "elapsed_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            }),
        );
        let previous = self.checkout_retirement_progress();
        let shrinking = report.retired_clean + report.retired_archived + report.retired_operation_archived > 0
                || report.strip.directories_stripped > 0
                || report.idle_strip.strip.directories_stripped > 0
                || report.scratch.retired > 0
                || report.trash.entries_removed > 0
                // A live catch-up lane drains the trash meanwhile.
                || report.trash.skipped_for_catchup_lane
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
            trash_in_use_entries: report.trash.in_use_names.clone(),
            trash_oldest_entry_ms: report.trash.oldest_pending_created_ms,
            trash_growing_runs: next_trash_growing_runs(
                previous.trash_growing_runs,
                previous.trash_pending,
                report.trash.entries_pending,
            ),
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
            || report.strip.more_remaining
            || report.idle_strip.strip.more_remaining
            || report.scratch.more_remaining;
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
                // A retired checkout's tombstone is no checkout: for a
                // session still recorded as closed (its cleaned record was
                // lost) it reads as gone, so the reconciliation below runs.
                let tombstone = exists
                    && super::tombstone::is_checkout_tombstone(Path::new(&session.worktree_path));
                let exists = exists && (cleaned || !tombstone);
                // For a cleaned session it is neither a checkout nor
                // garbage: skipped, and removed once it has expired (its
                // archive is gone, or it never had one and is older than
                // `recovery_archive_days`).
                if tombstone && cleaned {
                    crate::phase_telemetry::add(
                        "checkout_retirement_tombstone_skipped",
                        std::time::Duration::ZERO,
                    );
                    if self.tombstone_expired(Path::new(&session.worktree_path))
                        && super::tombstone::remove_checkout_tombstone(Path::new(
                            &session.worktree_path,
                        ))
                    {
                        crate::phase_telemetry::add(
                            "checkout_retirement_tombstone_expired",
                            std::time::Duration::ZERO,
                        );
                    }
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
    /// directory). The lock is waited for at most [`DELETER_LOCK_WAIT`]
    /// (a continuing catch-up lane releases it between slices); when it
    /// stays held or cannot be taken, this run deletes nothing: there is
    /// never a second deleter.
    pub(crate) fn drain_checkout_trash(
        &self,
        deadline: Instant,
    ) -> crate::checkout_trash::TrashDrainReport {
        let lock_path = crate::host_state::resolve_host_state_dir()
            .map(|(dir, _)| dir.join(crate::checkout_trash::DELETER_LOCK_FILE));
        self.drain_checkout_trash_with(
            lock_path.as_deref(),
            deadline,
            DrainOptions {
                lock_wait: DELETER_LOCK_WAIT,
                ..DrainOptions::default()
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn drain_checkout_trash_with_lock(
        &self,
        lock_path: Option<&Path>,
        deadline: Instant,
    ) -> crate::checkout_trash::TrashDrainReport {
        self.drain_checkout_trash_with(lock_path, deadline, DrainOptions::default())
    }

    fn drain_checkout_trash_with(
        &self,
        lock_path: Option<&Path>,
        deadline: Instant,
        options: DrainOptions,
    ) -> crate::checkout_trash::TrashDrainReport {
        let mut rows = self.checkout_trash_rows();
        let pending = rows.values().map(std::collections::BTreeMap::len).sum();
        let Some(first) = rows.keys().next().cloned() else {
            return crate::checkout_trash::TrashDrainReport::default();
        };
        let wait_until = deadline.min(Instant::now() + options.lock_wait);
        let lock = lock_path.and_then(|path| {
            loop {
                match crate::checkout_trash::try_deleter_lock(path) {
                    Ok(Some(lock)) => break Some(lock),
                    Ok(None) if Instant::now() < wait_until => {
                        std::thread::sleep(DELETER_LOCK_POLL);
                    }
                    Ok(None) | Err(_) => break None,
                }
            }
        });
        let Some(_lock) = lock else {
            crate::phase_telemetry::add("checkout_trash_deleter_busy", std::time::Duration::ZERO);
            return crate::checkout_trash::TrashDrainReport {
                entries_pending: pending,
                oldest_pending_created_ms: crate::checkout_trash::TrashBacklog::of(&rows)
                    .oldest_created_ms,
                deleter_busy: true,
                ..crate::checkout_trash::TrashDrainReport::default()
            };
        };
        let before = rows.clone();
        let now_ms = crate::clock::epoch_ms();
        let backlog = self.checkout_trash_deletable_backlog(&rows);
        let need = crate::checkout_trash::DrainNeed::of(
            self.checkout_trash_below_headroom(&first),
            backlog.behind(now_ms),
        );
        let ceiling = crate::gc::unattended_pacing::explicit_unlink_limit(self.main_root());
        let threads = options.threads.max(1);
        let make_pacer = || {
            let pacer = crate::checkout_trash::DrainPacer::for_need(need, options.sample)
                .with_ceiling(ceiling)
                .with_share(threads);
            if options.yield_on_memory_critical {
                pacer.yielding_on_memory_critical()
            } else {
                pacer
            }
        };
        let drain_started = Instant::now();
        let mut report = if threads > 1 {
            let trash_dirs = rows.keys().cloned().collect::<Vec<_>>();
            crate::checkout_trash::drain_trash_parallel(
                &mut rows,
                deadline,
                make_pacer,
                threads,
                None,
                || crate::checkout_trash::trash_volumes_available(&trash_dirs),
            )
        } else {
            crate::checkout_trash::drain_trash_paced(&mut rows, deadline, &mut make_pacer())
        };
        // Entries recorded by another process during the drain are kept:
        // only what this run removed, forgot, confirmed or counted is
        // applied, atomically.
        let applied = self.update_meta_json(
            CHECKOUT_RETIREMENT_TRASH_ROWS_KEY,
            |current: &mut crate::checkout_trash::TrashRows| {
                crate::checkout_trash::apply_drain_rows(current, &before, &rows);
            },
        );
        crate::warn_unrecorded("record the checkout trash drain", applied);
        let after = self.checkout_trash_rows();
        report.entries_pending = after.values().map(std::collections::BTreeMap::len).sum();
        report.oldest_pending_created_ms =
            crate::checkout_trash::TrashBacklog::of(&after).oldest_created_ms;
        let elapsed_ms = u64::try_from(drain_started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let pace = report
            .pace
            .unwrap_or(crate::checkout_trash::DeleterPace::Steady);
        let observed_rate = u64::try_from(report.unlinks)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_000)
            .checked_div(elapsed_ms)
            .unwrap_or(0);
        let backlog_oldest_age_s = backlog.oldest_age_ms(now_ms).map(|age| age / 1_000);
        let final_pace = report
            .final_pace
            .map(crate::checkout_trash::DeleterPace::as_str);
        let oldest_pending_age_s = report
            .oldest_pending_created_ms
            .map(|created| now_ms.saturating_sub(created).max(0) / 1_000);
        let ceiling_applied = report.configured_ceiling.is_some_and(|ceiling| {
            pace.unlinks_per_second()
                .is_some_and(|uncapped| ceiling < uncapped)
        });
        if ceiling_applied {
            crate::phase_telemetry::add(
                "checkout_trash_configured_ceiling_applied",
                std::time::Duration::ZERO,
            );
        }
        crate::phase_telemetry::attach(
            "checkout_trash_drain",
            serde_json::json!({
                "backlog_entries": backlog.entries,
                "backlog_oldest_age_s": backlog_oldest_age_s,
                "need": need.as_str(),
                "initial_pace": pace.as_str(),
                "initial_unlinks_per_second": report.initial_unlinks_per_second,
                "configured_ceiling": report.configured_ceiling,
                "ceiling_applied": ceiling_applied,
                "final_pace": final_pace,
                "pace_changes": report.pace_changes,
                "entries_removed": report.entries_removed,
                "unlinks": report.unlinks,
                "bytes": report.bytes,
                "elapsed_ms": elapsed_ms,
                "unlinks_per_second_observed": observed_rate,
                "passed_over": {
                    "in_use": report.entries_in_use,
                    "failed": report.failed.len(),
                    "unrecognized": report.unrecognized,
                },
                "budget_stopped": report.budget_stopped,
                "process_use_unknown": report.process_use_unknown,
                "end_reason": report.end_reason(),
                "threads": threads,
                "io_policy": crate::gc::current_io_policy(),
                "entries_pending": report.entries_pending,
                "oldest_pending_age_s": oldest_pending_age_s,
            }),
        );
        report
    }

    /// The backlog the deleter can work on: `rows` less the entries the last
    /// run passed over because a process still used them (they stay in the
    /// advice evidence, not in the pace or share decision).
    pub(crate) fn checkout_trash_deletable_backlog(
        &self,
        rows: &crate::checkout_trash::TrashRows,
    ) -> crate::checkout_trash::TrashBacklog {
        let in_use = self
            .checkout_retirement_progress()
            .trash_in_use_entries
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        crate::checkout_trash::TrashBacklog::deletable(rows, &in_use)
    }

    /// The continuing catch-up lane: while the trash is behind, drain it in
    /// slices of [`TRASH_CATCHUP_SLICE`] (the deleter lock held for one
    /// slice at most, then released for [`TRASH_CATCHUP_GAP`]) until it is
    /// no longer behind, `cap` is reached, a reading shows critical memory
    /// pressure (each slice also stops at once on one), or a slice makes no
    /// progress. Each slice is an ordinary paced drain: the single
    /// host-wide deleter, re-reading pressure every 2 s. One lane per host
    /// (`host_dir`'s [`TRASH_CATCHUP_LOCK_FILE`]). It holds no repository
    /// lock: the detached worker runs it after releasing its worker lock,
    /// so the next worker run (and its strip and retirement) starts as
    /// usual, and that run's drain takes the deleter lock between slices.
    #[cfg(test)]
    pub(crate) fn run_checkout_trash_catchup(
        &self,
        host_dir: Option<&Path>,
        cap: std::time::Duration,
        sample: fn() -> crate::checkout_trash::HostPressure,
    ) -> TrashCatchupReport {
        self.run_checkout_trash_catchup_lap(host_dir, cap, sample, false)
    }

    /// One catch-up lap, as described above; `until_empty` (the operator's
    /// `gc trash drain`) keeps draining while any entry is deletable, not
    /// only while the trash is behind.
    pub(crate) fn run_checkout_trash_catchup_lap(
        &self,
        host_dir: Option<&Path>,
        cap: std::time::Duration,
        sample: fn() -> crate::checkout_trash::HostPressure,
        until_empty: bool,
    ) -> TrashCatchupReport {
        let _span = crate::phase_telemetry::span("checkout_trash_catchup");
        let started = Instant::now();
        let end_at = started + cap;
        let mut report = TrashCatchupReport::new();
        // A retirement run probes the lane lock to see whether a lane is
        // live; a lane starting at that instant waits the probe out.
        let lane_lock = host_dir.and_then(|dir| {
            let path = dir.join(TRASH_CATCHUP_LOCK_FILE);
            let wait_until = Instant::now() + TRASH_CATCHUP_LANE_LOCK_WAIT;
            loop {
                match crate::checkout_trash::try_catchup_lane_lock(&path, self.main_root()) {
                    Ok(Some(lock)) => break Some(lock),
                    Ok(None) if Instant::now() < wait_until => {
                        std::thread::sleep(DELETER_LOCK_POLL);
                    }
                    Ok(None) | Err(_) => break None,
                }
            }
        });
        let (Some(dir), Some(_lane_lock)) = (host_dir, lane_lock) else {
            report.end = TrashCatchupEnd::LaneBusy;
            report.lane_held_by_other_repository = host_dir.is_some_and(|dir| {
                crate::checkout_trash::catchup_lane_holder_repository(
                    &dir.join(TRASH_CATCHUP_LOCK_FILE),
                )
                .is_some_and(|holder| !self.is_this_repository(&holder))
            });
            report.elapsed = started.elapsed();
            crate::phase_telemetry::add(
                "checkout_trash_catchup_end_lane_busy",
                std::time::Duration::ZERO,
            );
            return report;
        };
        self.clear_own_catchup_lane_waiter(dir);
        let deleter_lock = dir.join(crate::checkout_trash::DELETER_LOCK_FILE);
        let mut busy_since: Option<Instant> = None;
        report.end = loop {
            let now = Instant::now();
            if now >= end_at {
                break TrashCatchupEnd::Cap;
            }
            let rows = self.checkout_trash_rows();
            let deletable = self.checkout_trash_deletable_backlog(&rows);
            let done = if until_empty {
                deletable.entries == 0
            } else {
                !deletable.behind(crate::clock::epoch_ms())
            };
            if done {
                break TrashCatchupEnd::CaughtUp;
            }
            if sample().memory_critical {
                break TrashCatchupEnd::Pressure;
            }
            let slice_started = Instant::now();
            report.first_slice_at.get_or_insert(slice_started);
            let drained = self.drain_checkout_trash_with(
                Some(&deleter_lock),
                (slice_started + TRASH_CATCHUP_SLICE).min(end_at),
                DrainOptions {
                    lock_wait: std::time::Duration::ZERO,
                    yield_on_memory_critical: true,
                    sample,
                    threads: crate::checkout_trash::TRASH_CATCHUP_THREADS,
                },
            );
            if drained.deleter_busy {
                report.busy_retries = report.busy_retries.saturating_add(1);
                let since = *busy_since.get_or_insert(slice_started);
                if since.elapsed() >= TRASH_CATCHUP_BUSY_MAX {
                    break TrashCatchupEnd::DeleterBusy;
                }
                std::thread::sleep(
                    TRASH_CATCHUP_GAP.min(end_at.saturating_duration_since(Instant::now())),
                );
                continue;
            }
            busy_since = None;
            report.slices = report.slices.saturating_add(1);
            report.drain_time += slice_started.elapsed();
            report.entries_removed = report
                .entries_removed
                .saturating_add(drained.entries_removed);
            report.unlinks = report.unlinks.saturating_add(drained.unlinks);
            report.bytes = report.bytes.saturating_add(drained.bytes);
            if drained.yielded_to_pressure {
                break TrashCatchupEnd::Pressure;
            }
            if drained.entries_removed == 0 && !drained.budget_stopped {
                break TrashCatchupEnd::NoProgress;
            }
            // Let a retirement drain (this repository's next run, or
            // another repository's) take the deleter lock.
            std::thread::sleep(
                TRASH_CATCHUP_GAP.min(end_at.saturating_duration_since(Instant::now())),
            );
        };
        report.elapsed = started.elapsed();
        crate::phase_telemetry::add(
            match report.end {
                TrashCatchupEnd::CaughtUp => "checkout_trash_catchup_end_caught_up",
                TrashCatchupEnd::Cap => "checkout_trash_catchup_end_cap",
                TrashCatchupEnd::Pressure => "checkout_trash_catchup_end_pressure",
                TrashCatchupEnd::NoProgress => "checkout_trash_catchup_end_no_progress",
                TrashCatchupEnd::DeleterBusy => "checkout_trash_catchup_end_deleter_busy",
                TrashCatchupEnd::LaneBusy => "checkout_trash_catchup_end_lane_busy",
            },
            std::time::Duration::ZERO,
        );
        report
    }

    /// Catch-up laps back to back: a lap that ends at its cap (`lap_max`)
    /// is followed at once by the next, at most `max_laps` laps and `total`
    /// wall time in all, so no gap opens until the next worker. Any other
    /// end (caught up, pressure, no progress, busy) stops.
    pub(crate) fn run_checkout_trash_catchup_laps(
        &self,
        host_dir: Option<&Path>,
        bounds: TrashCatchupBounds,
        sample: fn() -> crate::checkout_trash::HostPressure,
    ) -> Vec<TrashCatchupReport> {
        let started = Instant::now();
        let mut laps: Vec<TrashCatchupReport> = Vec::new();
        loop {
            let remaining = bounds.total.saturating_sub(started.elapsed());
            let lap = self.run_checkout_trash_catchup_lap(
                host_dir,
                remaining.min(bounds.lap_max),
                sample,
                bounds.until_empty,
            );
            let time_left = !bounds.total.saturating_sub(started.elapsed()).is_zero();
            let drained_laps = laps
                .iter()
                .filter(|lap| lap.end != TrashCatchupEnd::LaneBusy)
                .count();
            let mut wait = None;
            let mut again = false;
            if lap.end == TrashCatchupEnd::Cap && drained_laps + 1 < bounds.max_laps && time_left {
                // Another repository waiting for the lane gets it now
                // rather than after a further lap.
                if host_dir.is_some_and(|dir| self.another_repository_waits_for_catchup_lane(dir)) {
                    crate::phase_telemetry::add(
                        "checkout_trash_catchup_yielded_to_waiter",
                        std::time::Duration::ZERO,
                    );
                } else {
                    again = true;
                    crate::phase_telemetry::add(
                        "checkout_trash_catchup_next_lap",
                        std::time::Duration::ZERO,
                    );
                }
            } else if lap.end == TrashCatchupEnd::LaneBusy
                && lap.lane_held_by_other_repository
                && time_left
                && let (Some(retry), Some(dir)) = (bounds.lane_retry, host_dir)
            {
                // Another repository's lane: wait for a turn while this
                // worker lives, and say so, so its holder hands over
                // between laps.
                self.note_catchup_lane_waiter(dir);
                crate::phase_telemetry::add(
                    "checkout_trash_catchup_lane_retry",
                    std::time::Duration::ZERO,
                );
                wait = Some(retry.min(bounds.total.saturating_sub(started.elapsed())));
                again = true;
            }
            laps.push(lap);
            if !again {
                break;
            }
            if let Some(wait) = wait {
                std::thread::sleep(wait);
            }
        }
        laps
    }

    /// The body of the worker's catch-up lane process: its laps, then
    /// `checkout_trash_catchup` on this process's metric row, with the
    /// delay from `worker_started_ms` (the starting worker's start, epoch
    /// ms) to the first slice and the disk I/O policy the process set.
    pub(crate) fn run_checkout_trash_catchup_process(
        &self,
        host_dir: Option<&Path>,
        worker_started_ms: Option<i64>,
        io_policy: &'static str,
        sample: fn() -> crate::checkout_trash::HostPressure,
    ) -> Vec<TrashCatchupReport> {
        let process_started = Instant::now();
        let laps =
            self.run_checkout_trash_catchup_laps(host_dir, TrashCatchupBounds::worker(), sample);
        let sum = |pick: fn(&TrashCatchupReport) -> u64| {
            laps.iter().map(pick).fold(0_u64, u64::saturating_add)
        };
        let drain_time = laps
            .iter()
            .map(|lap| lap.drain_time)
            .fold(std::time::Duration::ZERO, |total, time| {
                total.saturating_add(time)
            });
        let unlinks = sum(|lap| lap.unlinks as u64);
        let files_per_second = if drain_time.as_secs_f64() > 0.0 {
            (unlinks as f64 / drain_time.as_secs_f64()).round() as u64
        } else {
            0
        };
        let wall = process_started.elapsed().as_secs_f64();
        let drain_share_pct = if wall > 0.0 {
            (drain_time.as_secs_f64() * 100.0 / wall).round() as u64
        } else {
            0
        };
        let now = crate::clock::epoch_ms();
        let start_delay_ms = laps
            .iter()
            .find_map(|lap| lap.first_slice_at)
            .zip(worker_started_ms)
            .map(|(first, started)| {
                let first_ms = now
                    .saturating_sub(i64::try_from(first.elapsed().as_millis()).unwrap_or(i64::MAX));
                first_ms.saturating_sub(started).max(0)
            });
        if laps
            .iter()
            .filter(|lap| lap.end != TrashCatchupEnd::LaneBusy)
            .count()
            > 1
        {
            crate::phase_telemetry::add(
                "checkout_trash_catchup_back_to_back",
                std::time::Duration::ZERO,
            );
        }
        crate::phase_telemetry::attach(
            "checkout_trash_catchup",
            serde_json::json!({
                "end_reason": laps.last().map_or("none", |lap| lap.end.as_str()),
                "io_policy": io_policy,
                "catchup_start_delay_ms": start_delay_ms,
                "laps": laps.len(),
                "slices": sum(|lap| u64::from(lap.slices)),
                "entries_removed": sum(|lap| lap.entries_removed as u64),
                "unlinks": unlinks,
                "bytes": sum(|lap| lap.bytes),
                "drain_ms": u64::try_from(drain_time.as_millis()).unwrap_or(u64::MAX),
                "elapsed_ms": u64::try_from(process_started.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
                "files_per_second_observed": files_per_second,
                "busy_retries": sum(|lap| u64::from(lap.busy_retries)),
                "drain_share_pct": drain_share_pct,
            }),
        );
        laps
    }

    /// Whose catch-up lane, if any, is live on this host: its lock in
    /// `host_dir` is held, and its holder line names the repository. The
    /// probe takes the lock for an instant when it is free; a lane starting
    /// then waits it out. A holder that names no repository counts as
    /// another repository's.
    pub(crate) fn checkout_trash_catchup_lane_state(&self, host_dir: &Path) -> CatchupLaneState {
        let path = host_dir.join(TRASH_CATCHUP_LOCK_FILE);
        match crate::checkout_trash::try_deleter_lock(&path) {
            Ok(None) => {
                if crate::checkout_trash::catchup_lane_holder_repository(&path)
                    .is_some_and(|holder| self.is_this_repository(&holder))
                {
                    CatchupLaneState::ThisRepository
                } else {
                    CatchupLaneState::OtherRepository
                }
            }
            Ok(Some(_)) | Err(_) => CatchupLaneState::Free,
        }
    }

    fn is_this_repository(&self, path: &Path) -> bool {
        let root = self.main_root();
        path == root
            || std::fs::canonicalize(path)
                .ok()
                .zip(std::fs::canonicalize(root).ok())
                .is_some_and(|(path, root)| path == root)
    }

    /// Record that this repository's lane waits for the host lane
    /// ([`TRASH_CATCHUP_WAITER_FILE`]); the holder hands over between laps.
    fn note_catchup_lane_waiter(&self, host_dir: &Path) {
        crate::warn_unrecorded(
            "note a waiting checkout trash catch-up lane",
            std::fs::write(
                host_dir.join(TRASH_CATCHUP_WAITER_FILE),
                format!("repository {}\n", self.main_root().display()),
            ),
        );
    }

    /// Whether a lane of another repository noted, within
    /// [`TRASH_CATCHUP_WAITER_FRESH`], that it waits for the host lane.
    fn another_repository_waits_for_catchup_lane(&self, host_dir: &Path) -> bool {
        let path = host_dir.join(TRASH_CATCHUP_WAITER_FILE);
        let fresh = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age <= TRASH_CATCHUP_WAITER_FRESH);
        fresh
            && std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| {
                    text.lines()
                        .find_map(|line| line.strip_prefix("repository ").map(PathBuf::from))
                })
                .is_some_and(|waiter| !self.is_this_repository(&waiter))
    }

    /// Forget this repository's waiter note once its lane runs.
    fn clear_own_catchup_lane_waiter(&self, host_dir: &Path) {
        let path = host_dir.join(TRASH_CATCHUP_WAITER_FILE);
        let own = std::fs::read_to_string(&path).ok().is_some_and(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("repository ").map(PathBuf::from))
                .is_some_and(|waiter| self.is_this_repository(&waiter))
        });
        if own {
            crate::warn_unrecorded(
                "forget this repository's checkout trash catch-up waiter note",
                std::fs::remove_file(path),
            );
        }
    }

    /// The retirement run's drain: skipped (and counted) while a catch-up
    /// lane is live on this host, which drains this trash anyway and would
    /// only make the run wait for the deleter lock; otherwise the ordinary
    /// drain, waiting for that lock at most `DELETER_LOCK_WAIT`.
    fn retirement_trash_drain(
        &self,
        host_dir: Option<&Path>,
        deadline: Instant,
    ) -> crate::checkout_trash::TrashDrainReport {
        // Only this repository's own lane stands in for its drain: another
        // repository's lane does not touch this trash, so the drain takes
        // its turns on the deleter lock as before.
        if host_dir.is_some_and(|dir| {
            self.checkout_trash_catchup_lane_state(dir) == CatchupLaneState::ThisRepository
        }) {
            crate::phase_telemetry::add(
                "checkout_trash_retirement_drain_skipped",
                std::time::Duration::ZERO,
            );
            let rows = self.checkout_trash_rows();
            return crate::checkout_trash::TrashDrainReport {
                entries_pending: rows.values().map(std::collections::BTreeMap::len).sum(),
                oldest_pending_created_ms: crate::checkout_trash::TrashBacklog::of(&rows)
                    .oldest_created_ms,
                skipped_for_catchup_lane: true,
                ..crate::checkout_trash::TrashDrainReport::default()
            };
        }
        let lock_path = host_dir.map(|dir| dir.join(crate::checkout_trash::DELETER_LOCK_FILE));
        self.drain_checkout_trash_with(
            lock_path.as_deref(),
            deadline,
            DrainOptions {
                lock_wait: DELETER_LOCK_WAIT,
                ..DrainOptions::default()
            },
        )
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
        // A restore that neither completed nor cleaned up after itself:
        // nothing is adopted or recreated at the path until it is resolved.
        if crate::cleanup_resolve::restore_incomplete(path) {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: restore_incomplete_reason(path),
            });
        }
        let tombstone = super::tombstone::is_checkout_tombstone(path);
        if session.cleanup_state != crate::SessionCleanupState::Cleaned
            || session.origin != crate::SessionOrigin::Spawned
            || (path.exists() && !tombstone)
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
        // The tombstone left at the path goes first, under the claim, so the
        // checkout comes back at the same path (Git's registration was
        // removed when it was retired).
        if tombstone && !super::tombstone::remove_checkout_tombstone(path) {
            return Err(refuse(format!(
                "cannot remove the tombstone at {} to restore the checkout there",
                path.display()
            )));
        }
        // A checkout retired with a recovery archive comes back from it: the
        // same commits, staged and unstaged changes, untracked and
        // protected ignored files, each checked against the manifest. A
        // failed restore removes the partial checkout and keeps the archive.
        {
            if let Some(archive) = self.newest_recovery_archive(session.id) {
                let started = std::time::Instant::now();
                return match crate::cleanup_resolve::restore_recovery_archive(
                    self.main_root(),
                    &archive,
                    path,
                    &session.branch,
                ) {
                    Ok(restored) => {
                        crate::phase_telemetry::add(
                            "checkout_retirement_restored_for_reuse",
                            started.elapsed(),
                        );
                        // Regenerable directories are never archived: the
                        // restored checkout's preparation reads stale, with
                        // the same loud notice and next action as an idle
                        // strip.
                        let next_action = self.flag_restored_dependencies(path, session.id);
                        LAST_CHECKOUT_RESTORE.with(|cell| {
                            let mut report =
                                CheckoutRestored::new(path, &archive, &session.branch, &restored);
                            report.next_action = next_action;
                            *cell.borrow_mut() = Some(report);
                        });
                        crate::phase_telemetry::attach(
                            "checkout_restore",
                            serde_json::json!({
                                "session_id": session.id,
                                "head": restored.head,
                                "bundle_used": restored.bundle_used,
                                "branch_recreated": restored.branch_recreated,
                                "staged_patch_bytes": restored.staged_patch_bytes,
                                "unstaged_patch_bytes": restored.unstaged_patch_bytes,
                                "untracked_files": restored.untracked_files,
                                "ignored_files": restored.ignored_files,
                                "detached_branch_tip": restored.detached_branch_tip,
                            }),
                        );
                        Ok(true)
                    }
                    Err(reason) => {
                        // Nothing came back: the tombstone goes back too.
                        if tombstone {
                            crate::warn_unrecorded(
                                "put a tombstone back after a failed restore",
                                super::tombstone::write_checkout_tombstone(
                                    path,
                                    &session,
                                    &super::tombstone::SavedIn::Recovery(archive.clone()),
                                ),
                            );
                        }
                        Err(refuse(format!(
                            "the checkout was retired with a recovery archive and restoring it failed ({reason}); the archive is kept: restore it as {} describes",
                            archive.join("README.md").display()
                        )))
                    }
                };
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

    /// The checkout is no longer there: gone, or (retired through an
    /// archive) replaced by its tombstone.
    fn retired_away(worktree: &Path) -> bool {
        !worktree.exists() || super::super::tombstone::is_checkout_tombstone(worktree)
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
        broker
            .recovery_archive_roots()
            .iter()
            .filter_map(|root| std::fs::read_dir(root).ok())
            .flatten()
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
        assert!(retired_away(&worktree));
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
        assert!(retired_away(&worktree));
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
        assert!(retired_away(&worktree));
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
            assert!(retired_away(&worktree));
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
        assert!(retired_away(&worktree));
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

    /// Retire a closed checkout into the trash and age its entry past a
    /// day, so the trash is behind; returns the entry's path.
    fn trashed_day_old_entry(broker: &mut crate::Broker, label: &str) -> PathBuf {
        let (session, worktree) = closed(broker, label, |_| {});
        let current = broker.store_ref().session(session.id).unwrap();
        {
            let _lane = super::RetirementLaneScope::enter();
            broker
                .remove_checkout_through_trash(&current, &worktree, true)
                .unwrap();
        }
        let (dir, name, row) = broker
            .checkout_trash_rows()
            .into_iter()
            .flat_map(|(dir, entries)| {
                entries
                    .into_iter()
                    .map(move |(name, row)| (dir.clone(), name, row))
            })
            .find(|(_, _, row)| row.session_id == session.id && !row.stripped)
            .unwrap();
        let old = format!("0000000000001-{}-1-1", session.id);
        std::fs::rename(dir.join(&name), dir.join(&old)).unwrap();
        broker
            .update_meta_json(
                CHECKOUT_RETIREMENT_TRASH_ROWS_KEY,
                |rows: &mut crate::checkout_trash::TrashRows| {
                    if let Some(entries) = rows.get_mut(&dir) {
                        entries.remove(&name);
                        entries.insert(old.clone(), row);
                    }
                },
            )
            .unwrap();
        dir.join(old)
    }

    fn no_pressure() -> crate::checkout_trash::HostPressure {
        crate::checkout_trash::HostPressure::default()
    }

    fn critical_memory() -> crate::checkout_trash::HostPressure {
        crate::checkout_trash::HostPressure {
            memory_critical: true,
            ..crate::checkout_trash::HostPressure::default()
        }
    }

    /// The continuing catch-up lane drains while the trash is behind and
    /// ends once it is not; it stops at its cap and at critical memory
    /// pressure without deleting, and only one lane runs per host.
    #[test]
    fn the_catchup_lane_drains_until_caught_up_within_its_bounds() {
        let (owner, _storage, mut broker) = fixture();
        let entry = trashed_day_old_entry(&mut broker, "catch up");
        let host = owner.path();
        assert!(
            broker
                .checkout_trash_deletable_backlog(&broker.checkout_trash_rows())
                .behind(crate::clock::epoch_ms())
        );

        let capped = broker.run_checkout_trash_catchup(Some(host), Duration::ZERO, no_pressure);
        assert_eq!(capped.end, super::TrashCatchupEnd::Cap, "{capped:?}");
        assert_eq!(capped.slices, 0);
        assert!(entry.exists());

        let pressured =
            broker.run_checkout_trash_catchup(Some(host), Duration::from_secs(60), critical_memory);
        assert_eq!(
            pressured.end,
            super::TrashCatchupEnd::Pressure,
            "{pressured:?}"
        );
        assert!(entry.exists());

        let lane =
            crate::checkout_trash::try_deleter_lock(&host.join(super::TRASH_CATCHUP_LOCK_FILE))
                .unwrap()
                .unwrap();
        let busy =
            broker.run_checkout_trash_catchup(Some(host), Duration::from_secs(60), no_pressure);
        assert_eq!(busy.end, super::TrashCatchupEnd::LaneBusy, "{busy:?}");
        drop(lane);
        let unavailable =
            broker.run_checkout_trash_catchup(None, Duration::from_secs(60), no_pressure);
        assert_eq!(unavailable.end, super::TrashCatchupEnd::LaneBusy);
        assert!(entry.exists());

        let caught_up =
            broker.run_checkout_trash_catchup(Some(host), Duration::from_secs(60), no_pressure);
        assert_eq!(
            caught_up.end,
            super::TrashCatchupEnd::CaughtUp,
            "{caught_up:?}"
        );
        assert!(caught_up.slices >= 1, "{caught_up:?}");
        assert!(caught_up.entries_removed >= 1, "{caught_up:?}");
        assert!(!entry.exists());
        assert!(caught_up.elapsed < Duration::from_secs(60));
    }

    /// The lane process's body drains this repository's trash until no
    /// entry is deletable (the worker starts it whenever the trash holds
    /// entries), ends caught up, and reports a start delay measured from the
    /// worker's start; with this repository's own lane already live it ends
    /// at once, removing nothing.
    #[test]
    fn the_catchup_lane_process_drains_until_empty_from_the_workers_start() {
        let (owner, _storage, mut broker) = fixture();
        let entry = trashed_day_old_entry(&mut broker, "process");
        let host = owner.path();
        let own = crate::checkout_trash::try_catchup_lane_lock(
            &host.join(super::TRASH_CATCHUP_LOCK_FILE),
            broker.main_root(),
        )
        .unwrap()
        .unwrap();
        let busy = broker.run_checkout_trash_catchup_process(
            Some(host),
            Some(crate::clock::epoch_ms()),
            "important",
            no_pressure,
        );
        assert_eq!(busy.len(), 1, "own lane live: no retry ({busy:?})");
        assert_eq!(busy[0].end, super::TrashCatchupEnd::LaneBusy);
        assert!(!busy[0].lane_held_by_other_repository);
        assert!(entry.exists());
        drop(own);

        let worker_started_ms = crate::clock::epoch_ms() - 5_000;
        let laps = broker.run_checkout_trash_catchup_process(
            Some(host),
            Some(worker_started_ms),
            "important",
            no_pressure,
        );
        assert_eq!(laps.len(), 1, "{laps:?}");
        assert_eq!(laps[0].end, super::TrashCatchupEnd::CaughtUp);
        assert!(laps[0].first_slice_at.is_some());
        assert!(!entry.exists());
        assert!(super::TrashCatchupBounds::worker().until_empty);
    }

    /// Laps follow one another only after a lap that hit its cap, and never
    /// more than the bound.
    #[test]
    fn back_to_back_catchup_laps_are_bounded() {
        let (owner, _storage, mut broker) = fixture();
        let entry = trashed_day_old_entry(&mut broker, "laps");
        let capped = |max_laps| super::TrashCatchupBounds {
            total: Duration::from_secs(60),
            lap_max: Duration::ZERO,
            max_laps,
            until_empty: false,
            lane_retry: None,
        };
        let laps =
            broker.run_checkout_trash_catchup_laps(Some(owner.path()), capped(3), no_pressure);
        assert_eq!(laps.len(), 3, "{laps:?}");
        assert!(
            laps.iter()
                .all(|lap| lap.end == super::TrashCatchupEnd::Cap)
        );
        let one =
            broker.run_checkout_trash_catchup_laps(Some(owner.path()), capped(1), no_pressure);
        assert_eq!(one.len(), 1);
        let no_time = broker.run_checkout_trash_catchup_laps(
            Some(owner.path()),
            super::TrashCatchupBounds {
                total: Duration::ZERO,
                ..capped(3)
            },
            no_pressure,
        );
        assert_eq!(no_time.len(), 1, "the total bounds the laps too");
        assert!(entry.exists());
        // A lap that is not capped (caught up) ends the run.
        let drained = broker.run_checkout_trash_catchup_laps(
            Some(owner.path()),
            super::TrashCatchupBounds {
                lap_max: Duration::from_secs(60),
                ..capped(3)
            },
            no_pressure,
        );
        assert_eq!(drained.len(), 1, "{drained:?}");
        assert_eq!(drained[0].end, super::TrashCatchupEnd::CaughtUp);
        assert!(!entry.exists());
        assert_eq!(
            super::TrashCatchupBounds::worker().max_laps,
            super::TRASH_CATCHUP_MAX_LAPS
        );
    }

    /// While this repository's catch-up lane is live, the retirement run
    /// skips its own drain (counted, the trash left to the lane) instead of
    /// waiting for the deleter lock. Another repository's live lane does not
    /// serve this trash: the drain runs as before.
    #[test]
    fn the_retirement_drain_is_skipped_only_for_this_repositorys_lane() {
        let (owner, _storage, mut broker) = fixture();
        let other = trashed_day_old_entry(&mut broker, "other lane");
        let host = owner.path();
        let lane_path = host.join(super::TRASH_CATCHUP_LOCK_FILE);
        let foreign_root = owner.path().join("another-repository");

        let foreign = crate::checkout_trash::try_catchup_lane_lock(&lane_path, &foreign_root)
            .unwrap()
            .unwrap();
        assert_eq!(
            broker.checkout_trash_catchup_lane_state(host),
            super::CatchupLaneState::OtherRepository
        );
        let drained =
            broker.retirement_trash_drain(Some(host), Instant::now() + Duration::from_secs(600));
        assert!(!drained.skipped_for_catchup_lane, "{drained:?}");
        assert!(
            !other.exists(),
            "drained although another repository's lane is live"
        );
        drop(foreign);

        let entry = trashed_day_old_entry(&mut broker, "own lane");
        let own = crate::checkout_trash::try_catchup_lane_lock(&lane_path, broker.main_root())
            .unwrap()
            .unwrap();
        assert_eq!(
            broker.checkout_trash_catchup_lane_state(host),
            super::CatchupLaneState::ThisRepository
        );
        let started = Instant::now();
        let skipped =
            broker.retirement_trash_drain(Some(host), Instant::now() + Duration::from_secs(600));
        assert!(skipped.skipped_for_catchup_lane, "{skipped:?}");
        assert_eq!(skipped.end_reason(), "skipped_for_catchup_lane");
        assert_eq!(skipped.entries_removed, 0);
        assert!(skipped.entries_pending >= 1);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "no wait for the deleter lock"
        );
        assert!(entry.exists());
        drop(own);
        assert_eq!(
            broker.checkout_trash_catchup_lane_state(host),
            super::CatchupLaneState::Free
        );
        let drained =
            broker.retirement_trash_drain(Some(host), Instant::now() + Duration::from_secs(600));
        assert!(!drained.skipped_for_catchup_lane, "{drained:?}");
        assert!(!entry.exists());
    }

    /// A worker's lane that finds another repository's lane live notes that
    /// it waits and retries within its time bound instead of giving up; a
    /// holder that sees another repository waiting hands the lane over
    /// between laps instead of chaining the next one.
    #[test]
    fn a_lane_waits_for_another_repositorys_lane_and_the_holder_hands_over() {
        let (owner, _storage, mut broker) = fixture();
        let entry = trashed_day_old_entry(&mut broker, "waiting");
        let host = owner.path();
        let lane_path = host.join(super::TRASH_CATCHUP_LOCK_FILE);
        let waiter_path = host.join(super::TRASH_CATCHUP_WAITER_FILE);
        let foreign_root = owner.path().join("another-repository");

        let foreign = crate::checkout_trash::try_catchup_lane_lock(&lane_path, &foreign_root)
            .unwrap()
            .unwrap();
        let waited = broker.run_checkout_trash_catchup_laps(
            Some(host),
            super::TrashCatchupBounds {
                total: Duration::from_millis(2_500),
                lap_max: Duration::from_secs(60),
                max_laps: 3,
                until_empty: false,
                lane_retry: Some(Duration::from_millis(200)),
            },
            no_pressure,
        );
        assert!(waited.len() >= 2, "retried: {waited:?}");
        assert!(waited.iter().all(|lap| {
            lap.end == super::TrashCatchupEnd::LaneBusy && lap.lane_held_by_other_repository
        }));
        let note = std::fs::read_to_string(&waiter_path).unwrap();
        assert!(
            note.contains(&broker.main_root().display().to_string()),
            "{note}"
        );
        assert!(entry.exists());
        drop(foreign);

        // Its own lane clears its note; another repository's note makes a
        // capped lap the last one.
        let done = broker.run_checkout_trash_catchup_laps(
            Some(host),
            super::TrashCatchupBounds {
                total: Duration::from_secs(60),
                lap_max: Duration::ZERO,
                max_laps: 3,
                until_empty: false,
                lane_retry: None,
            },
            no_pressure,
        );
        assert_eq!(done.len(), 3, "{done:?}");
        assert!(!waiter_path.exists(), "the lane cleared its own note");
        std::fs::write(
            &waiter_path,
            format!("repository {}\n", foreign_root.display()),
        )
        .unwrap();
        let handed_over = broker.run_checkout_trash_catchup_laps(
            Some(host),
            super::TrashCatchupBounds {
                total: Duration::from_secs(60),
                lap_max: Duration::ZERO,
                max_laps: 3,
                until_empty: false,
                lane_retry: None,
            },
            no_pressure,
        );
        assert_eq!(handed_over.len(), 1, "{handed_over:?}");
        assert_eq!(handed_over[0].end, super::TrashCatchupEnd::Cap);
    }

    /// A catch-up slice stops at once, resumably, at a reading of critical
    /// memory pressure, instead of going on at the floor; the entry stays
    /// recorded and is not counted as failing.
    #[test]
    fn a_catchup_slice_yields_at_once_to_critical_memory_pressure() {
        let (owner, _storage, mut broker) = fixture();
        let entry = trashed_day_old_entry(&mut broker, "yield");
        let lock = owner.path().join(crate::checkout_trash::DELETER_LOCK_FILE);
        let drained = broker.drain_checkout_trash_with(
            Some(&lock),
            Instant::now() + Duration::from_secs(60),
            super::DrainOptions {
                lock_wait: Duration::ZERO,
                yield_on_memory_critical: true,
                sample: critical_memory,
                threads: 1,
            },
        );
        assert!(drained.yielded_to_pressure, "{drained:?}");
        assert_eq!(drained.end_reason(), "pressure");
        assert!(drained.failed.is_empty(), "{drained:?}");
        assert!(entry.exists());
        let rows = broker.checkout_trash_rows();
        let name = entry.file_name().unwrap().to_string_lossy().into_owned();
        let row = rows
            .values()
            .find_map(|entries| entries.get(&name))
            .unwrap();
        assert_eq!(row.failures, 0);
    }

    /// An entry a process still uses is passed over, not waited on: the
    /// entries behind it are deleted in the same run, and it is deleted by a
    /// later run once nothing uses it (its row stays recorded meanwhile).
    #[test]
    fn an_entry_in_use_never_blocks_the_entries_behind_it() {
        let (owner, _storage, mut broker) = fixture();
        let mut moved = Vec::new();
        for label in ["first in use", "second free"] {
            let (session, worktree) = closed(&mut broker, label, |_| {});
            let current = broker.store_ref().session(session.id).unwrap();
            let _lane = super::RetirementLaneScope::enter();
            broker
                .remove_checkout_through_trash(&current, &worktree, true)
                .unwrap();
            moved.push(session.id);
        }
        let entries = broker
            .checkout_trash_rows()
            .into_iter()
            .flat_map(|(dir, entries)| {
                entries
                    .into_iter()
                    .filter(|(_, row)| !row.stripped)
                    .map(move |(name, row)| (dir.join(name), row.session_id))
            })
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 2, "{entries:?}");
        // Rows are ordered by name, which is the drain's order: the first
        // is the one the drain reaches first.
        let (first, _) = entries[0].clone();
        let (second, _) = entries[1].clone();
        let inside = std::fs::canonicalize(&first).unwrap();
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

        let passed_over = broker.drain_checkout_trash_with_lock(Some(&lock), far);
        assert_eq!(
            (passed_over.entries_in_use, passed_over.entries_pending),
            (1, 1),
            "{passed_over:?}"
        );
        assert!(passed_over.failed.is_empty(), "{passed_over:?}");
        assert!(inside.exists(), "the entry in use is kept");
        assert!(!second.exists(), "the entry behind it went in the same run");
        let held_name = first.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(passed_over.in_use_names, vec![held_name.clone()]);
        // Recorded as passed over, it no longer counts toward "behind",
        // however old it is.
        broker
            .store_ref()
            .meta_set(
                CHECKOUT_RETIREMENT_PROGRESS_KEY,
                &serde_json::to_string(&CheckoutRetirementProgress {
                    trash_in_use_entries: passed_over.in_use_names.clone(),
                    ..CheckoutRetirementProgress::default()
                })
                .unwrap(),
            )
            .unwrap();
        let rows = broker.checkout_trash_rows();
        assert_eq!(broker.checkout_trash_deletable_backlog(&rows).entries, 0);
        assert_eq!(
            crate::checkout_trash::TrashBacklog::of(&rows).entries,
            1,
            "{held_name} still counts for the advice"
        );

        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
        let retried = broker.drain_checkout_trash_with_lock(Some(&lock), far);
        assert_eq!(retried.entries_pending, 0, "{retried:?}");
        assert!(!inside.exists(), "retried once nothing uses it");
        assert_eq!(moved.len(), 2);
    }

    /// The drain has its own share of the run: a third, half while the
    /// trash is behind. Phases that ran to the lane's deadline still leave
    /// it that whole share, and phases that end early leave it the rest.
    #[test]
    fn the_drain_keeps_its_own_budget() {
        let window = Duration::from_secs(180);
        let started = Instant::now();
        let deadline = started + window;
        assert_eq!(
            super::retirement_sweep_deadline(started, window, false),
            started + window * 2 / 3
        );
        assert_eq!(
            super::retirement_sweep_deadline(started, window, true),
            started + window / 2,
            "behind, the phases before the drain get half"
        );
        assert_eq!(
            super::trash_drain_deadline(deadline, started + Duration::from_secs(30), window, true),
            deadline,
            "phases that ended early leave the drain the rest of the lane"
        );
        assert_eq!(
            super::trash_drain_deadline(deadline, deadline, window, false),
            deadline + window / 3,
            "phases that overran still leave the drain its share"
        );
        assert_eq!(
            super::trash_drain_deadline(deadline, deadline, window, true),
            deadline + window / 2
        );
        assert_eq!(super::next_trash_growing_runs(2, 10, 11), 3);
        assert_eq!(super::next_trash_growing_runs(2, 10, 10), 2);
        assert_eq!(super::next_trash_growing_runs(2, 10, 9), 0);
    }

    /// `broker status` names a trash backlog a day old, or one that grew
    /// for several runs in a row, and stays quiet otherwise.
    #[test]
    fn status_names_a_trash_backlog_that_is_old_or_growing() {
        let (_owner, _storage, mut broker) = fixture();
        let mut has_advice = |progress: &CheckoutRetirementProgress| {
            broker
                .store_ref()
                .meta_set(
                    CHECKOUT_RETIREMENT_PROGRESS_KEY,
                    &serde_json::to_string(progress).unwrap(),
                )
                .unwrap();
            broker
                .status(crate::clock::epoch_ms())
                .unwrap()
                .advice
                .iter()
                .any(|advice| advice.id == "cleanup.trash-backlog")
        };
        let now = crate::clock::epoch_ms();
        let recent = CheckoutRetirementProgress {
            trash_pending: 40,
            trash_oldest_entry_ms: Some(now - 60_000),
            trash_growing_runs: 1,
            ..CheckoutRetirementProgress::default()
        };
        assert!(!has_advice(&recent));
        assert!(has_advice(&CheckoutRetirementProgress {
            trash_oldest_entry_ms: Some(now - crate::checkout_trash::TRASH_BACKLOG_AGE_MS - 1),
            ..recent.clone()
        }));
        assert!(has_advice(&CheckoutRetirementProgress {
            trash_growing_runs: super::TRASH_GROWING_RUNS_ADVICE_AT,
            ..recent.clone()
        }));
        assert!(!has_advice(&CheckoutRetirementProgress {
            trash_pending: 0,
            trash_growing_runs: super::TRASH_GROWING_RUNS_ADVICE_AT,
            ..recent
        }));
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
        assert!(retired_away(worktree));
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
        assert!(retired_away(&worktree));
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
        assert!(retired_away(&worktree));
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
        assert!(retired_away(&worktree));
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
                    scratch: false,
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
        assert!(retired_away(&worktree));
        assert!(is_cleaned(&broker, session.id));
    }

    /// `start --reuse` on a retired checkout recreates it from the session
    /// branch; one with archived leftovers is restored from its archive.
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
        assert!(super::super::tombstone::is_checkout_tombstone(&dirty_path));
        assert!(broker.recreate_retired_checkout(&dirty_path).unwrap());
        assert_eq!(
            std::fs::read_to_string(dirty_path.join("notes.txt")).unwrap(),
            "leftover\n"
        );
        // Idempotent: a checkout that is back is left alone.
        assert!(!broker.recreate_retired_checkout(&dirty_path).unwrap());
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
        assert!(retired_away(&worktree));
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
        // Bytes are the volume's free-space change across each removal (no
        // per-file stat), which other writers on a shared test volume can
        // hide for a tree this small, so the unlinks are checked instead.
        assert!(drained.unlinks >= 8, "{drained:?}");
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
        assert!(retired_away(&worktree));
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
        assert!(retired_away(&worktree));
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
        assert!(retired_away(&worktree));
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
        assert!(retired_away(&worktree));
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
        assert!(retired_away(&worktree));
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

    /// A folder is remembered as absent only on ENOENT with its pool
    /// listable: in a pool that cannot be listed (an unmounted or
    /// unreachable volume reads the same) an absent folder is not
    /// remembered, and an unreadable one is `checkout_unreadable`. A
    /// remembered folder seen again in its pool's listing is judged again
    /// and the memory dropped.
    #[test]
    fn only_a_proven_absent_folder_is_remembered_and_seeing_it_again_drops_it() {
        use std::os::unix::fs::PermissionsExt;
        let (owner, _storage, mut broker) = fixture();
        let (gone, gone_worktree) = closed(&mut broker, "absent folder", |_| {});
        let (hidden, _) = closed(&mut broker, "pool not listable", |_| {});
        let (unreadable, _) = closed(&mut broker, "pool not searchable", |_| {});
        // Search but no read permission: the folder's absence is ENOENT, but
        // the pool cannot be listed.
        let search_only = owner.path().join("search-only-pool");
        std::fs::create_dir_all(&search_only).unwrap();
        // Neither: nothing in it can be read.
        let locked = owner.path().join("locked-pool");
        std::fs::create_dir_all(&locked).unwrap();
        for (session, path) in [
            (&hidden, search_only.join("checkout")),
            (&unreadable, locked.join("checkout")),
        ] {
            broker
                .store()
                .connection()
                .execute(
                    "UPDATE sessions SET worktree_path = ?1 WHERE id = ?2",
                    rusqlite::params![path.to_string_lossy(), session.id],
                )
                .unwrap();
        }
        std::fs::set_permissions(&search_only, std::fs::Permissions::from_mode(0o100)).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let first = strip_pass(&broker);
        std::fs::set_permissions(&search_only, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            first.refused.get("checkout_missing"),
            Some(&1),
            "{first:#?}"
        );
        assert_eq!(
            first.refused.get("checkout_unreadable"),
            Some(&1),
            "{first:#?}"
        );
        let absent: std::collections::BTreeMap<i64, i64> =
            broker.meta_json(super::super::strip::CHECKOUT_STRIP_ABSENT_KEY);
        assert!(absent.is_empty(), "{absent:?}");

        // Remembered as absent, then the folder shows up again.
        let closed_at = broker
            .store_ref()
            .session(gone.id)
            .unwrap()
            .closed_at
            .unwrap_or(0);
        broker
            .update_meta_json(
                super::super::strip::CHECKOUT_STRIP_ABSENT_KEY,
                |stored: &mut std::collections::BTreeMap<i64, i64>| {
                    stored.insert(gone.id, closed_at);
                },
            )
            .unwrap();
        broker
            .update_meta_json(
                super::super::strip::CHECKOUT_STRIP_DONE_KEY,
                |stored: &mut std::collections::BTreeMap<i64, i64>| {
                    stored.remove(&gone.id);
                },
            )
            .unwrap();
        assert!(gone_worktree.is_dir());

        let again = strip_pass(&broker);

        assert!(again.checkouts_examined >= 1, "{again:#?}");
        let absent: std::collections::BTreeMap<i64, i64> =
            broker.meta_json(super::super::strip::CHECKOUT_STRIP_ABSENT_KEY);
        assert!(!absent.contains_key(&gone.id), "{absent:?}");
    }

    /// A live holder (same pid and start time) keeps a closed checkout only
    /// while it has not moved on: once the same process is bound to a newer
    /// session after this one closed, the strip pass strips it. A process in
    /// the checkout still keeps it.
    #[test]
    fn a_holder_that_moved_on_to_a_newer_session_does_not_keep_a_closed_checkout() {
        let Some(table) = crate::session_holder::ProcessTable::snapshot() else {
            return;
        };
        let Some(me) = table.process(i64::from(std::process::id())) else {
            return;
        };
        let (_owner, _storage, mut broker) = fixture();
        let (older, older_worktree) = closed_with_dependencies(&mut broker, "older task");
        let bind = |broker: &mut crate::Broker, session_id: i64| {
            broker
                .store()
                .append_event(
                    crate::events::SESSION_HOLDER_BOUND,
                    Some(session_id),
                    Some(&crate::events::session_holder_bound_payload(
                        &me, "fixture", None,
                    )),
                )
                .unwrap();
        };
        bind(&mut broker, older.id);

        let kept = strip_pass(&broker);
        assert_eq!(kept.refused.get("holder_alive"), Some(&1), "{kept:#?}");
        assert!(older_worktree.join("node_modules").exists());

        std::thread::sleep(Duration::from_millis(20));
        let newer = broker.start_worktree("newer task", None).unwrap();
        bind(&mut broker, newer.id);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "node".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: std::fs::canonicalize(&older_worktree).unwrap(),
            }]),
        ));
        let in_use = strip_pass(&broker);
        assert_eq!(
            in_use.refused.get("process_using_checkout"),
            Some(&1),
            "{in_use:#?}"
        );
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));

        let stripped = strip_pass(&broker);

        assert_eq!(stripped.checkouts_stripped, 1, "{stripped:#?}");
        assert!(!older_worktree.join("node_modules").exists());
    }

    /// The retirement run sweeps stale reclaim-plan review snapshots from
    /// the pool root, as the plan writer does, and keeps fresh ones.
    #[test]
    fn the_retirement_run_prunes_stale_reclaim_plan_snapshots() {
        let (_owner, _storage, mut broker) = fixture();
        // A first session creates and binds the worktree root.
        let (_session, _worktree) = closed(&mut broker, "binds the root", |_| {});
        let root = broker
            .require_bound_repository_worktree_root("test")
            .unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let snapshot = |fill: char, age_days: u64| {
            let path = root.join(format!(
                ".aethyme-reclaim-plan-{}.json",
                fill.to_string().repeat(64)
            ));
            std::fs::write(&path, "{}").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(age_days * 86_400))
                .unwrap();
            path
        };
        let old = snapshot('a', 8);
        let fresh = snapshot('b', 0);

        run(&mut broker);

        assert!(!old.exists());
        assert!(fresh.exists());
    }

    /// End to end: a checkout holding an unpushed commit, a staged change,
    /// an unstaged change, an untracked file and an ignored local file is
    /// archived and removed, and reusing it brings back the same tree from
    /// the archive; the archive stays.
    #[test]
    fn a_retired_archived_checkout_is_restored_for_reuse() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "restore me", |worktree| {
            std::fs::write(worktree.join("work.txt"), "committed, not pushed\n").unwrap();
            git(worktree, &["add", "work.txt"]);
            git(
                worktree,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "commit",
                    "-qm",
                    "unpushed work",
                ],
            );
            std::fs::write(worktree.join("work.txt"), "staged change\n").unwrap();
            git(worktree, &["add", "work.txt"]);
            std::fs::write(worktree.join("base.txt"), "unstaged change\n").unwrap();
            std::fs::write(worktree.join("notes.txt"), "untracked notes\n").unwrap();
            std::fs::write(worktree.join("secrets.local"), "local only\n").unwrap();
        });
        let head = git(&worktree, &["rev-parse", "HEAD"]).trim().to_string();

        let report = run(&mut broker);

        assert_eq!(report.retired_archived, 1, "{report:#?}");
        // The path does not vanish: a tombstone says what happened and how
        // to bring it back, and every lane recognises it.
        assert!(super::super::tombstone::is_checkout_tombstone(&worktree));
        let note = std::fs::read_to_string(worktree.join(super::super::tombstone::TOMBSTONE_FILE))
            .unwrap();
        assert!(note.contains("--reuse"), "{note}");
        assert!(note.contains("archive: "), "{note}");
        // Asked afresh (the earlier pass remembered the session as handled),
        // the strip pass keeps the tombstone as such.
        broker
            .update_meta_json(
                super::super::strip::CHECKOUT_STRIP_DONE_KEY,
                |stored: &mut std::collections::BTreeMap<i64, i64>| {
                    stored.remove(&session.id);
                },
            )
            .unwrap();
        let strip = strip_pass(&broker);
        assert_eq!(strip.refused.get("tombstone"), Some(&1), "{strip:#?}");
        let again = run(&mut broker);
        assert_eq!(again.examined, 0, "{again:#?}");
        assert!(again.kept.is_empty(), "{again:#?}");
        assert!(super::super::tombstone::is_checkout_tombstone(&worktree));
        // A command naming the archived session answers with the restore
        // action and its command.
        let refusal =
            crate::session_auto_close::closed_session_refusal(broker.store_ref(), session.id);
        assert!(refusal.contains("restore_required"), "{refusal}");
        // ... and as data for `--json` callers.
        let action = crate::session_auto_close::take_refusal_action()
            .expect("the refusal leaves its structured action");
        assert_eq!(action["code"], "restore_required");
        assert_eq!(action["session_id"], session.id);
        assert_eq!(
            action["command"],
            crate::broker::restore_command(&worktree).as_str()
        );
        assert!(refusal.contains("--reuse"), "{refusal}");
        // A new session with the same task never gets the tombstone's path.
        let fresh = broker.start_worktree("restore me", None).unwrap();
        assert_ne!(PathBuf::from(&fresh.worktree_path), worktree);
        // The session record says it has something to restore.
        assert!(broker.session_restorable_archive(session.id).is_some());
        // Its local branch was deleted meanwhile: the restore recreates it
        // from the tip the archive recorded.
        if git_status(
            broker.main_root(),
            &["rev-parse", "--verify", "--quiet", &session.branch],
        ) {
            git(broker.main_root(), &["branch", "-D", &session.branch]);
        }
        assert!(broker.recreate_retired_checkout(&worktree).unwrap());

        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).trim(), head);
        assert_eq!(
            git(&worktree, &["symbolic-ref", "--short", "HEAD"]).trim(),
            session.branch
        );
        assert_eq!(
            git(&worktree, &["show", ":work.txt"]),
            "staged change\n",
            "the staged change is staged again"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("base.txt")).unwrap(),
            "unstaged change\n"
        );
        assert_eq!(
            git(&worktree, &["diff", "--cached", "--name-only"]).trim(),
            "work.txt"
        );
        assert_eq!(git(&worktree, &["diff", "--name-only"]).trim(), "base.txt");
        assert_eq!(
            std::fs::read_to_string(worktree.join("notes.txt")).unwrap(),
            "untracked notes\n"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("secrets.local")).unwrap(),
            "local only\n"
        );
        assert!(
            !archives(&broker, session.id).is_empty(),
            "the archive stays"
        );
    }

    /// A checkout holding an unpushed commit and an untracked file, retired
    /// through an archive; returns the session, its path and archived HEAD.
    fn retired_with_archive(
        broker: &mut crate::Broker,
        label: &str,
    ) -> (crate::Session, PathBuf, String) {
        let (session, worktree) = closed(broker, label, |worktree| {
            std::fs::write(worktree.join("work.txt"), "committed, not pushed\n").unwrap();
            git(worktree, &["add", "work.txt"]);
            git(
                worktree,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "commit",
                    "-qm",
                    "unpushed work",
                ],
            );
            std::fs::write(worktree.join("notes.txt"), "untracked notes\n").unwrap();
        });
        let head = git(&worktree, &["rev-parse", "HEAD"]).trim().to_string();
        let report = run(broker);
        assert_eq!(report.retired_archived, 1, "{report:#?}");
        (session, worktree, head)
    }

    /// New archives go to the pool's volume, in the marked broker-owned
    /// directory beside the checkouts; an archive left in host state stays
    /// found; an operation archive is never picked for a restore.
    #[test]
    fn retirement_archives_on_the_pool_and_still_finds_host_state_ones() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree, _) = retired_with_archive(&mut broker, "pool archive");
        let pool_root = broker
            .pool_recovery_archive_root(false)
            .expect("the pool-side archive root exists, identified by its marker");
        let archive = broker
            .newest_recovery_archive(session.id)
            .expect("the archive is found");
        assert!(archive.starts_with(&pool_root), "{}", archive.display());
        assert!(
            pool_root
                .parent()
                .unwrap()
                .join(".aethyme-recovery-archives-root.json")
                .is_file()
        );
        assert_eq!(
            std::fs::canonicalize(pool_root.parent().unwrap().parent().unwrap()).unwrap(),
            std::fs::canonicalize(worktree.parent().unwrap()).unwrap(),
            "beside the checkouts, on the pool's volume"
        );
        // An older archive in host state, and an operation archive.
        let host = broker.recovery_archive_root().unwrap();
        let legacy = host.join(format!("{}-legacy", 990_001));
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join(crate::cleanup_resolve::MANIFEST_FILE), "{}").unwrap();
        assert_eq!(broker.newest_recovery_archive(990_001), Some(legacy));
        let operation = pool_root.join(format!("{}-operation-0123456789ab-1", 990_002));
        std::fs::create_dir_all(&operation).unwrap();
        std::fs::write(operation.join(crate::cleanup_resolve::MANIFEST_FILE), "{}").unwrap();
        assert_eq!(broker.newest_recovery_archive(990_002), None);
        assert_eq!(broker.newest_operation_archive(990_002), Some(operation));
    }

    /// The archive bound never removes an archive its owner kept with
    /// `storage keep-archive`.
    #[test]
    fn the_archive_bound_never_removes_an_archive_its_owner_kept() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, _, _) = retired_with_archive(&mut broker, "kept archive");
        let archive = broker
            .newest_recovery_archive(session.id)
            .expect("the archive");
        std::fs::write(crate::storage_ledger::keep_marker(&archive), "kept\n").unwrap();
        let owned = broker.owned_recovery_archive(&archive).unwrap();
        let refused = broker
            .evict_landed_archive(&owned, Instant::now() + Duration::from_secs(60))
            .unwrap_err();
        assert!(refused.contains("kept by the owner's marker"), "{refused}");
        assert!(
            archive
                .join(crate::cleanup_resolve::MANIFEST_FILE)
                .is_file()
        );
    }

    /// The bound pass lists, sizes and classifies archives from metadata
    /// alone: archives it does not remove are never fully verified, and the
    /// archive roots are inspected once per pass, not once per archive.
    #[test]
    fn the_archive_bound_never_fully_verifies_archives_it_does_not_remove() {
        let (_owner, _storage, mut broker) = fixture();
        for label in ["bound one", "bound two", "bound three"] {
            retired_with_archive(&mut broker, label);
        }
        let recorder = crate::phase_telemetry::Recorder::default();
        let verified_before =
            crate::cleanup_resolve::FULL_ARCHIVE_VERIFICATIONS.with(std::cell::Cell::get);
        let usage = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            broker.enforce_recovery_archive_bound(Instant::now() + Duration::from_secs(60))
        };
        let verified_after =
            crate::cleanup_resolve::FULL_ARCHIVE_VERIFICATIONS.with(std::cell::Cell::get);
        assert_eq!(
            verified_after, verified_before,
            "no archive the pass keeps is fully verified"
        );
        let pass = &recorder.attachments()["recovery_archive_bound_pass"];
        assert_eq!(pass["listed"], 3, "{pass}");
        assert_eq!(pass["listing_skipped_by_budget"], 0, "{pass}");
        assert_eq!(pass["budget_stops"], 0, "{pass}");
        assert!(!usage.partial, "{usage:?}");
        assert_eq!(usage.pool.archives + usage.host_state.archives, 3);
        let (_, counts) = recorder.totals();
        assert_eq!(
            counts.get("recovery_archive_roots_inspect").copied(),
            Some(1),
            "{counts:?}"
        );
    }

    /// A pass out of budget stops, says so, and leaves the rest for later;
    /// sizes measured by an earlier pass are the cursor the next pass
    /// resumes from, so a backlog is measured once, not every run.
    #[test]
    fn the_archive_bound_respects_its_budget_and_resumes_from_measured_sizes() {
        let (_owner, _storage, mut broker) = fixture();
        for label in ["budget one", "budget two"] {
            retired_with_archive(&mut broker, label);
        }
        // Forget what the worker's own passes measured.
        let host_state = broker.storage_context().host_state_dir.clone().unwrap();
        let repository = broker.main_root().to_string_lossy().into_owned();
        crate::storage_ledger::record_archive_bound_state(
            &host_state,
            &repository,
            &crate::storage_ledger::ArchiveBoundState::default(),
        )
        .unwrap();

        // Out of budget before it starts: nothing listed, nothing walked.
        let recorder = crate::phase_telemetry::Recorder::default();
        let usage = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            broker.enforce_recovery_archive_bound(Instant::now())
        };
        assert!(usage.partial, "{usage:?}");
        let pass = &recorder.attachments()["recovery_archive_bound_pass"];
        assert_eq!(pass["listed"], 0, "{pass}");
        assert_eq!(pass["listing_skipped_by_budget"], 2, "{pass}");
        assert_eq!(pass["size_measured"], 0, "{pass}");
        assert_eq!(pass["budget_stops"], 1, "{pass}");

        // With budget: both measured and recorded.
        let recorder = crate::phase_telemetry::Recorder::default();
        let usage = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            broker.enforce_recovery_archive_bound(Instant::now() + Duration::from_secs(60))
        };
        assert!(!usage.partial, "{usage:?}");
        let pass = &recorder.attachments()["recovery_archive_bound_pass"];
        assert_eq!(pass["size_measured"], 2, "{pass}");
        assert_eq!(
            crate::storage_ledger::archive_bound_state(&host_state, &repository)
                .sizes
                .len(),
            2
        );

        // The next pass resumes from them: nothing is walked again.
        let recorder = crate::phase_telemetry::Recorder::default();
        let again = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            broker.enforce_recovery_archive_bound(Instant::now() + Duration::from_secs(60))
        };
        let pass = &recorder.attachments()["recovery_archive_bound_pass"];
        assert_eq!(pass["size_measured"], 0, "{pass}");
        assert_eq!(pass["size_cached"], 2, "{pass}");
        assert_eq!(
            again.pool.bytes + again.host_state.bytes,
            usage.pool.bytes + usage.host_state.bytes
        );
    }

    /// A checkout holding only a commit not yet on the default branch,
    /// retired through an archive, whose commit then lands on the primary
    /// checkout: the archive holds nothing that exists nowhere else.
    /// Returns the archive.
    fn retired_then_landed(broker: &mut crate::Broker, label: &str) -> PathBuf {
        let (session, worktree) = closed(broker, label, |worktree| {
            let name = format!("{}.txt", label.replace(' ', "-"));
            std::fs::write(worktree.join(&name), "landed later\n").unwrap();
            git(worktree, &["add", &name]);
            git(
                worktree,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "commit",
                    "-qm",
                    "work that lands",
                ],
            );
        });
        let head = git(&worktree, &["rev-parse", "HEAD"]).trim().to_string();
        let report = run(broker);
        assert_eq!(report.retired_archived, 1, "{report:#?}");
        let root = broker.main_root().to_path_buf();
        git(&root, &["merge", "-q", "--no-ff", "-m", "land", &head]);
        broker
            .newest_recovery_archive(session.id)
            .expect("the archive")
    }

    /// One bound pass with the test hooks set, returning its usage and the
    /// pass telemetry.
    fn bound_pass(
        broker: &crate::Broker,
        skew_days: i64,
        extra_skew_ms: i64,
    ) -> (
        crate::storage_ledger::RecoveryArchiveUsage,
        serde_json::Value,
    ) {
        super::super::archive_bound::CLOCK_SKEW_MS_FOR_TEST
            .with(|skew| skew.set(skew_days * 86_400_000 + extra_skew_ms));
        super::super::archive_bound::BOUND_BYTES_FOR_TEST.with(|bound| bound.set(Some(1)));
        let recorder = crate::phase_telemetry::Recorder::default();
        let usage = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            broker.enforce_recovery_archive_bound(Instant::now() + Duration::from_secs(120))
        };
        super::super::archive_bound::CLOCK_SKEW_MS_FOR_TEST.with(|skew| skew.set(0));
        super::super::archive_bound::BOUND_BYTES_FOR_TEST.with(|bound| bound.set(None));
        let pass = recorder.attachments()["recovery_archive_bound_pass"].clone();
        (usage, pass)
    }

    fn full_proofs() -> usize {
        crate::cleanup_resolve::FULL_ARCHIVE_VERIFICATIONS.with(std::cell::Cell::get)
    }

    /// Over the bound, the pass removes landed archives, fully proving
    /// exactly the archives it removes and no other.
    #[test]
    fn an_over_bound_pass_fully_proves_exactly_the_archives_it_removes() {
        let (_owner, _storage, mut broker) = fixture();
        let first = retired_then_landed(&mut broker, "evict one");
        let second = retired_then_landed(&mut broker, "evict two");
        retired_with_archive(&mut broker, "keeps unique work");
        let before = full_proofs();
        let (usage, pass) = bound_pass(&broker, 30, 0);
        assert_eq!(usage.evicted, 2, "{usage:?} {pass}");
        assert_eq!(full_proofs() - before, usage.evicted, "{pass}");
        assert_eq!(pass["proofs"], 2, "{pass}");
        assert!(!first.exists() && !second.exists());
        // The archive holding unique work stays, and is named.
        assert!(usage.over_bound, "{usage:?}");
        assert_eq!(usage.unique_kept_over_bound, 1, "{usage:?}");
    }

    /// An archive whose manifest says landed but whose content changed is
    /// never removed by the bound: the full proof refuses it, and later
    /// passes rotate past it instead of proving it again every run.
    #[test]
    fn the_bound_never_removes_an_archive_whose_content_was_altered() {
        let (_owner, _storage, mut broker) = fixture();
        let archive = retired_then_landed(&mut broker, "altered");
        std::fs::write(archive.join("added-after.txt"), "not in the manifest\n").unwrap();
        let before = full_proofs();
        let (usage, pass) = bound_pass(&broker, 30, 0);
        assert_eq!(usage.evicted, 0, "{usage:?} {pass}");
        assert_eq!(pass["proof_failures"], 1, "{pass}");
        assert_eq!(full_proofs() - before, 1);
        assert!(
            archive
                .join(crate::cleanup_resolve::MANIFEST_FILE)
                .is_file()
        );
        assert!(archive.join("added-after.txt").is_file());
        // The next pass passes over it until its retry is due.
        let (again, pass) = bound_pass(&broker, 30, 0);
        assert_eq!(again.evicted, 0, "{pass}");
        assert_eq!(pass["proofs"], 0, "{pass}");
        assert_eq!(pass["proof_deferred_skipped"], 1, "{pass}");
        assert!(archive.join("added-after.txt").is_file());
    }

    /// A removal proof that runs out of budget leaves its archive intact,
    /// does not hold back the archives behind it, is passed over by the
    /// next pass, and is retried first with a fresh budget once due.
    #[test]
    fn a_proof_out_of_budget_is_rotated_past_and_retried_later() {
        let (_owner, _storage, mut broker) = fixture();
        let older = retired_then_landed(&mut broker, "proof times out");
        let newer = retired_then_landed(&mut broker, "proof behind it");
        super::super::archive_bound::EXPIRED_PROOFS_FOR_TEST.with(|left| left.set(1));
        let (usage, pass) = bound_pass(&broker, 30, 0);
        super::super::archive_bound::EXPIRED_PROOFS_FOR_TEST.with(|left| left.set(0));
        assert_eq!(pass["proof_timeouts"], 1, "{pass}");
        assert_eq!(usage.evicted, 1, "{usage:?} {pass}");
        assert!(
            older.join(crate::cleanup_resolve::MANIFEST_FILE).is_file(),
            "the archive whose proof timed out is intact"
        );
        assert!(!newer.exists(), "the archive behind it was still removed");
        // Not due yet: passed over.
        let (again, pass) = bound_pass(&broker, 30, 0);
        assert_eq!(again.evicted, 0, "{pass}");
        assert_eq!(pass["proof_deferred_skipped"], 1, "{pass}");
        assert!(older.join(crate::cleanup_resolve::MANIFEST_FILE).is_file());
        // Due: retried first, with a fresh budget, and removed.
        let (later, pass) = bound_pass(&broker, 30, 2 * 60 * 60 * 1_000);
        assert_eq!(pass["proof_retries"], 1, "{pass}");
        assert_eq!(later.evicted, 1, "{later:?} {pass}");
        assert!(!older.exists());
    }

    /// A listing resumes after the archive an interrupted listing reached
    /// last, and wraps, so every archive is reached over several runs.
    #[test]
    fn an_interrupted_listing_resumes_after_its_cursor() {
        let (_owner, _storage, mut broker) = fixture();
        for label in ["cursor one", "cursor two", "cursor three"] {
            retired_with_archive(&mut broker, label);
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let full = broker.recovery_archive_listing(deadline, None).unwrap();
        let order = full
            .owned
            .iter()
            .map(|archive| archive.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(order.len(), 3);
        assert_eq!(full.last_listed.as_ref(), Some(&order[2]));
        assert!(full.not_listed.is_empty());
        let resumed = broker
            .recovery_archive_listing(deadline, Some(&order[0]))
            .unwrap();
        let resumed_order = resumed
            .owned
            .iter()
            .map(|archive| archive.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            resumed_order,
            vec![order[1].clone(), order[2].clone(), order[0].clone()]
        );
        // Out of budget: nothing listed, everything left for the next run.
        let stopped = broker
            .recovery_archive_listing(Instant::now(), Some(&order[1]))
            .unwrap();
        assert!(stopped.owned.is_empty());
        assert_eq!(
            stopped.not_listed,
            vec![order[2].clone(), order[0].clone(), order[1].clone()]
        );
    }

    /// A restore whose cleanup fails leaves nothing at the session's path
    /// (it was built at a sibling staging path) and a marker that refuses
    /// any reuse there until the leftovers are removed.
    #[test]
    fn a_failed_restore_whose_undo_fails_blocks_reuse_until_resolved() {
        let (_owner, _storage, mut broker) = fixture();
        let (_session, worktree, _) = retired_with_archive(&mut broker, "failed undo");
        crate::cleanup_resolve::FAIL_AFTER_ADD_FOR_TEST.with(|cell| cell.set(true));
        crate::cleanup_resolve::FAIL_UNDO_FOR_TEST.with(|cell| cell.set(true));
        let failed = broker.recreate_retired_checkout(&worktree);
        crate::cleanup_resolve::FAIL_AFTER_ADD_FOR_TEST.with(|cell| cell.set(false));
        crate::cleanup_resolve::FAIL_UNDO_FOR_TEST.with(|cell| cell.set(false));
        let error = failed
            .expect_err("the injected failure refuses")
            .to_string();
        assert!(error.contains("could not be removed"), "{error}");
        // The session's path holds no partial tree: the tombstone is back.
        assert!(super::super::tombstone::is_checkout_tombstone(&worktree));
        let staging = crate::cleanup_resolve::restore_staging_path(&worktree);
        assert!(
            staging.join(".git").exists(),
            "the partial checkout is aside"
        );
        assert!(crate::cleanup_resolve::restore_incomplete(&worktree));
        // Reuse is refused while the marker is there.
        let refused = broker
            .recreate_retired_checkout(&worktree)
            .expect_err("an incomplete restore blocks reuse")
            .to_string();
        assert!(refused.contains("did not complete"), "{refused}");
        // Resolved by hand: the next reuse restores.
        git(
            broker.main_root(),
            &["worktree", "remove", "--force", &staging.to_string_lossy()],
        );
        std::fs::remove_file(crate::cleanup_resolve::restore_incomplete_marker(&worktree)).unwrap();
        assert!(broker.recreate_retired_checkout(&worktree).unwrap());
        assert!(worktree.join("notes.txt").is_file());
        assert!(!crate::cleanup_resolve::restore_incomplete(&worktree));
    }

    /// After a restore the session's preparation reads stale, the session
    /// gets the same session-scoped blocker as an idle strip, and the
    /// command's output names the prepare command.
    #[test]
    fn a_restore_leaves_the_prepare_signal_and_next_action() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree) = closed(&mut broker, "restore prepares", |worktree| {
            std::fs::create_dir_all(worktree.join(".aethyme")).unwrap();
            std::fs::write(
                worktree.join(".aethyme/prepare.toml"),
                "schema_version = 1\n\n[[steps]]\nname = \"deps\"\ncommand = [\"sh\", \"-c\", \"true\"]\n\
                 inputs = [\"lock.file\"]\noutputs = [\"node_modules/\"]\ncache = \"repository_shared\"\n\
                 required_for_hooks = false\n",
            )
            .unwrap();
            git(worktree, &["add", "-f", ".aethyme/prepare.toml"]);
            git(
                worktree,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "commit",
                    "-qm",
                    "declare preparation",
                ],
            );
            std::fs::write(worktree.join("notes.txt"), "untracked notes\n").unwrap();
        });
        let report = run(&mut broker);
        assert_eq!(report.retired_archived, 1, "{report:#?}");
        crate::broker::take_last_checkout_restore();
        assert!(broker.recreate_retired_checkout(&worktree).unwrap());
        let restored = crate::broker::take_last_checkout_restore().expect("a restore report");
        let command = format!("aethyme broker submit prepare --session {}", session.id);
        assert_eq!(restored.next_action.as_deref(), Some(command.as_str()));
        assert!(
            restored.lines().iter().any(|line| line.contains(&command)),
            "{:?}",
            restored.lines()
        );
        assert!(crate::preparation::stripped_since_prepared(
            broker.main_root(),
            session.id
        ));
    }

    /// When the branch moved after the archive was written, the restored
    /// checkout is detached at the archived HEAD and the command's output
    /// says so, with the branch tip and the commands to reconcile.
    #[test]
    fn a_restore_onto_a_moved_branch_reports_the_detached_checkout() {
        let (_owner, _storage, mut broker) = fixture();
        let (session, worktree, head) = retired_with_archive(&mut broker, "moved branch");
        let elsewhere = git(broker.main_root(), &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        assert_ne!(elsewhere, head);
        git(
            broker.main_root(),
            &["branch", "-f", &session.branch, &elsewhere],
        );
        crate::broker::take_last_checkout_restore();
        assert!(broker.recreate_retired_checkout(&worktree).unwrap());
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).trim(), head);
        let restored = crate::broker::take_last_checkout_restore().expect("a restore report");
        let detached = restored.detached.clone().expect("detached");
        assert_eq!(detached.branch_tip, elsewhere);
        assert_eq!(detached.archived_head, head);
        assert!(detached.reconcile_command.contains("switch -c"));
        let lines = restored.lines();
        assert!(lines[0].contains("DETACHED"), "{lines:?}");
        let value = serde_json::to_value(&restored).unwrap();
        assert_eq!(value["detached"]["branch_tip"], elsewhere.as_str());
    }

    /// An idle open session whose preparation declares the top-level
    /// `node_modules/` and a file inside a Python environment loses the
    /// proved environment root and the proved pnpm workspace members with
    /// the root; what the closed-checkout proofs refuse is kept with their
    /// reason; preparation then reads not current. With the top-level
    /// `node_modules` unprovable, members are kept as `member_root_kept`.
    #[test]
    fn the_idle_strip_takes_environment_roots_and_members_with_their_root() {
        let (_owner, _storage, mut broker) = fixture();
        exclude_node_modules(&broker);
        std::fs::write(
            broker.main_root().join(".git/info/exclude"),
            "node_modules/\n.venv/\n",
        )
        .unwrap();
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
        let prepare = "schema_version = 1\n\n[[steps]]\nname = \"js\"\ncommand = [\"sh\", \"-c\", \"true\"]\n\
             inputs = [\"lock.file\"]\noutputs = [\"node_modules/\"]\ncache = \"repository_shared\"\n\
             required_for_hooks = false\n\n[[steps]]\nname = \"py\"\ncommand = [\"sh\", \"-c\", \"true\"]\n\
             inputs = [\"lock.file\"]\noutputs = [\"tools/.venv/bin/tool\", \"tools/.venv/bin/python\", \"bad/.venv/bin/tool\", \"bad/.venv/bin/python\", \"pip/.venv/bin/ruff\"]\n\
             cache = \"repository_shared\"\nrequired_for_hooks = false\n";
        let session_with = |broker: &mut crate::Broker, task: &str, root_marker: bool| {
            let open = broker.start_worktree(task, None).unwrap();
            let worktree = PathBuf::from(&open.worktree_path);
            std::fs::create_dir_all(worktree.join(".aethyme")).unwrap();
            std::fs::write(worktree.join(".aethyme/prepare.toml"), prepare).unwrap();
            // The top-level install, with pnpm's shim in its `.bin` (its
            // `.pnpm` store or `.modules.yaml` is what proves it).
            if root_marker {
                std::fs::create_dir_all(worktree.join("node_modules/.pnpm/dep@1/node_modules/dep"))
                    .unwrap();
            }
            std::fs::create_dir_all(worktree.join("node_modules/.bin")).unwrap();
            std::fs::write(worktree.join("node_modules/.bin/tsc"), "#!/bin/sh\n").unwrap();
            if root_marker {
                std::fs::write(
                    worktree.join("node_modules/.modules.yaml"),
                    "layoutVersion: 5\n",
                )
                .unwrap();
            }
            // A pnpm workspace member: links into the store and a shim.
            let member = worktree.join("apps/a/node_modules");
            std::fs::create_dir_all(member.join(".bin")).unwrap();
            std::os::unix::fs::symlink(
                "../../../node_modules/.pnpm/dep@1/node_modules/dep",
                member.join("dep"),
            )
            .unwrap();
            std::fs::write(member.join(".bin/tsc"), "#!/bin/sh\n").unwrap();
            // A member holding a file of its own: not a proved member.
            std::fs::create_dir_all(worktree.join("apps/b/node_modules")).unwrap();
            std::fs::write(worktree.join("apps/b/node_modules/cache.js"), "x\n").unwrap();
            // A Python environment containing a declared output.
            std::fs::create_dir_all(worktree.join("tools/.venv/bin")).unwrap();
            std::fs::write(worktree.join("tools/.venv/pyvenv.cfg"), "home = /usr\n").unwrap();
            std::fs::write(worktree.join("tools/.venv/bin/tool"), "#!/bin/sh\n").unwrap();
            // One holding something besides the environment layout.
            std::fs::create_dir_all(worktree.join("bad/.venv/bin")).unwrap();
            std::fs::write(worktree.join("bad/.venv/pyvenv.cfg"), "home = /usr\n").unwrap();
            std::fs::write(worktree.join("bad/.venv/bin/tool"), "#!/bin/sh\n").unwrap();
            std::fs::write(worktree.join("bad/.venv/notes.txt"), "mine\n").unwrap();
            // An environment a step only installs into (its interpreter is
            // not a declared output): creation is not proved.
            std::fs::create_dir_all(worktree.join("pip/.venv/bin")).unwrap();
            std::fs::write(worktree.join("pip/.venv/pyvenv.cfg"), "home = /usr\n").unwrap();
            std::fs::write(worktree.join("pip/.venv/bin/ruff"), "#!/bin/sh\n").unwrap();
            broker
                .store()
                .connection()
                .execute(
                    "UPDATE sessions SET last_activity_at = ?1 WHERE id = ?2",
                    rusqlite::params![crate::clock::epoch_ms() - 5 * 3_600_000, open.id],
                )
                .unwrap();
            crate::worktree_status_activity::age_tree_for_test(
                &worktree,
                std::time::SystemTime::now() - Duration::from_secs(10 * 3_600),
            );
            let admin = PathBuf::from(git(&worktree, &["rev-parse", "--absolute-git-dir"]).trim());
            for name in ["index", "HEAD"] {
                std::fs::File::options()
                    .write(true)
                    .open(admin.join(name))
                    .unwrap()
                    .set_modified(std::time::SystemTime::now() - Duration::from_secs(10 * 3_600))
                    .unwrap();
            }
            (open, worktree)
        };
        let pass = |broker: &mut crate::Broker| {
            let far = Instant::now() + Duration::from_secs(600);
            let mut run = broker
                .begin_checkout_retirement_run(far)
                .unwrap()
                .expect("open paths resolve");
            broker.strip_idle_open_checkouts(&mut run, far, 100)
        };

        let (open, worktree) = session_with(&mut broker, "venv and members", true);
        let report = pass(&mut broker);

        assert_eq!(report.strip.checkouts_stripped, 1, "{report:#?}");
        assert!(!worktree.join("node_modules").exists(), "{report:#?}");
        assert!(
            !worktree.join("apps/a/node_modules").exists(),
            "{report:#?}"
        );
        assert!(!worktree.join("tools/.venv").exists(), "{report:#?}");
        assert!(worktree.join("apps/b/node_modules/cache.js").exists());
        assert!(worktree.join("bad/.venv/notes.txt").exists());
        assert!(worktree.join("pip/.venv/bin/ruff").exists());
        assert_eq!(
            report
                .strip
                .refused
                .get("environment_root_creation_unproven"),
            Some(&1),
            "{report:#?}"
        );
        assert_eq!(
            report.strip.refused.get("witness_missing"),
            Some(&1),
            "{report:#?}"
        );
        assert_eq!(
            report.strip.refused.get("unexpected_content"),
            Some(&1),
            "{report:#?}"
        );
        let kinds = &report.by_kind;
        assert_eq!(kinds["node_modules"].removed, 1, "{kinds:?}");
        assert_eq!(kinds["node_modules_member"].removed, 1, "{kinds:?}");
        assert_eq!(kinds["node_modules_member"].kept, 1, "{kinds:?}");
        assert_eq!(kinds["venv"].removed, 1, "{kinds:?}");
        assert_eq!(kinds["venv"].kept, 2, "{kinds:?}");
        // `submit prepare` sees the work to do: the declared file inside
        // the environment is missing and the strip marker stands.
        assert!(crate::preparation::stripped_since_prepared(
            broker.main_root(),
            open.id
        ));
        assert_ne!(
            broker.preparation_status(open.id).unwrap().state,
            crate::PreparationState::Current
        );

        // The top-level `node_modules` cannot be proved: its members stay.
        let (_other, kept) = session_with(&mut broker, "root kept", false);
        let report = pass(&mut broker);
        assert!(kept.join("node_modules").exists(), "{report:#?}");
        assert!(
            std::fs::symlink_metadata(kept.join("apps/a/node_modules/dep")).is_ok(),
            "{report:#?}"
        );
        assert_eq!(
            report.strip.refused.get("member_root_kept"),
            Some(&2),
            "{report:#?}"
        );
        assert!(!kept.join("tools/.venv").exists(), "{report:#?}");

        // The top-level `node_modules` proves but its move fails: members
        // proved alongside it stay with it.
        let (_third, failed) = session_with(&mut broker, "root move fails", true);
        super::super::strip::FAIL_MOVE_FOR_TEST
            .with(|cell| *cell.borrow_mut() = Some("node_modules".into()));
        let report = pass(&mut broker);
        super::super::strip::FAIL_MOVE_FOR_TEST.with(|cell| *cell.borrow_mut() = None);
        assert!(failed.join("node_modules/.bin/tsc").exists(), "{report:#?}");
        assert!(
            std::fs::symlink_metadata(failed.join("apps/a/node_modules/dep")).is_ok(),
            "{report:#?}"
        );
        assert_eq!(
            report.strip.refused.get("move_failed"),
            Some(&1),
            "{report:#?}"
        );
        assert_eq!(
            report.strip.refused.get("member_root_kept"),
            Some(&2),
            "{report:#?}"
        );
        crate::session_holder::set_process_use_snapshot_for_tests(None);
    }

    /// End to end with steps that really create their outputs: after an
    /// idle strip removed the top-level `node_modules`, a pnpm member and a
    /// Python environment whose interpreter a step declares,
    /// `submit prepare` recreates all three and reads current again.
    #[test]
    fn submit_prepare_after_an_idle_strip_recreates_what_was_removed() {
        let (_owner, storage, mut broker) = fixture();
        std::fs::write(
            broker.main_root().join(".git/info/exclude"),
            "node_modules/\n.venv/\n",
        )
        .unwrap();
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));
        let open = broker.start_worktree("prepare restores", None).unwrap();
        let worktree = PathBuf::from(&open.worktree_path);
        std::fs::create_dir_all(worktree.join(".aethyme")).unwrap();
        std::fs::write(
            worktree.join(".aethyme/prepare.toml"),
            "schema_version = 1\n\
             [[steps]]\nname = \"js\"\n\
             command = [\"/bin/sh\", \"-c\", \"mkdir -p node_modules/.pnpm/d@1/node_modules/d node_modules/.bin apps/a/node_modules && touch node_modules/.modules.yaml && ln -sfn ../../../node_modules/.pnpm/d@1/node_modules/d apps/a/node_modules/d\"]\n\
             outputs = [\"node_modules/\"]\nrequired_for_hooks = false\n\
             [[steps]]\nname = \"py\"\n\
             command = [\"/bin/sh\", \"-c\", \"mkdir -p tools/.venv/bin && echo 'home = /usr' > tools/.venv/pyvenv.cfg && touch tools/.venv/bin/python\"]\n\
             outputs = [\"tools/.venv/bin/python\"]\nrequired_for_hooks = false\n",
        )
        .unwrap();
        let config = crate::preparation::load_config(&worktree).unwrap().unwrap();
        let policy = crate::broker::gate_trust::GatePolicy::from_parts(&[], Some(&config));
        crate::broker::gate_trust::record_trust_for_host_state(
            broker.main_root(),
            &storage.options().host_state_dir.unwrap(),
            &[policy.policy_sha256.as_str()],
            "idle strip prepare fixture",
        )
        .unwrap();
        broker
            .prepare_session(open.id, false, Duration::ZERO)
            .expect("the first preparation runs");
        assert!(worktree.join("tools/.venv/bin/python").is_file());

        // Idle for hours, nobody in it.
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET last_activity_at = ?1 WHERE id = ?2",
                rusqlite::params![crate::clock::epoch_ms() - 5 * 3_600_000, open.id],
            )
            .unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(10 * 3_600);
        crate::worktree_status_activity::age_tree_for_test(&worktree, old);
        let admin = PathBuf::from(git(&worktree, &["rev-parse", "--absolute-git-dir"]).trim());
        for name in ["index", "HEAD"] {
            std::fs::File::options()
                .write(true)
                .open(admin.join(name))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let far = Instant::now() + Duration::from_secs(600);
        let mut run = broker
            .begin_checkout_retirement_run(far)
            .unwrap()
            .expect("open paths resolve");
        let report = broker.strip_idle_open_checkouts(&mut run, far, 100);
        assert_eq!(report.strip.checkouts_stripped, 1, "{report:#?}");
        assert!(!worktree.join("node_modules").exists(), "{report:#?}");
        assert!(
            !worktree.join("apps/a/node_modules").exists(),
            "{report:#?}"
        );
        assert!(!worktree.join("tools/.venv").exists(), "{report:#?}");
        assert_ne!(
            broker.preparation_status(open.id).unwrap().state,
            crate::PreparationState::Current
        );

        broker
            .prepare_session(open.id, false, Duration::ZERO)
            .expect("one command prepares again");
        assert!(worktree.join("node_modules/.modules.yaml").is_file());
        assert!(
            std::fs::symlink_metadata(worktree.join("apps/a/node_modules/d")).is_ok(),
            "the member comes back with the root install"
        );
        assert!(worktree.join("tools/.venv/pyvenv.cfg").is_file());
        assert_eq!(
            broker.preparation_status(open.id).unwrap().state,
            crate::PreparationState::Current
        );
        crate::session_holder::set_process_use_snapshot_for_tests(None);
    }

    /// An open session idle past `idle_strip_after_hours` with no process in
    /// its checkout has its regenerable directories stripped once, its
    /// preparation marked stale and one note left; it is not stripped again
    /// until new activity, a process inside keeps it, and `0` turns it off.
    #[test]
    fn an_idle_open_session_is_stripped_once_per_stretch_of_inactivity() {
        let (_owner, _storage, mut broker) = fixture();
        exclude_node_modules(&broker);
        std::fs::write(
            broker.main_root().join(".git/info/exclude"),
            "node_modules/\ntarget/\n",
        )
        .unwrap();
        let open = broker.start_worktree("idle open session", None).unwrap();
        let worktree = PathBuf::from(&open.worktree_path);
        // A repository that declares its preparation (one command rebuilds).
        std::fs::create_dir_all(worktree.join(".aethyme")).unwrap();
        std::fs::write(
            worktree.join(".aethyme/prepare.toml"),
            "schema_version = 1\n\n[[steps]]\nname = \"deps\"\ncommand = [\"sh\", \"-c\", \"true\"]\n\
             inputs = [\"lock.file\"]\noutputs = [\"node_modules/\"]\ncache = \"repository_shared\"\n\
             required_for_hooks = false\n",
        )
        .unwrap();
        let populate = |worktree: &Path| {
            std::fs::create_dir_all(worktree.join("node_modules/dep")).unwrap();
            std::fs::write(worktree.join("node_modules/dep/index.js"), "x\n").unwrap();
            std::fs::write(
                worktree.join("node_modules/.modules.yaml"),
                "layoutVersion: 5\n",
            )
            .unwrap();
        };
        populate(&worktree);
        let idle_since = |broker: &mut crate::Broker, hours: i64, offset_ms: i64| {
            broker
                .store()
                .connection()
                .execute(
                    "UPDATE sessions SET last_activity_at = ?1 WHERE id = ?2",
                    rusqlite::params![
                        crate::clock::epoch_ms() - hours * 3_600_000 - offset_ms,
                        open.id
                    ],
                )
                .unwrap();
        };
        // Nobody touched the checkout's files for hours (the moves and the
        // notice themselves update the checkout directory's time).
        let age_files = |worktree: &Path| {
            let old = std::time::SystemTime::now() - Duration::from_secs(10 * 3_600);
            let admin = PathBuf::from(git(worktree, &["rev-parse", "--absolute-git-dir"]).trim());
            let mut paths = vec![
                admin.join("index"),
                admin.join("HEAD"),
                worktree.to_path_buf(),
            ];
            for name in git(worktree, &["ls-tree", "-d", "--name-only", "HEAD"]).lines() {
                paths.push(worktree.join(name));
            }
            for path in paths.iter().filter(|path| path.exists()) {
                std::fs::File::open(path)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
            }
        };
        let pass_with = |broker: &mut crate::Broker, age: bool, stale_snapshot: bool| {
            if age {
                age_files(&worktree);
            }
            let far = Instant::now() + Duration::from_secs(600);
            let mut run = broker
                .begin_checkout_retirement_run(far)
                .unwrap()
                .expect("open paths resolve");
            if stale_snapshot {
                run.process_use().unwrap();
                run.age_process_use_for_test(Duration::from_secs(600));
            }
            broker.strip_idle_open_checkouts(&mut run, far, 100)
        };
        let pass = |broker: &mut crate::Broker| pass_with(broker, true, false);

        // Not idle long enough.
        idle_since(&mut broker, 1, 0);
        assert_eq!(pass(&mut broker).strip.checkouts_stripped, 0);
        assert!(worktree.join("node_modules").exists());

        // A process inside keeps it.
        idle_since(&mut broker, 5, 0);
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "node".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: std::fs::canonicalize(&worktree).unwrap(),
            }]),
        ));
        let kept = pass(&mut broker);
        assert_eq!(
            kept.strip.refused.get("process_using_checkout"),
            Some(&1),
            "{kept:#?}"
        );
        assert!(worktree.join("node_modules").exists());
        crate::session_holder::set_process_use_snapshot_for_tests(Some(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        ));

        // Idle by session activity, but the Git index was just written
        // (an agent working by absolute paths): not idle.
        let index = PathBuf::from(git(&worktree, &["rev-parse", "--absolute-git-dir"]).trim())
            .join("index");
        age_files(&worktree);
        std::fs::File::open(&index)
            .unwrap()
            .set_modified(std::time::SystemTime::now())
            .unwrap();
        let recent = pass_with(&mut broker, false, false);
        assert_eq!(recent.kept_recent_files, 1, "{recent:#?}");
        assert_eq!(
            recent.strip.refused.get("recent_file_activity"),
            Some(&1),
            "{recent:#?}"
        );
        assert!(worktree.join("node_modules").exists());

        // A preparation running (its shared lock on the checkout): skipped,
        // not remembered as handled.
        {
            let crate::file_lock::artifact_use::ArtifactUseScopeAdmission::Admitted(_preparing) =
                crate::file_lock::artifact_use::begin_artifact_use_scope(
                    broker.main_root(),
                    &worktree,
                )
                .unwrap()
            else {
                panic!("artifact-use scope admitted");
            };
            let busy = pass(&mut broker);
            assert_eq!(
                busy.strip.refused.get("checkout_busy"),
                Some(&1),
                "{busy:#?}"
            );
            assert!(worktree.join("node_modules").exists());
        }

        // A marker whose producer still runs keeps the checkout as
        // `checkout_marker_live`; one left by a producer that is gone (a
        // killed guarded command) does not keep it: the strip below goes
        // through with it still in place.
        let mut running = {
            use std::os::unix::process::CommandExt;
            std::process::Command::new("sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .unwrap()
        };
        let running_pid = i32::try_from(running.id()).unwrap();
        let live = crate::file_lock::artifact_use::write_running_marker_for_test(
            broker.main_root(),
            &worktree,
            "live-producer",
            Some(
                crate::file_lock::artifact_use::ProcessStartIdentity::capture(
                    running_pid,
                    running_pid,
                )
                .unwrap(),
            ),
        )
        .unwrap();
        let kept = pass(&mut broker);
        running.kill().unwrap();
        running.wait().unwrap();
        assert_eq!(
            kept.strip.refused.get("checkout_marker_live"),
            Some(&1),
            "{kept:#?}"
        );
        assert!(worktree.join("node_modules").exists());
        std::fs::remove_file(&live).unwrap();
        {
            use std::os::unix::process::CommandExt;
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .unwrap();
            let pid = i32::try_from(child.id()).unwrap();
            let identity =
                crate::file_lock::artifact_use::ProcessStartIdentity::capture(pid, pid).unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !crate::gates::gate_process_group_absent(pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            crate::file_lock::artifact_use::write_running_marker_for_test(
                broker.main_root(),
                &worktree,
                "exited-producer",
                Some(identity),
            )
            .unwrap();
        }

        // The run's observation is minutes old: a fresh one is taken under
        // the claim, and a process it sees keeps the checkout.
        crate::session_holder::queue_process_use_snapshots_for_tests(
            vec![
                crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
                crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                    pid: std::process::id(),
                    command: "node".into(),
                    descriptor: "cwd".into(),
                    file_type: Some("DIR".into()),
                    path: std::fs::canonicalize(&worktree).unwrap(),
                }]),
            ],
            false,
        );
        let fresh = pass_with(&mut broker, true, true);
        assert_eq!(
            fresh.strip.refused.get("process_using_checkout"),
            Some(&1),
            "{fresh:#?}"
        );
        assert!(worktree.join("node_modules").exists());

        // A catalogued directory no step declares (`target`) is kept.
        std::fs::create_dir_all(worktree.join("target/debug")).unwrap();
        std::fs::write(
            worktree.join("target/CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();

        let stripped = pass(&mut broker);

        assert_eq!(stripped.strip.checkouts_stripped, 1, "{stripped:#?}");
        assert_eq!(stripped.noted, 1, "{stripped:#?}");
        assert!(!worktree.join("node_modules").exists());
        assert_eq!(stripped.kept_not_declared, 1, "{stripped:#?}");
        assert!(
            worktree.join("target/CACHEDIR.TAG").exists(),
            "not a declared output: kept"
        );
        assert!(worktree.join("base.txt").exists(), "tracked content stays");
        assert!(
            broker
                .main_root()
                .join(format!(
                    ".aethyme/run/preparation/session-{}.stripped",
                    open.id
                ))
                .exists(),
            "its preparation reads stale"
        );
        let notes = broker.store_ref().unread_automation_notes(open.id).unwrap();
        assert!(
            notes
                .iter()
                .any(|note| note.message.contains("submit prepare")),
            "{notes:?}"
        );
        // The loud signal: the notice agents read first, outside Git's view,
        // and the same next action from the broker.
        let notice = worktree.join(crate::merge::ACTION_REQUIRED_RELPATH);
        let text = std::fs::read_to_string(&notice).unwrap();
        assert!(text.contains("node_modules"), "{text}");
        assert!(
            text.contains(&format!(
                "aethyme broker submit prepare --session {}",
                open.id
            )),
            "{text}"
        );
        assert!(
            git(&worktree, &["status", "--porcelain"]).trim().is_empty(),
            "the notice never shows in git status"
        );
        let blocker = broker
            .session_action_required_blocker(open.id)
            .unwrap()
            .expect("the notice is a blocker");
        assert_eq!(
            blocker.clear,
            format!("aethyme broker submit prepare --session {}", open.id)
        );
        assert_eq!(blocker.scope, crate::BlockerScope::Session);
        // Without the notice (it could not be written: another notice, a
        // tracked or unignored path) the session still gets the blocker and
        // the status advice, both naming the command.
        let saved = std::fs::read(&notice).unwrap();
        std::fs::remove_file(&notice).unwrap();
        let blocker = broker
            .session_action_required_blocker(open.id)
            .unwrap()
            .expect("stripped and not prepared is a blocker without the notice");
        assert_eq!(blocker.scope, crate::BlockerScope::Session);
        assert_eq!(
            blocker.clear,
            format!("aethyme broker submit prepare --session {}", open.id)
        );
        let advice = broker
            .session_status_advice(open.id, crate::clock::epoch_ms())
            .unwrap();
        assert!(
            advice.iter().any(|row| row.id == "idle_strip_prepare"
                && row.commands
                    == vec![format!(
                        "aethyme broker submit prepare --session {}",
                        open.id
                    )]),
            "{advice:?}"
        );
        std::fs::write(&notice, saved).unwrap();
        // Prepared again (a newer preparation record): the notice clears.
        let record = broker
            .main_root()
            .join(format!(".aethyme/run/preparation/session-{}.json", open.id));
        std::fs::write(&record, "{}").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&record)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        assert!(
            broker
                .session_action_required_blocker(open.id)
                .unwrap()
                .is_none()
        );
        assert!(!notice.exists(), "the notice clears once prepared again");

        // Re-prepared while still idle, with no new activity: not again.
        populate(&worktree);
        let again = pass(&mut broker);
        assert_eq!(again.strip.checkouts_stripped, 0, "{again:#?}");
        assert_eq!(again.skipped_handled, 1, "{again:#?}");
        assert!(worktree.join("node_modules").exists());

        // A memo written under older coverage rules is set aside: the
        // session is examined once more (and stripped again), then
        // remembered under the current rules.
        broker
            .update_meta_json(
                super::super::strip_idle::IDLE_STRIP_RULES_KEY,
                |stored: &mut u32| *stored = super::super::strip_idle::IDLE_STRIP_RULES - 1,
            )
            .unwrap();
        let reexamined = pass(&mut broker);
        assert_eq!(reexamined.skipped_handled, 0, "{reexamined:#?}");
        assert_eq!(reexamined.strip.checkouts_stripped, 1, "{reexamined:#?}");
        assert_eq!(
            broker.meta_json::<u32>(super::super::strip_idle::IDLE_STRIP_RULES_KEY),
            super::super::strip_idle::IDLE_STRIP_RULES
        );
        populate(&worktree);
        let remembered = pass(&mut broker);
        assert_eq!(remembered.skipped_handled, 1, "{remembered:#?}");

        // New activity, then idle again: armed again.
        idle_since(&mut broker, 5, 1_000);
        assert_eq!(pass(&mut broker).strip.checkouts_stripped, 1);

        // `0` turns it off.
        populate(&worktree);
        idle_since(&mut broker, 5, 2_000);
        std::fs::write(
            broker.main_root().join(".aethyme/config.toml"),
            "[cleanup]\nidle_strip_after_hours = 0\n",
        )
        .unwrap();
        assert_eq!(pass(&mut broker).strip.checkouts_examined, 0);
        assert!(worktree.join("node_modules").exists());
    }
}
