//! Exclusive advisory file locks, shared by every broker lock file.
//!
//! These are `std::fs::File::lock` locks, which on Unix are `flock(2)`: they
//! belong to the open file description, are advisory, and are released when
//! the last descriptor for that description closes. That is exactly what the
//! hand-written `libc::flock` copies this replaces relied on, so dropping the
//! guard (or the process dying) still releases the lock.

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// How long a foreground lifecycle command waits for the claim. A start on
/// Mockup (11,437 tracked files, load about 40) held it for 20-25 s, so a 10 s
/// wait failed 102 starts in one day (2026-11-05), each leaving its agent to
/// retry by hand. This covers about seven such holders queued ahead.
const LIFECYCLE_LOCK_WAIT: Duration = Duration::from_secs(180);
/// How often the waiter at the head of the queue tries the lock.
const LIFECYCLE_LOCK_POLL: Duration = Duration::from_millis(50);
/// How often a waiter with others ahead of it rescans the queue: it cannot
/// take the lock before they do, so it need not look as often.
const LIFECYCLE_QUEUED_POLL: Duration = Duration::from_millis(250);
/// When a waiting command first says so, and how often after that.
const LIFECYCLE_WAIT_NOTICE_AFTER: Duration = Duration::from_secs(5);
const LIFECYCLE_WAIT_NOTICE_EVERY: Duration = Duration::from_secs(30);

/// Directory, under `.aethyme/run`, where every command waiting for the
/// lifecycle lock keeps one ticket file for as long as it waits. The lock
/// itself stays the flock on `worktree-lifecycle.lock`; tickets only decide who
/// may try it next, so a lost or unreadable ticket can cost fairness, never
/// mutual exclusion.
const LIFECYCLE_WAITERS_DIR: &str = "lifecycle-lock-waiters";
/// How often a waiting command writes a fresh heartbeat into its ticket (and
/// re-creates the ticket if something removed it).
const LIFECYCLE_TICKET_HEARTBEAT: Duration = Duration::from_secs(2);
/// A ticket whose live process has not written a heartbeat for this long
/// (stopped, hung, or swapped out) is passed over, so it cannot hold up the
/// queue. It is not removed: the waiter takes its turn again once it beats.
const LIFECYCLE_TICKET_STALL: Duration = Duration::from_secs(10);
/// A ticket enqueued longer ago than this is removed whatever its process: no
/// command waits this long, so it was left by one that never cleaned up.
const LIFECYCLE_TICKET_MAX_AGE: Duration = Duration::from_secs(15 * 60);
/// How long one waiter trusts a ticket's liveness verdict before checking the
/// process again.
const LIFECYCLE_LIVENESS_CACHE: Duration = Duration::from_secs(1);
/// A waiter passed by this many later arrivals is aged: it then outranks every
/// later arrival whatever its budget.
const LIFECYCLE_TICKET_MAX_PASSES: usize = 3;
/// A waiter that has had no earlier arrival ahead of it for this long is aged
/// too, so deadline order cannot keep a long-budget waiter behind a steady
/// stream of short budgets until its own deadline.
const LIFECYCLE_TICKET_MAX_PASSED_FOR: Duration = Duration::from_secs(20);
/// Ticket times further ahead of this host's monotonic clock than this were
/// written before the last boot. Wide, so a heartbeat written while a slow scan
/// was reading other tickets is never mistaken for one.
const LIFECYCLE_TICKET_PRE_BOOT_SLACK: Duration = Duration::from_secs(60);

/// Directory, under `.aethyme/run`, where each background lane records since
/// when, and how many times in a row, it has yielded to queued waiters.
const LIFECYCLE_YIELDS_DIR: &str = "lifecycle-lock-yields";
/// A background lane that has yielded continuously for this long queues
/// instead of yielding again.
const BACKGROUND_YIELD_LIMIT: Duration = Duration::from_secs(60);
/// A background lane that has yielded this many times in a row queues on its
/// next attempt instead of yielding again.
const BACKGROUND_YIELD_COUNT: u32 = 5;
/// How long a promoted background lane waits in the queue before it gives up
/// for this pass (never past an active command Git deadline).
const BACKGROUND_PROMOTED_WAIT: Duration = Duration::from_secs(30);
/// The promoted wait while this process holds the GC lock: short enough that
/// a foreground command waiting for the GC lock (`broker start` at the
/// checkout limit, `gc apply`, 30 s) is never outwaited by it.
const BACKGROUND_PROMOTED_WAIT_UNDER_GC_LOCK: Duration = Duration::from_secs(3);

/// Orders tickets one process creates within the same clock tick.
static LIFECYCLE_TICKET_SEQUENCE: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Depth of [`ForegroundLifecycleTryClaim`] scopes on this thread.
    static FOREGROUND_TRY_CLAIM: Cell<u32> = const { Cell::new(0) };
}

/// While alive, nonblocking lifecycle claims made on this thread do not yield
/// to queued waiters. For a foreground command that must try the claim without
/// queueing (`broker start` making room at the checkout limit, an operator's
/// explicit apply): it acts for a waiting user, so it is not background work to
/// be deferred. Background lanes never enter it.
pub(crate) struct ForegroundLifecycleTryClaim {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ForegroundLifecycleTryClaim {
    pub(crate) fn enter() -> Self {
        FOREGROUND_TRY_CLAIM.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self {
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for ForegroundLifecycleTryClaim {
    fn drop(&mut self) {
        FOREGROUND_TRY_CLAIM.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn foreground_try_claim_active() -> bool {
    FOREGROUND_TRY_CLAIM.with(|depth| depth.get() > 0)
}

thread_local! {
    /// Depth of [`DetachedBackgroundWorker`] scopes on this thread.
    static DETACHED_BACKGROUND_WORKER: Cell<u32> = const { Cell::new(0) };
}

/// Marks this thread as a detached background worker process (the artifact,
/// cleanup and retirement worker, the managed GC sweep tick, the storage
/// worker), entered where that worker starts. Only inside it may a background
/// lane that has yielded too long queue and wait for its turn. Everywhere else
/// (inline maintenance inside `status`, `start`, `push` and every other
/// foreground command) a lane only try-claims and yields, so no user waits on
/// background work; its yields still count toward the lane's streak, so the
/// next detached pass promotes.
pub(crate) struct DetachedBackgroundWorker {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl DetachedBackgroundWorker {
    pub(crate) fn enter() -> Self {
        DETACHED_BACKGROUND_WORKER.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self {
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for DetachedBackgroundWorker {
    fn drop(&mut self) {
        DETACHED_BACKGROUND_WORKER.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn detached_background_worker() -> bool {
    DETACHED_BACKGROUND_WORKER.with(|depth| depth.get() > 0)
}

/// Whether this thread runs inside the detached background worker, where
/// nobody waits on a short bounded retry.
pub(crate) fn in_detached_background_worker() -> bool {
    detached_background_worker()
}

thread_local! {
    /// Depth of [`ForegroundCommand`] scopes on this thread.
    static FOREGROUND_COMMAND: Cell<u32> = const { Cell::new(0) };
}

/// Marks this thread as running a broker CLI command someone waits on,
/// entered where the CLI dispatches. Inside it the `agents` view does not
/// judge stale sessions for retirement (holder, lease and process evidence,
/// then a nonblocking lifecycle claim it lost on a busy repository): it
/// counts the sessions that are due and queues the one shared detached
/// worker, which judges them under the lifecycle queue rules. Worker
/// processes are dispatched through the same entry, so a
/// [`DetachedBackgroundWorker`] scope outranks this one. Library callers
/// (tests, in-process tools) enter neither and judge inline as before.
pub(crate) struct ForegroundCommand {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ForegroundCommand {
    pub(crate) fn enter() -> Self {
        FOREGROUND_COMMAND.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self {
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for ForegroundCommand {
    fn drop(&mut self) {
        FOREGROUND_COMMAND.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Whether session retirement may be judged inline on this thread: always in
/// a detached worker, never in a foreground CLI command.
pub(crate) fn inline_session_retirement_allowed() -> bool {
    detached_background_worker() || FOREGROUND_COMMAND.with(|depth| depth.get() == 0)
}

/// Which side of the queue a lifecycle claim came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleClaimKind {
    /// A command that waits its turn in the queue (start, adopt, finish,
    /// cleanup, close, submit, gate admission, storage removal).
    Foreground,
    /// A nonblocking claim; it yields to any queued waiter unless made
    /// inside a [`ForegroundLifecycleTryClaim`] scope.
    Nonblocking,
    /// A background lane that yielded too long and is now queued with a
    /// bounded wait, so a busy queue cannot starve it.
    Promoted,
}

impl LifecycleClaimKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Nonblocking => "nonblocking",
            Self::Promoted => "promoted",
        }
    }
}

/// Open (creating if needed) a lock file without truncating it.
pub(crate) fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

/// Acquire the cross-process lifecycle mutation claim for the primary repo.
///
/// The persistent lock lives under the primary repository's runtime state,
/// never under a checkout that an operation may remove. All guarded checkout
/// creation, adoption, and removal paths use the same file, so registering a
/// nested checkout cannot race deletion of its parent. The claim is deliberately
/// scoped to checkout creation/adoption/removal; ordinary edits, builds, and
/// submissions do not acquire it. The lock file is never unlinked: doing so
/// while another process holds it would allow a second inode at the same path.
///
/// Waits at most [`LIFECYCLE_LOCK_WAIT`], and never past an active command-scoped
/// Git deadline such as `finish --timeout`.
pub(crate) fn acquire_repository_lifecycle_lock(main_root: &Path) -> io::Result<ExclusiveFileLock> {
    let wait = crate::git::active_git_deadline().map_or(LIFECYCLE_LOCK_WAIT, |deadline| {
        LIFECYCLE_LOCK_WAIT.min(deadline.saturating_duration_since(Instant::now()))
    });
    acquire_repository_lifecycle_lock_with_wait(main_root, wait)
}

/// The command holding the claim, for the first words of its arguments: `broker
/// start`, never the free text or paths that follow them.
fn lifecycle_operation() -> String {
    std::env::args()
        .skip(1)
        .take_while(|word| !word.starts_with('-') && !word.contains(['/', '\\']))
        .take(4)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The session this command names with `--session <id>`, if any: digits only,
/// so nothing else from the command line reaches the record.
fn lifecycle_session_argument() -> Option<String> {
    let digits = |value: &str| {
        (!value.is_empty() && value.chars().all(|c| c.is_ascii_digit())).then(|| value.to_owned())
    };
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--" {
            break;
        }
        if argument == "--session" {
            return args.next().as_deref().and_then(digits);
        }
        if let Some(value) = argument.strip_prefix("--session=") {
            return digits(value);
        }
    }
    None
}

/// Say who holds the claim, for the next waiter. Telemetry only: a failed write
/// must not fail the operation that already owns the lock, and the waiter reports
/// an unidentified holder. `operation=` stays last: it runs to the end of the
/// line.
fn record_lifecycle_holder(lock: &ExclusiveFileLock, kind: LifecycleClaimKind) {
    crate::warn_unrecorded(
        "record the worktree lifecycle lock holder",
        lock.record_holder(&format!(
            "pid={} since_ms={} kind={} session={} operation={}",
            std::process::id(),
            crate::clock::epoch_ms(),
            kind.as_str(),
            lifecycle_session_argument().unwrap_or_else(|| "-".into()),
            lifecycle_operation()
        )),
    );
}

/// The fields a lock file's holder record carries; `None` when it has none.
struct LifecycleHolderRecord {
    pid: i64,
    since_ms: i64,
    kind: Option<String>,
    session: Option<String>,
    operation: Option<String>,
}

fn read_lifecycle_holder(path: &Path) -> Option<LifecycleHolderRecord> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let (head, operation) = match text.split_once("operation=") {
        Some((head, operation)) => (head, Some(operation.trim())),
        None => (text.as_str(), None),
    };
    let field = |name: &str| {
        head.split_whitespace()
            .find_map(|word| word.strip_prefix(name))
            .map(str::to_owned)
    };
    Some(LifecycleHolderRecord {
        pid: field("pid=")?.parse().ok()?,
        since_ms: field("since_ms=")?.parse().ok()?,
        kind: field("kind="),
        session: field("session=").filter(|session| session != "-"),
        operation: operation
            .filter(|operation| !operation.is_empty())
            .map(str::to_owned),
    })
}

/// The command a live recorded holder runs, for telemetry; `None` when the
/// holder is unrecorded or gone.
fn live_lifecycle_holder_operation(path: &Path) -> Option<String> {
    let record = read_lifecycle_holder(path)?;
    crate::blockers::process_alive(record.pid).then(|| {
        record
            .operation
            .unwrap_or_else(|| "unknown operation".into())
    })
}

/// The holder a contended lock file names, in words for the person waiting.
///
/// flock has no owner to ask, and the record is written just after locking, so
/// it can briefly name the previous holder, and a binary older than this record
/// writes none. A named process that has exited therefore says that the holder
/// is unidentified, not that the lock is free.
fn describe_lifecycle_holder(path: &Path) -> String {
    let Some(record) = read_lifecycle_holder(path) else {
        return "the holder did not record itself (an older aethyme binary?)".into();
    };
    let pid = record.pid;
    if !crate::blockers::process_alive(pid) {
        return format!(
            "the last recorded holder, pid {pid}, has exited, so the current holder is \
             unidentified (an older aethyme binary?)"
        );
    }
    let mut what = record
        .operation
        .unwrap_or_else(|| "unknown operation".into());
    if let Some(session) = record.session {
        what.push_str(&format!(", session {session}"));
    }
    if let Some(kind) = record.kind {
        what.push_str(&format!(", {kind}"));
    }
    let held_for = (crate::clock::epoch_ms() - record.since_ms).max(0) / 1000;
    format!("pid {pid} ({what}) has held it for {held_for} s and is running")
}

/// Make a nonblocking claim for a checkout lifecycle operation on behalf of
/// the background `lane` (a short lowercase name such as `retire`).
///
/// Admission paths use this when queueing behind a remover could outlive the
/// caller's bounded maintenance window. `None` means another lifecycle claim
/// is active; path and inode changes remain errors.
///
/// Queued waiters outrank it: while any live ticket is queued it returns
/// `None` without touching the lock. That yielding is bounded per lane: once
/// the lane has yielded [`BACKGROUND_YIELD_COUNT`] times in a row, or for
/// [`BACKGROUND_YIELD_LIMIT`], its next attempt inside a
/// [`DetachedBackgroundWorker`] takes a ticket and waits its turn for at most
/// [`BACKGROUND_PROMOTED_WAIT`] ([`BACKGROUND_PROMOTED_WAIT_UNDER_GC_LOCK`]
/// while this process holds the GC lock), so a queue that is never empty
/// cannot starve retirement, cleanup or GC. Inline in a foreground command it
/// never waits: it yields and the streak carries over to the next detached
/// pass. A [`ForegroundLifecycleTryClaim`] scope on this thread skips the
/// yielding.
pub(crate) fn try_acquire_repository_lifecycle_lock(
    main_root: &Path,
    lane: &'static str,
) -> io::Result<Option<ExclusiveFileLock>> {
    let path = repository_lifecycle_lock_path(main_root)?;
    if !foreground_try_claim_active()
        && let Some(directory) = existing_lifecycle_waiters_directory(main_root)
        && scan_lifecycle_queue(&directory, None, None).live > 0
    {
        return yield_or_promote_background_lane(main_root, lane);
    }
    let file = open_lock_file_nofollow(&path)?;
    let Some(mut lock) = ExclusiveFileLock::try_acquire(file)? else {
        return Ok(None);
    };
    if repository_lifecycle_lock_path(main_root)? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "broker lifecycle lock path changed while it was acquired",
        ));
    }
    verify_named_lock_file(&path, &lock.file)?;
    record_lifecycle_holder(&lock, LifecycleClaimKind::Nonblocking);
    clear_background_yields(main_root, lane);
    // Counted, not attached: the `lifecycle_lock` object describes the
    // command's queued claims, which a maintenance claim must not overwrite.
    crate::phase_telemetry::add("lifecycle_lock_nonblocking_acquired", Duration::ZERO);
    lock.lifecycle_held_since = Some((Instant::now(), LifecycleClaimKind::Nonblocking));
    Ok(Some(lock))
}

/// A background lane's run of consecutive yields, as persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BackgroundYields {
    /// [`host_monotonic_ns`] when the run of yields began.
    since_ns: u128,
    count: u32,
}

fn background_yields_path(main_root: &Path, lane: &str, create: bool) -> Option<PathBuf> {
    if lane.is_empty()
        || !lane
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        return None;
    }
    let directory = repository_runtime_directory(main_root)
        .ok()?
        .join(LIFECYCLE_YIELDS_DIR);
    if create {
        ensure_real_directory(&directory).ok()?;
    } else if !std::fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir()) {
        return None;
    }
    Some(directory.join(lane))
}

fn read_background_yields(path: &Path) -> Option<BackgroundYields> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let field = |name: &str| {
        text.split_whitespace()
            .find_map(|word| word.strip_prefix(name))
            .map(str::to_owned)
    };
    Some(BackgroundYields {
        since_ns: field("since_ns=")?.parse().ok()?,
        count: field("count=")?.parse().ok()?,
    })
}

/// Best effort: a lost record only delays or advances one lane's promotion.
fn write_background_yields(path: &Path, yields: BackgroundYields) {
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        return;
    }
    std::fs::write(
        path,
        format!("since_ns={} count={}\n", yields.since_ns, yields.count),
    )
    .ok();
}

fn clear_background_yields(main_root: &Path, lane: &str) {
    if let Some(path) = background_yields_path(main_root, lane, false)
        && std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file())
    {
        std::fs::remove_file(path).ok();
    }
}

/// Whether a lane whose run of yields so far is `previous` should queue now
/// rather than yield once more.
fn background_lane_due_for_promotion(previous: Option<BackgroundYields>, now_ns: u128) -> bool {
    previous.is_some_and(|previous| {
        previous.count >= BACKGROUND_YIELD_COUNT
            || now_ns.saturating_sub(previous.since_ns) >= BACKGROUND_YIELD_LIMIT.as_nanos()
    })
}

/// A queued waiter exists: yield once more, or, when the lane has yielded long
/// enough, queue for a bounded turn.
fn yield_or_promote_background_lane(
    main_root: &Path,
    lane: &'static str,
) -> io::Result<Option<ExclusiveFileLock>> {
    let now_ns = host_monotonic_ns();
    let record = background_yields_path(main_root, lane, true);
    // A record from before a reboot (its start lies in the future) restarts.
    let previous = record
        .as_deref()
        .and_then(read_background_yields)
        .filter(|previous| previous.since_ns <= now_ns);
    let yielding_for_ms = previous.map_or(0, |previous| {
        u64::try_from(now_ns.saturating_sub(previous.since_ns) / 1_000_000).unwrap_or(u64::MAX)
    });
    let detached = detached_background_worker();
    let context = if detached { "detached" } else { "inline" };
    let due = background_lane_due_for_promotion(previous, now_ns);
    // Inline, a lane never waits: it yields, and its streak keeps growing
    // until a detached worker pass promotes it.
    if !due || !detached {
        let yields = BackgroundYields {
            since_ns: previous.map_or(now_ns, |previous| previous.since_ns),
            count: previous.map_or(1, |previous| previous.count.saturating_add(1)),
        };
        if let Some(record) = &record {
            write_background_yields(record, yields);
        }
        crate::phase_telemetry::add("lifecycle_lock_yielded_to_waiter", Duration::ZERO);
        crate::phase_telemetry::attach(
            "lifecycle_lock_background",
            serde_json::json!({
                "lane": lane,
                "context": context,
                "outcome": "yielded",
                "consecutive_yields": yields.count,
                "yielding_for_ms": yielding_for_ms,
                "promotion_deferred_to_detached_worker": due,
            }),
        );
        return Ok(None);
    }
    // Promoted: the run of yields ends here whatever the wait's outcome.
    clear_background_yields(main_root, lane);
    // Waiting while holding the GC lock would make a foreground GC-lock
    // waiter lose to background work, so that wait is capped instead of the
    // GC lock being released: the GC lock guards a pass's journaled plan, and
    // re-validating every plan after re-taking it is not provably safe here.
    let capped_by_gc_lock = crate::gc::this_process_holds_gc_lock();
    let ceiling = if capped_by_gc_lock {
        BACKGROUND_PROMOTED_WAIT_UNDER_GC_LOCK
    } else {
        BACKGROUND_PROMOTED_WAIT
    };
    let wait = crate::git::active_git_deadline().map_or(ceiling, |deadline| {
        ceiling.min(deadline.saturating_duration_since(Instant::now()))
    });
    let started = Instant::now();
    let mut observation = LifecycleQueueObservation::default();
    let result = acquire_lifecycle_lock_in_turn(
        main_root,
        wait,
        started,
        &mut observation,
        LifecycleClaimKind::Promoted,
    );
    let waited = started.elapsed();
    crate::phase_telemetry::add("lifecycle_lock_promoted_wait", waited);
    crate::phase_telemetry::attach(
        "lifecycle_lock_background",
        serde_json::json!({
            "lane": lane,
            "context": context,
            "promoted_capped_gc_lock": capped_by_gc_lock,
            "outcome": match &result {
                Ok(_) => "promoted_acquired",
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => "promoted_timeout",
                Err(_) => "promoted_failed",
            },
            "consecutive_yields": previous.map_or(0, |previous| previous.count),
            "yielding_for_ms": yielding_for_ms,
            "promoted_wait_ms": u64::try_from(waited.as_millis()).unwrap_or(u64::MAX),
            "queue_position_at_enqueue": observation.position_at_enqueue,
        }),
    );
    match result {
        Ok(mut lock) => {
            lock.lifecycle_held_since = Some((Instant::now(), LifecycleClaimKind::Promoted));
            Ok(Some(lock))
        }
        // A turn that did not come within the bound is an ordinary busy
        // answer: the lane retries on its next pass.
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

fn repository_lifecycle_lock_path(main_root: &Path) -> io::Result<std::path::PathBuf> {
    repository_runtime_lock_path(main_root, "worktree-lifecycle.lock")
}

/// Try to claim one session's compact-recovery writer without blocking other
/// lifecycle operations. The persistent lock is released automatically when
/// the process exits, so an interrupted archive attempt can be reconciled.
pub(crate) fn try_acquire_retirement_archive_lock(
    main_root: &Path,
    session_id: i64,
) -> io::Result<Option<ExclusiveFileLock>> {
    if session_id <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "retirement lock requires a positive session id",
        ));
    }
    let name = format!("checkout-retirement-{session_id}.lock");
    let path = repository_runtime_lock_path(main_root, &name)?;
    let file = open_lock_file_nofollow(&path)?;
    let Some(lock) = ExclusiveFileLock::try_acquire(file)? else {
        return Ok(None);
    };
    if repository_runtime_lock_path(main_root, &name)? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "retirement archive lock path changed while it was acquired",
        ));
    }
    verify_named_lock_file(&path, &lock.file)?;
    Ok(Some(lock))
}

fn repository_runtime_lock_path(main_root: &Path, name: &str) -> io::Result<std::path::PathBuf> {
    Ok(repository_runtime_directory(main_root)?.join(name))
}

/// The primary repository's real `.aethyme/run` directory, created if absent
/// and refused when it is a symlink or escapes the repository.
pub(crate) fn repository_run_directory(main_root: &Path) -> io::Result<std::path::PathBuf> {
    repository_runtime_directory(main_root)
}

/// Which side of the per-session command/retirement fence to acquire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionOperationLockMode {
    Shared,
    Exclusive,
}

/// A per-session operation fence. The shared lock remains close-on-exec so a
/// detached helper cannot pin a session forever. Native artifact, gate, and
/// coordinated-operation witnesses protect children after the broker parent
/// exits; retirement refuses when those inventories are unknown.
#[derive(Debug)]
pub(crate) enum SessionOperationGuard {
    Shared { _lock: SharedSessionOperationLock },
    Exclusive { _lock: ExclusiveFileLock },
}

#[derive(Debug)]
pub(crate) struct SharedSessionOperationLock {
    _file: File,
}

/// Try to serialize one session's admitted work against abandoned retirement.
/// The lock file is persistent and remains outside any worktree the operation
/// may remove. A busy lock is an ordinary retryable refusal; path or lock
/// protocol errors remain errors and must not be interpreted as an idle
/// session.
pub(crate) fn try_acquire_session_operation_guard(
    main_root: &Path,
    session_id: i64,
    mode: SessionOperationLockMode,
) -> io::Result<Option<SessionOperationGuard>> {
    if session_id <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session operation guard requires a positive session ID",
        ));
    }
    let name = format!("session-operation-{session_id}.lock");
    let path = repository_runtime_lock_path(main_root, &name)?;
    let file = open_lock_file_nofollow(&path)?;
    match mode {
        SessionOperationLockMode::Shared => {
            match file.try_lock_shared() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
            if repository_runtime_lock_path(main_root, &name)? != path {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "session operation lock path changed while it was acquired",
                ));
            }
            verify_named_lock_file(&path, &file)?;
            Ok(Some(SessionOperationGuard::Shared {
                _lock: SharedSessionOperationLock { _file: file },
            }))
        }
        SessionOperationLockMode::Exclusive => {
            let Some(lock) = ExclusiveFileLock::try_acquire(file)? else {
                return Ok(None);
            };
            if repository_runtime_lock_path(main_root, &name)? != path {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "session operation lock path changed while it was acquired",
                ));
            }
            verify_named_lock_file(&path, &lock.file)?;
            Ok(Some(SessionOperationGuard::Exclusive { _lock: lock }))
        }
    }
}

/// Whether this host supports the nonblocking per-session advisory lock used
/// by the guarded expiry protocol. Descendants are protected separately by
/// durable producer/process witnesses rather than inheriting this descriptor.
pub(crate) const fn session_operation_guard_supported() -> bool {
    cfg!(unix)
}

fn repository_runtime_directory(main_root: &Path) -> io::Result<std::path::PathBuf> {
    let root = main_root.canonicalize()?;
    let state_dir = root.join(".aethyme");
    ensure_real_directory(&state_dir)?;
    let run_dir = state_dir.join("run");
    ensure_real_directory(&run_dir)?;
    let canonical_run = run_dir.canonicalize()?;
    if canonical_run != run_dir || !canonical_run.starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "broker runtime path is not a real directory under the primary repository: {}",
                run_dir.display()
            ),
        ));
    }

    Ok(canonical_run)
}

/// Open the lock that orders `command-metrics.jsonl` appends (shared) against
/// the GC trim that replaces that log (exclusive). It lives in `.aethyme/run`,
/// not beside the log, and is never unlinked, so every holder locks one inode.
pub(crate) fn open_command_metrics_lock(main_root: &Path) -> io::Result<File> {
    let path = repository_runtime_lock_path(main_root, "command-metrics.lock")?;
    let file = open_lock_file_nofollow(&path)?;
    verify_named_lock_file(&path, &file)?;
    Ok(file)
}

/// Claim one complete CLI scheduling pass, including repository watches.
/// The file remains persistent so a second inode cannot split ownership.
pub(crate) fn acquire_pr_watch_tick_lock(main_root: &Path) -> io::Result<ExclusiveFileLock> {
    let path = repository_runtime_lock_path(main_root, "pr-watch-tick.lock")?;
    let file = open_lock_file_nofollow(&path)?;
    let lock = ExclusiveFileLock::try_acquire(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "another PR watch tick is running; retry after it finishes",
        )
    })?;
    if repository_runtime_lock_path(main_root, "pr-watch-tick.lock")? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "broker PR watch lock path changed while it was acquired",
        ));
    }
    verify_named_lock_file(&path, &lock.file)?;
    Ok(lock)
}

/// Serialize one environment's external lifecycle effects. The lock lives
/// outside the removable environment directory and is never unlinked. Process
/// exit releases ownership, so an interrupted destroy can safely resume.
pub(crate) fn try_acquire_environment_operation_lock(
    root: &Path,
    env_id: &str,
) -> io::Result<ExclusiveFileLock> {
    use sha2::{Digest, Sha256};
    std::fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    let directory = root.join("operation-locks");
    ensure_real_directory(&directory)?;
    let name = format!("{:x}.lock", Sha256::digest(env_id.as_bytes()));
    let path = directory.join(name);
    let file = open_lock_file_nofollow(&path)?;
    let lock = ExclusiveFileLock::try_acquire(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("environment {env_id} has a lifecycle operation in progress; retry after it finishes"),
        )
    })?;
    verify_named_lock_file(&path, &lock.file)?;
    Ok(lock)
}

/// Claim the cooperative commit lane for one session without serializing
/// commits from other worktrees through the repository-wide Git write lock.
/// The persistent lock file is never removed, so contenders always lock the
/// same inode. A busy lane refuses immediately; it never queues behind a
/// commit hook that may be waiting for another repository operation.
pub(crate) fn try_acquire_session_commit_lock(
    main_root: &Path,
    session_id: i64,
) -> io::Result<ExclusiveFileLock> {
    if session_id <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session commit lock requires a positive session id",
        ));
    }
    let name = format!("session-commit-{session_id}.lock");
    let path = repository_runtime_lock_path(main_root, &name)?;
    let file = open_lock_file_nofollow(&path)?;
    let lock = ExclusiveFileLock::try_acquire(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("another broker commit is running for session {session_id}"),
        )
    })?;
    if repository_runtime_lock_path(main_root, &name)? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "broker session commit lock path changed while it was acquired",
        ));
    }
    verify_named_lock_file(&path, &lock.file)?;
    Ok(lock)
}

/// What one queued claim saw while it waited, for its telemetry and refusal.
#[derive(Default)]
struct LifecycleQueueObservation {
    /// Live waiters ahead of this one when it queued.
    position_at_enqueue: usize,
    /// Live waiters, this one excluded, when it queued.
    length_at_enqueue: usize,
    /// The command holding the lock when this one first found it held.
    waited_behind: Option<String>,
    /// Whether this claim got a ticket; without one it polls unordered, as
    /// binaries before the queue did.
    ticketed: bool,
    /// Distinct later arrivals seen ahead of this claim.
    passed_by: usize,
    /// Whether the claim aged (see [`LIFECYCLE_TICKET_MAX_PASSES`]).
    aged: bool,
}

/// Wait for the lifecycle lock in ticket order. Each waiter keeps a ticket
/// under `.aethyme/run` naming its deadline (enqueue time plus its wait budget),
/// its enqueue time, PID, process start time and a sequence, all times on the
/// host's monotonic clock. Tickets are served earliest deadline first, ties by
/// enqueue time, then PID and sequence, so a short-budget caller such as gate
/// admission goes ahead of a `start` with three minutes to spare. An arrival
/// with budget `b` goes ahead of an earlier waiter only while that waiter
/// still has more than `b` seconds left.
///
/// Deadline order alone would let a steady stream of short budgets keep a long
/// waiter back until its last seconds, so tickets age. Once a waiter has been
/// passed by [`LIFECYCLE_TICKET_MAX_PASSES`] later arrivals, or has had no
/// earlier arrival ahead of it for [`LIFECYCLE_TICKET_MAX_PASSED_FOR`],
/// whichever comes first, it writes `aged=1` into its ticket. Aged tickets rank
/// before every unaged one, among themselves in enqueue order. The rank is
/// derived from each ticket's name and contents alone, so every process sees
/// the same total order. A waiter is therefore passed by at most three later
/// arrivals it saw, or for at most 20 s, before only earlier arrivals and the
/// current holder stand between it and the lock.
///
/// Only the waiter with no live ticket ahead tries the lock. A ticket whose
/// process has exited, or whose PID now belongs to another process, is
/// removed; one whose live process stopped writing heartbeats is passed over.
/// Older binaries keep no ticket and may still take the lock out of turn; that
/// costs order, never exclusion, which the flock alone provides.
///
/// Records the wait, the queue position and the holder it waited behind on
/// every outcome, the timeout included, and the hold time when the guard drops.
pub(crate) fn acquire_repository_lifecycle_lock_with_wait(
    main_root: &Path,
    wait: Duration,
) -> io::Result<ExclusiveFileLock> {
    let started = Instant::now();
    let mut observation = LifecycleQueueObservation::default();
    let result = acquire_lifecycle_lock_in_turn(
        main_root,
        wait,
        started,
        &mut observation,
        LifecycleClaimKind::Foreground,
    );
    let waited = started.elapsed();
    crate::phase_telemetry::add("lifecycle_lock_wait", waited);
    crate::phase_telemetry::attach(
        "lifecycle_lock",
        serde_json::json!({
            "claim": LifecycleClaimKind::Foreground.as_str(),
            "outcome": match &result {
                Ok(_) => "acquired",
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => "timeout",
                Err(_) => "failed",
            },
            "wait_ms": u64::try_from(waited.as_millis()).unwrap_or(u64::MAX),
            "queue_position_at_enqueue": observation.position_at_enqueue,
            "queue_length_at_enqueue": observation.length_at_enqueue,
            "queued": observation.ticketed,
            "passed_by_later_arrivals": observation.passed_by,
            "aged": observation.aged,
            "waited_behind": observation.waited_behind,
            "holder_operation": lifecycle_operation(),
        }),
    );
    let mut lock = result?;
    lock.lifecycle_held_since = Some((Instant::now(), LifecycleClaimKind::Foreground));
    Ok(lock)
}

/// How long a waiter sleeps before looking again: the head tries the lock
/// often, everyone behind it only rescans.
fn lifecycle_poll_interval(ahead: usize) -> Duration {
    if ahead == 0 {
        LIFECYCLE_LOCK_POLL
    } else {
        LIFECYCLE_QUEUED_POLL
    }
}

fn acquire_lifecycle_lock_in_turn(
    main_root: &Path,
    wait: Duration,
    started: Instant,
    observation: &mut LifecycleQueueObservation,
    kind: LifecycleClaimKind,
) -> io::Result<ExclusiveFileLock> {
    let path = repository_lifecycle_lock_path(main_root)?;
    let deadline = started + wait;
    let mut next_notice = started + LIFECYCLE_WAIT_NOTICE_AFTER;
    // Best effort: a waiter that cannot keep a ticket still waits, unordered.
    let mut ticket = LifecycleTicket::enqueue(main_root, wait);
    let mut liveness = LifecycleLivenessCache::default();
    if let Some(ticket) = &ticket {
        let view =
            scan_lifecycle_queue(&ticket.directory, Some(ticket.rank()), Some(&mut liveness));
        observation.position_at_enqueue = view.ahead;
        observation.length_at_enqueue = view.live;
        observation.ticketed = true;
    }
    loop {
        let ahead = match ticket.as_mut() {
            Some(ticket) => {
                ticket.heartbeat();
                let view = scan_lifecycle_queue(
                    &ticket.directory,
                    Some(ticket.rank()),
                    Some(&mut liveness),
                );
                ticket.observe(&view);
                observation.passed_by = ticket.passed_by.len();
                observation.aged = ticket.aged;
                view.ahead
            }
            None => 0,
        };
        if ahead == 0 {
            let file = open_lock_file_nofollow(&path)?;
            if let Some(lock) = ExclusiveFileLock::try_acquire(file)? {
                // Revalidate the primary-root-derived location after acquiring
                // the persistent file. This catches a replaced runtime path or
                // lock name before the claim is used for a destructive action.
                if repository_lifecycle_lock_path(main_root)? != path {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "broker lifecycle lock path changed while it was acquired",
                    ));
                }
                verify_named_lock_file(&path, &lock.file)?;
                // Leave the queue only once the lock is ours, so no background
                // claim slips in between.
                drop(ticket.take());
                record_lifecycle_holder(&lock, kind);
                return Ok(lock);
            }
        }
        if observation.waited_behind.is_none() {
            observation.waited_behind = live_lifecycle_holder_operation(&path);
        }
        let now = Instant::now();
        if now >= deadline {
            let queued = ticket.as_ref().map_or(0, |ticket| {
                scan_lifecycle_queue(&ticket.directory, Some(ticket.rank()), None).live
            });
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "another broker lifecycle operation holds {}: {}; {}; waited {} s, retry this command after it finishes",
                    path.display(),
                    describe_lifecycle_holder(&path),
                    describe_lifecycle_queue(queued, ahead, observation),
                    started.elapsed().as_secs(),
                ),
            ));
        }
        // Background lanes stay quiet; their telemetry says they waited.
        if kind == LifecycleClaimKind::Foreground && now >= next_notice {
            eprintln!(
                "waiting for the repository lifecycle lock: {}; {} ahead of this command in the queue; waited {} of at most {} s",
                describe_lifecycle_holder(&path),
                ahead,
                started.elapsed().as_secs(),
                wait.as_secs(),
            );
            next_notice = now + LIFECYCLE_WAIT_NOTICE_EVERY;
        }
        thread::sleep(lifecycle_poll_interval(ahead).min(deadline.saturating_duration_since(now)));
    }
}

/// The queue, in words for a refusal: how many others wait and where this
/// command stood.
fn describe_lifecycle_queue(
    queued: usize,
    ahead: usize,
    observation: &LifecycleQueueObservation,
) -> String {
    if !observation.ticketed {
        return "the waiter queue was unavailable, so this command waited unordered".into();
    }
    format!(
        "{queued} other command(s) are waiting for it, {ahead} of them ahead of this one \
         ({} ahead when it queued)",
        observation.position_at_enqueue
    )
}

/// The host's monotonic clock in nanoseconds (`CLOCK_MONOTONIC`): shared by
/// every process on this host and unaffected by wall-clock changes, so ticket
/// order and staleness survive the clock being set forward or back. It
/// restarts at boot; tickets from before a reboot belong to dead processes.
#[cfg(unix)]
fn host_monotonic_ns() -> u128 {
    // SAFETY: timespec is plain integers (and, on some targets, padding), for
    // which all-zero bytes are a valid value.
    let mut now: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: `now` is a valid, writable timespec for the duration of the call.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 {
        return 0;
    }
    u128::try_from(now.tv_sec).unwrap_or(0) * 1_000_000_000
        + u128::try_from(now.tv_nsec).unwrap_or(0)
}

#[cfg(not(unix))]
fn host_monotonic_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos())
}

/// What a ticket's name fixes: deadline, enqueue time, PID and per-process
/// sequence. Together with the aged flag its contents carry it gives the
/// ticket's [`LifecycleRank`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct LifecycleTicketKey {
    deadline_ns: u128,
    enqueue_ns: u128,
    pid: u32,
    sequence: u64,
}

/// A ticket's place in the queue. Aged tickets come first, in enqueue order;
/// the rest by earliest deadline, then enqueue time; PID and sequence break
/// any tie. A total order derived from ticket name and contents only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LifecycleRank {
    key: LifecycleTicketKey,
    aged: bool,
}

impl LifecycleRank {
    /// A ticket that has not aged.
    #[cfg(test)]
    fn fresh(key: LifecycleTicketKey) -> Self {
        Self { key, aged: false }
    }

    fn order(&self) -> (u8, u128, u128, u32, u64) {
        let key = self.key;
        if self.aged {
            (0, key.enqueue_ns, key.deadline_ns, key.pid, key.sequence)
        } else {
            (1, key.deadline_ns, key.enqueue_ns, key.pid, key.sequence)
        }
    }
}

impl Ord for LifecycleRank {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.order().cmp(&other.order())
    }
}

impl PartialOrd for LifecycleRank {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A ticket's file name,
/// `<deadline ns>.<enqueue ns>.<pid>.<process start>.<sequence>`.
struct LifecycleTicketName {
    key: LifecycleTicketKey,
    /// The process's start time as [`crate::gates::process_start_time`] gives
    /// it, or 0 when the platform could not say.
    process_start: u64,
}

fn lifecycle_ticket_file_name(key: LifecycleTicketKey, process_start: u64) -> String {
    format!(
        "{}.{}.{}.{process_start}.{}",
        key.deadline_ns, key.enqueue_ns, key.pid, key.sequence
    )
}

fn parse_lifecycle_ticket_name(name: &str) -> Option<LifecycleTicketName> {
    let mut parts = name.split('.');
    let deadline_ns = parts.next()?.parse().ok()?;
    let enqueue_ns = parts.next()?.parse().ok()?;
    let pid = parts.next()?.parse().ok()?;
    let process_start = parts.next()?.parse().ok()?;
    let sequence = parts.next()?.parse().ok()?;
    if parts.next().is_some() || pid == 0 {
        return None;
    }
    Some(LifecycleTicketName {
        key: LifecycleTicketKey {
            deadline_ns,
            enqueue_ns,
            pid,
            sequence,
        },
        process_start,
    })
}

/// The heartbeat a ticket's contents carry, `heartbeat=<monotonic ns>`.
fn lifecycle_ticket_heartbeat(text: &str) -> Option<u128> {
    text.split_whitespace()
        .find_map(|word| word.strip_prefix("heartbeat="))
        .and_then(|value| value.parse().ok())
}

/// Whether a ticket's owner marked it aged (`aged=1`).
fn lifecycle_ticket_aged(text: &str) -> bool {
    text.split_whitespace().any(|word| word == "aged=1")
}

/// Whether a waiter passed by `passed_by` distinct later arrivals, and with no
/// earlier arrival ahead of it since `eligible_since_ns`, has aged at `now_ns`.
fn lifecycle_ticket_has_aged(
    passed_by: usize,
    eligible_since_ns: Option<u128>,
    now_ns: u128,
) -> bool {
    passed_by >= LIFECYCLE_TICKET_MAX_PASSES
        || eligible_since_ns.is_some_and(|since| {
            now_ns.saturating_sub(since) >= LIFECYCLE_TICKET_MAX_PASSED_FOR.as_nanos()
        })
}

fn own_process_start() -> u64 {
    static START: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *START.get_or_init(|| {
        i32::try_from(std::process::id())
            .ok()
            .and_then(crate::gates::process_start_time)
            .unwrap_or(0)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleTicketState {
    Live,
    /// Its process lives but stopped writing heartbeats.
    Stalled,
    /// Its process is gone, the PID now names another process, it predates
    /// this boot, or it is older than any wait.
    Dead,
}

/// Judge one ticket against the host monotonic clock `now_ns`. `heartbeat_ns`
/// is the last heartbeat its contents carry; without one the enqueue time
/// stands in. File modification times are never consulted, so a wall-clock
/// jump cannot age a live ticket.
fn lifecycle_ticket_state(
    name: &LifecycleTicketName,
    heartbeat_ns: Option<u128>,
    now_ns: u128,
) -> LifecycleTicketState {
    let last_beat = heartbeat_ns.unwrap_or(name.key.enqueue_ns);
    // Times well ahead of this clock were written before the last boot.
    let boot_slack = LIFECYCLE_TICKET_PRE_BOOT_SLACK.as_nanos();
    if name.key.enqueue_ns > now_ns.saturating_add(boot_slack)
        || last_beat > now_ns.saturating_add(boot_slack)
        || now_ns.saturating_sub(name.key.enqueue_ns) > LIFECYCLE_TICKET_MAX_AGE.as_nanos()
    {
        return LifecycleTicketState::Dead;
    }
    if name.key.pid != std::process::id() {
        if !crate::blockers::process_alive(i64::from(name.key.pid)) {
            return LifecycleTicketState::Dead;
        }
        if name.process_start != 0
            && let Some(current) = i32::try_from(name.key.pid)
                .ok()
                .and_then(crate::gates::process_start_time)
            && current != name.process_start
        {
            return LifecycleTicketState::Dead;
        }
    }
    if now_ns.saturating_sub(last_beat) > LIFECYCLE_TICKET_STALL.as_nanos() {
        LifecycleTicketState::Stalled
    } else {
        LifecycleTicketState::Live
    }
}

/// One waiter's recent liveness verdicts by ticket name, each trusted for
/// [`LIFECYCLE_LIVENESS_CACHE`], so a waiter rescanning a long queue does not
/// probe every process on every pass.
#[derive(Default)]
struct LifecycleLivenessCache {
    /// Per ticket name: its state, whether it had aged, and when it was read.
    verdicts: std::collections::HashMap<String, (LifecycleTicketState, bool, Instant)>,
}

impl LifecycleLivenessCache {
    fn cached(&self, name: &str, now: Instant) -> Option<(LifecycleTicketState, bool)> {
        self.verdicts
            .get(name)
            .filter(|(_, _, checked)| {
                now.saturating_duration_since(*checked) < LIFECYCLE_LIVENESS_CACHE
            })
            .map(|(state, aged, _)| (*state, *aged))
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct LifecycleQueueView {
    /// Live tickets ordered before `own`.
    ahead: usize,
    /// Live tickets other than `own`.
    live: usize,
    /// Live tickets ahead of `own` that were enqueued after it, by name.
    later_ahead: Vec<String>,
    /// Live tickets ahead of `own` that were enqueued before it.
    earlier_ahead: usize,
}

/// Count the live tickets in `directory` other than `own`, and those ahead of
/// it, removing dead ones. Anything that is not a regular file with a ticket
/// name is left alone and ignored. With a cache, a verdict younger than
/// [`LIFECYCLE_LIVENESS_CACHE`] is reused instead of probing again.
fn scan_lifecycle_queue(
    directory: &Path,
    own: Option<LifecycleRank>,
    mut cache: Option<&mut LifecycleLivenessCache>,
) -> LifecycleQueueView {
    let mut view = LifecycleQueueView::default();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return view;
    };
    let checked_at = Instant::now();
    let mut seen = std::collections::HashSet::new();
    for entry in entries.flatten() {
        let Ok(file_name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(name) = parse_lifecycle_ticket_name(&file_name) else {
            continue;
        };
        if own.is_some_and(|own| own.key == name.key) {
            continue;
        }
        let cached = cache
            .as_deref()
            .and_then(|cache| cache.cached(&file_name, checked_at));
        let (state, aged) = match cached {
            Some(verdict) => verdict,
            None => {
                let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
                    continue;
                };
                if !metadata.file_type().is_file() {
                    continue;
                }
                let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
                let heartbeat = lifecycle_ticket_heartbeat(&text);
                let aged = lifecycle_ticket_aged(&text);
                // Read the clock after the ticket, per entry: a scan of a slow
                // volume can outlast a heartbeat written during it.
                let state = lifecycle_ticket_state(&name, heartbeat, host_monotonic_ns());
                if let Some(cache) = cache.as_deref_mut() {
                    cache
                        .verdicts
                        .insert(file_name.clone(), (state, aged, checked_at));
                }
                (state, aged)
            }
        };
        match state {
            LifecycleTicketState::Dead => {
                // Best effort: another waiter may have removed it first.
                std::fs::remove_file(entry.path()).ok();
            }
            LifecycleTicketState::Stalled => {
                seen.insert(file_name);
            }
            LifecycleTicketState::Live => {
                view.live += 1;
                let rank = LifecycleRank {
                    key: name.key,
                    aged,
                };
                if let Some(own) = own
                    && rank < own
                {
                    view.ahead += 1;
                    if name.key.enqueue_ns > own.key.enqueue_ns {
                        view.later_ahead.push(file_name.clone());
                    } else {
                        view.earlier_ahead += 1;
                    }
                }
                seen.insert(file_name);
            }
        }
    }
    if let Some(cache) = cache {
        cache.verdicts.retain(|name, _| seen.contains(name));
    }
    view
}

fn lifecycle_waiters_directory(main_root: &Path) -> io::Result<PathBuf> {
    let directory = repository_runtime_directory(main_root)?.join(LIFECYCLE_WAITERS_DIR);
    ensure_real_directory(&directory)?;
    Ok(directory)
}

/// The waiters directory when it already exists as a real directory; a
/// nonblocking claim never creates it.
fn existing_lifecycle_waiters_directory(main_root: &Path) -> Option<PathBuf> {
    let directory = repository_runtime_directory(main_root)
        .ok()?
        .join(LIFECYCLE_WAITERS_DIR);
    std::fs::symlink_metadata(&directory)
        .ok()
        .filter(|metadata| metadata.file_type().is_dir())
        .map(|_| directory)
}

/// One waiter's place in the lifecycle queue, removed when dropped.
struct LifecycleTicket {
    directory: PathBuf,
    path: PathBuf,
    file: File,
    key: LifecycleTicketKey,
    process_start: u64,
    refreshed: Instant,
    /// Whether this waiter has aged; written into the ticket for others.
    aged: bool,
    /// Distinct later arrivals this waiter has seen ahead of it.
    passed_by: std::collections::HashSet<String>,
    /// Since when no earlier arrival has been ahead of this waiter.
    eligible_since_ns: Option<u128>,
}

impl LifecycleTicket {
    fn enqueue(main_root: &Path, wait: Duration) -> Option<Self> {
        let directory = lifecycle_waiters_directory(main_root).ok()?;
        let enqueue_ns = host_monotonic_ns();
        let key = LifecycleTicketKey {
            deadline_ns: enqueue_ns.saturating_add(wait.as_nanos()),
            enqueue_ns,
            pid: std::process::id(),
            sequence: LIFECYCLE_TICKET_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        };
        Self::create(directory, key, own_process_start())
    }

    fn create(directory: PathBuf, key: LifecycleTicketKey, process_start: u64) -> Option<Self> {
        let path = directory.join(lifecycle_ticket_file_name(key, process_start));
        let file = Self::open_new(&path)?;
        let ticket = Self {
            directory,
            path,
            file,
            key,
            process_start,
            refreshed: Instant::now(),
            aged: false,
            passed_by: std::collections::HashSet::new(),
            eligible_since_ns: None,
        };
        ticket.write_heartbeat();
        Some(ticket)
    }

    fn open_new(path: &Path) -> Option<File> {
        let mut options = OpenOptions::new();
        // `create_new` refuses anything already at the name, a symlink included.
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path).ok()
    }

    fn rank(&self) -> LifecycleRank {
        LifecycleRank {
            key: self.key,
            aged: self.aged,
        }
    }

    /// Fixed width, so a rewrite in place never leaves a longer tail behind.
    /// Only the owner writes its ticket; others only read it.
    fn write_heartbeat(&self) {
        use std::os::unix::fs::FileExt;
        let contents = format!(
            "heartbeat={:039} aged={} passed={:03} eligible_since={:039}\n",
            host_monotonic_ns(),
            u8::from(self.aged),
            self.passed_by.len().min(999),
            self.eligible_since_ns.unwrap_or(0),
        );
        self.file.write_all_at(contents.as_bytes(), 0).ok();
    }

    /// Count the later arrivals a scan found ahead of this waiter and note
    /// when no earlier arrival was; once it has aged, publish that at once.
    fn observe(&mut self, view: &LifecycleQueueView) {
        if self.aged {
            return;
        }
        let now_ns = host_monotonic_ns();
        self.passed_by.extend(view.later_ahead.iter().cloned());
        if view.earlier_ahead == 0 && self.eligible_since_ns.is_none() {
            self.eligible_since_ns = Some(now_ns);
        }
        if lifecycle_ticket_has_aged(self.passed_by.len(), self.eligible_since_ns, now_ns) {
            self.aged = true;
            self.write_heartbeat();
            crate::phase_telemetry::add("lifecycle_lock_ticket_aged", Duration::ZERO);
        }
    }

    /// At most every [`LIFECYCLE_TICKET_HEARTBEAT`]: write a fresh heartbeat so
    /// other waiters can tell a live waiter from a stopped one, and put the
    /// ticket back under its own name if something removed it (a waiter that
    /// misjudged it, or a cleanup), so this waiter keeps its place.
    fn heartbeat(&mut self) {
        if self.refreshed.elapsed() < LIFECYCLE_TICKET_HEARTBEAT {
            return;
        }
        self.refreshed = Instant::now();
        match std::fs::symlink_metadata(&self.path) {
            Ok(_) => self.write_heartbeat(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let path = self
                    .directory
                    .join(lifecycle_ticket_file_name(self.key, self.process_start));
                if let Some(file) = Self::open_new(&path) {
                    self.file = file;
                    self.path = path;
                    self.write_heartbeat();
                    crate::phase_telemetry::add("lifecycle_lock_ticket_recreated", Duration::ZERO);
                }
            }
            Err(_) => {}
        }
    }
}

impl Drop for LifecycleTicket {
    fn drop(&mut self) {
        // Best effort: a ticket left behind is removed by the next waiter once
        // this process is gone.
        std::fs::remove_file(&self.path).ok();
    }
}

fn ensure_real_directory(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "broker runtime path is not a real directory: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.file_type().is_dir() {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "broker runtime path changed while it was created: {}",
                        path.display()
                    ),
                ))
            }
        }
        Err(error) => Err(error),
    }
}
fn open_lock_file_nofollow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself. `verify_named_lock_file` rejects it
        // after open rather than following it to an unrelated lock target.
        options.custom_flags(0x0020_0000);
    }
    options.open(path)
}

fn verify_named_lock_file(path: &Path, opened: &File) -> io::Result<()> {
    let named = std::fs::symlink_metadata(path)?;
    let opened = opened.metadata()?;
    if !named.file_type().is_file() || !opened.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "worktree lifecycle lock is not a regular file: {}",
                path.display()
            ),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if named.dev() != opened.dev()
            || named.ino() != opened.ino()
            || named.nlink() != 1
            || opened.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "worktree lifecycle lock path changed while it was opened: {}",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

/// An exclusive lock held on an open file until the guard drops.
#[derive(Debug)]
pub(crate) struct ExclusiveFileLock {
    file: File,
    /// Set on a repository lifecycle claim: when and how it was acquired, so
    /// dropping the guard records the hold and clears the holder record.
    lifecycle_held_since: Option<(Instant, LifecycleClaimKind)>,
}

impl ExclusiveFileLock {
    /// Block until `file` is locked exclusively.
    pub(crate) fn acquire(file: File) -> io::Result<Self> {
        file.lock()?;
        Ok(Self {
            file,
            lifecycle_held_since: None,
        })
    }

    /// Write who holds the lock into the lock file, so a waiter can name its
    /// holder. Only ever written while the lock is held.
    pub(crate) fn record_holder(&self, holder: &str) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.file.set_len(0)?;
        self.file.write_all_at(holder.as_bytes(), 0)
    }

    /// Empty the holder line, still under the lock, so the next holder never
    /// appears to be this one before it records itself.
    pub(crate) fn clear_holder(&self) -> io::Result<()> {
        self.file.set_len(0)
    }

    /// Lock `file` exclusively if nobody holds it; `None` when it is held.
    pub(crate) fn try_acquire(file: File) -> io::Result<Option<Self>> {
        match file.try_lock() {
            Ok(()) => Ok(Some(Self {
                file,
                lifecycle_held_since: None,
            })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        if let Some((since, kind)) = self.lifecycle_held_since {
            let held = since.elapsed();
            crate::phase_telemetry::add("lifecycle_lock_held", held);
            if kind == LifecycleClaimKind::Foreground {
                crate::phase_telemetry::attach(
                    "lifecycle_lock",
                    serde_json::json!({
                        "held_ms": u64::try_from(held.as_millis()).unwrap_or(u64::MAX),
                    }),
                );
            }
            // Still under the lock: the next waiter must not read this holder.
            self.clear_holder().ok();
        }
        // Closing the descriptor would release the lock anyway; unlocking
        // first makes the release independent of any duplicated descriptor.
        let _ = self.file.unlock();
    }
}

/// A shared lock held on an open file until the guard drops.
#[derive(Debug)]
pub(crate) struct SharedFileLock {
    file: File,
}

impl SharedFileLock {
    /// Lock `file` shared without waiting; `None` when an exclusive user holds it.
    pub(crate) fn try_acquire(file: File) -> io::Result<Option<Self>> {
        match file.try_lock_shared() {
            Ok(()) => Ok(Some(Self { file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for SharedFileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[path = "artifact_use.rs"]
pub(crate) mod artifact_use;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_lifecycle_claim_returns_none_without_waiting_when_busy() {
        let repository = tempfile::tempdir().unwrap();
        let held = acquire_repository_lifecycle_lock(repository.path()).unwrap();
        assert!(
            try_acquire_repository_lifecycle_lock(repository.path(), "test_lane")
                .unwrap()
                .is_none()
        );
        drop(held);
        assert!(
            try_acquire_repository_lifecycle_lock(repository.path(), "test_lane")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn session_operation_guards_are_scoped_to_one_session() {
        let repository = tempfile::tempdir().unwrap();
        let shared = try_acquire_session_operation_guard(
            repository.path(),
            41,
            SessionOperationLockMode::Shared,
        )
        .unwrap()
        .expect("first session operation should be admitted");
        assert!(
            try_acquire_session_operation_guard(
                repository.path(),
                41,
                SessionOperationLockMode::Exclusive,
            )
            .unwrap()
            .is_none(),
            "retirement must wait while an operation is admitted"
        );
        let unrelated = try_acquire_session_operation_guard(
            repository.path(),
            42,
            SessionOperationLockMode::Exclusive,
        )
        .unwrap();
        assert!(unrelated.is_some(), "another session stays independent");
        drop(unrelated);
        drop(shared);
        assert!(
            try_acquire_session_operation_guard(
                repository.path(),
                41,
                SessionOperationLockMode::Exclusive,
            )
            .unwrap()
            .is_some(),
            "exclusive retirement can proceed after the operation ends"
        );
    }

    #[cfg(unix)]
    #[test]
    fn child_does_not_extend_shared_session_operation_after_parent_exits() {
        let repository = tempfile::tempdir().unwrap();
        let shared = try_acquire_session_operation_guard(
            repository.path(),
            41,
            SessionOperationLockMode::Shared,
        )
        .unwrap()
        .expect("operation should be admitted");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf ready; read _"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn child behind a readiness barrier");
        let mut ready = [0_u8; 5];
        std::io::Read::read_exact(child.stdout.as_mut().unwrap(), &mut ready)
            .expect("child reaches its read barrier");
        assert_eq!(&ready, b"ready");

        // Session guards stay close-on-exec. Child lifetime is covered by the
        // operation's durable artifact/gate/journal witnesses, not by leaking
        // the lock into unrelated long-lived helpers.
        drop(shared);
        // Another test thread's fork can hold the description open until its
        // exec closes it, so this claim may miss briefly. The child above stays
        // alive throughout, so one that had inherited the lock never lets go.
        let deadline = Instant::now() + Duration::from_secs(30);
        let exclusive = loop {
            let attempt = try_acquire_session_operation_guard(
                repository.path(),
                41,
                SessionOperationLockMode::Exclusive,
            )
            .unwrap();
            if attempt.is_some() || Instant::now() >= deadline {
                break attempt;
            }
            thread::sleep(Duration::from_millis(5));
        };
        assert!(exclusive.is_some());
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"done\n")
            .expect("release child barrier");
        child.wait().expect("wait for lock-holding child");
    }

    #[test]
    fn pr_watch_claim_excludes_another_tick_and_releases_on_drop() {
        let repository = tempfile::tempdir().unwrap();
        let claim = acquire_pr_watch_tick_lock(repository.path()).unwrap();
        let error = acquire_pr_watch_tick_lock(repository.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        // Checkout lifecycle and watch polling must remain independent.
        let lifecycle = acquire_repository_lifecycle_lock(repository.path()).unwrap();
        drop(lifecycle);
        drop(claim);
        let resumed = acquire_pr_watch_tick_lock(repository.path()).unwrap();
        assert!(
            repository
                .path()
                .join(".aethyme/run/pr-watch-tick.lock")
                .is_file()
        );
        drop(resumed);
    }

    #[cfg(unix)]
    #[test]
    fn pr_watch_claim_refuses_redirected_runtime_and_lock_paths() {
        let repository = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(repository.path().join(".aethyme")).unwrap();
        let run = repository.path().join(".aethyme/run");
        std::os::unix::fs::symlink(outside.path(), &run).unwrap();
        assert!(acquire_pr_watch_tick_lock(repository.path()).is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        std::fs::remove_file(&run).unwrap();
        std::fs::create_dir(&run).unwrap();
        let target = outside.path().join("target");
        std::fs::write(&target, b"preserve").unwrap();
        let path = run.join("pr-watch-tick.lock");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(acquire_pr_watch_tick_lock(repository.path()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve");
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&target, &path).unwrap();
        assert!(acquire_pr_watch_tick_lock(repository.path()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve");
    }

    fn lifecycle_claim_contends_in_order(first: &str, second: &str) {
        let repository = tempfile::tempdir().unwrap();
        let repository_root = repository.path().canonicalize().unwrap();
        let checkout = repository_root.join("linked-worktree");
        std::fs::create_dir(&checkout).unwrap();
        let primary_alias = repository_root.join(".");
        let held_by_first = acquire_repository_lifecycle_lock(&repository_root)
            .unwrap_or_else(|error| panic!("{first} claim failed: {error}"));
        let lock_path = repository_lifecycle_lock_path(&primary_alias).unwrap();
        assert!(
            lock_path.starts_with(repository_root.join(".aethyme/run")),
            "the persistent claim must be rooted in primary repository state"
        );
        assert!(
            !lock_path.starts_with(&checkout),
            "removing a linked checkout must not remove the repository claim"
        );
        let second_file = open_lock_file_nofollow(&lock_path).unwrap();
        assert!(
            matches!(
                second_file.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ),
            "{second} must wait while {first} holds the repository lifecycle claim"
        );
        assert!(lock_path.exists(), "the lock file must remain persistent");

        drop(held_by_first);
        second_file.try_lock().unwrap_or_else(|error| {
            panic!("{second} did not acquire after {first} released: {error}")
        });
    }

    #[test]
    fn a_held_lock_excludes_another_open_until_it_drops() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broker.lock");
        let held = ExclusiveFileLock::acquire(open_lock_file(&path).unwrap()).unwrap();

        // A second open file description contends, as another process would.
        let contender = open_lock_file(&path).unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        contender
            .try_lock()
            .expect("the lock is free once the guard drops");
    }

    #[test]
    fn a_holder_written_into_a_held_lock_is_readable_by_a_waiter() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("owner.lock");
        std::fs::write(&path, b"a much longer previous holder line").unwrap();
        let held = ExclusiveFileLock::acquire(open_lock_file(&path).unwrap()).unwrap();
        held.record_holder("s812 since=1").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "s812 since=1");
    }

    #[test]
    fn opening_a_lock_file_keeps_its_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("slot.lock");
        std::fs::write(&path, b"holder").unwrap();
        let _lock = ExclusiveFileLock::acquire(open_lock_file(&path).unwrap()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"holder");
    }

    #[test]
    fn a_foreground_claim_outlasts_a_holder_past_the_old_ten_second_wait() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let held = acquire_repository_lifecycle_lock(&root).unwrap();
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_secs(11));
            drop(held);
        });
        let started = Instant::now();
        acquire_repository_lifecycle_lock(&root).unwrap();
        assert!(started.elapsed() >= Duration::from_secs(10));
        release.join().unwrap();
    }

    #[test]
    fn a_foreground_claim_never_waits_past_the_commands_git_deadline() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let _held = acquire_repository_lifecycle_lock(&root).unwrap();
        let started = Instant::now();
        let error = crate::git::with_git_deadline(Duration::from_millis(200), || {
            acquire_repository_lifecycle_lock(&root)
        })
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_refusal_names_the_holder_how_long_it_held_and_that_it_runs() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let _held = acquire_repository_lifecycle_lock(&root).unwrap();
        let message =
            acquire_repository_lifecycle_lock_with_wait(&root, Duration::from_millis(100))
                .err()
                .unwrap()
                .to_string();
        assert!(
            message.contains(&format!("pid {} (", std::process::id()))
                && message.contains("has held it for")
                && message.contains("is running")
                && message.contains("retry this command"),
            "{message}"
        );
    }

    #[test]
    fn a_refusal_does_not_blame_an_exited_or_unrecorded_holder() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let path = repository_lifecycle_lock_path(&root).unwrap();
        // Held by something that recorded nothing, as an older binary does.
        let held = ExclusiveFileLock::acquire(open_lock_file_nofollow(&path).unwrap()).unwrap();
        let refusal = || {
            acquire_repository_lifecycle_lock_with_wait(&root, Duration::from_millis(100))
                .err()
                .unwrap()
                .to_string()
        };
        let message = refusal();
        assert!(message.contains("did not record itself"), "{message}");

        let mut exited = std::process::Command::new("true").spawn().unwrap();
        let pid = exited.id();
        exited.wait().unwrap();
        held.record_holder(&format!("pid={pid} since_ms=1 operation=broker start"))
            .unwrap();
        let message = refusal();
        assert!(
            message.contains(&format!("pid {pid}, has exited")) && message.contains("unidentified"),
            "{message}"
        );
    }

    fn lifecycle_tickets(root: &Path) -> Vec<String> {
        let directory = root.join(".aethyme/run").join(LIFECYCLE_WAITERS_DIR);
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn wait_for_tickets(root: &Path, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while lifecycle_tickets(root).len() < count {
            assert!(Instant::now() < deadline, "waiters never queued");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn queued_waiters_take_the_lock_in_the_order_they_queued() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let held = acquire_repository_lifecycle_lock(&root).unwrap();
        let (order_tx, order_rx) = std::sync::mpsc::channel();
        let mut waiters = Vec::new();
        for (index, label) in ["first", "second", "third"].into_iter().enumerate() {
            let waiter_root = root.clone();
            let order_tx = order_tx.clone();
            waiters.push(thread::spawn(move || {
                let claim = acquire_repository_lifecycle_lock(&waiter_root).unwrap();
                order_tx.send(label).unwrap();
                thread::sleep(Duration::from_millis(100));
                drop(claim);
            }));
            // Each waiter queues before the next one starts.
            wait_for_tickets(&root, index + 1);
        }
        drop(held);
        for waiter in waiters {
            waiter.join().unwrap();
        }
        let order: Vec<_> = order_rx.try_iter().collect();
        assert_eq!(order, ["first", "second", "third"]);
        assert!(
            lifecycle_tickets(&root).is_empty(),
            "every waiter leaves the queue once it holds the lock"
        );
    }

    #[test]
    fn a_dead_waiters_ticket_is_skipped_and_removed() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        let mut exited = std::process::Command::new("true").spawn().unwrap();
        let pid = exited.id();
        exited.wait().unwrap();
        // Queued long before this command, by a process that has exited.
        let dead = directory.join(format!("1.1.{pid}.0.0"));
        std::fs::write(&dead, b"").unwrap();
        let started = Instant::now();
        let claim =
            acquire_repository_lifecycle_lock_with_wait(&root, Duration::from_secs(5)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!dead.exists(), "a dead waiter's ticket is reaped");
        drop(claim);
    }

    #[test]
    fn a_ticket_whose_pid_now_names_another_process_is_reaped() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        let mut live = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = live.id();
        let Some(start) = crate::gates::process_start_time(pid as i32) else {
            // This platform cannot tell a reused PID; nothing to prove here.
            live.kill().ok();
            live.wait().ok();
            return;
        };
        // The ticket names the PID with another start time: a reused PID.
        let now = host_monotonic_ns();
        let reused = directory.join(format!("{now}.{now}.{pid}.{}.0", start.wrapping_add(1)));
        std::fs::write(&reused, format!("heartbeat={now}")).unwrap();
        let view = scan_lifecycle_queue(
            &directory,
            Some(LifecycleRank::fresh(LifecycleTicketKey {
                deadline_ns: u128::MAX,
                enqueue_ns: u128::MAX,
                pid: std::process::id(),
                sequence: 0,
            })),
            None,
        );
        assert_eq!(view, LifecycleQueueView::default());
        assert!(!reused.exists());
        live.kill().ok();
        live.wait().ok();
    }

    #[test]
    fn a_live_waiter_that_stopped_refreshing_its_ticket_is_passed_over_not_removed() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        let mut live = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = live.id();
        let start = crate::gates::process_start_time(pid as i32).unwrap_or(0);
        // Queued a minute ago and silent since; its process is still alive.
        let now = host_monotonic_ns();
        let then = now.saturating_sub(Duration::from_secs(60).as_nanos());
        let stalled = directory.join(format!("{then}.{then}.{pid}.{start}.0"));
        std::fs::write(&stalled, format!("heartbeat={then}")).unwrap();
        let claim =
            acquire_repository_lifecycle_lock_with_wait(&root, Duration::from_secs(5)).unwrap();
        assert!(stalled.exists(), "a live process's ticket is never removed");
        drop(claim);
        live.kill().ok();
        live.wait().ok();
    }

    #[test]
    fn a_nonblocking_claim_yields_to_a_queued_foreground_waiter() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        let ticket =
            LifecycleTicket::create(directory, far_deadline_key(), own_process_start()).unwrap();
        assert!(
            try_acquire_repository_lifecycle_lock(&root, "test_lane")
                .unwrap()
                .is_none(),
            "background maintenance must not take the free lock from a queued waiter"
        );
        {
            let _foreground = ForegroundLifecycleTryClaim::enter();
            let claim = try_acquire_repository_lifecycle_lock(&root, "test_lane").unwrap();
            assert!(claim.is_some(), "a foreground try-claim does not yield");
        }
        drop(ticket);
        assert!(
            try_acquire_repository_lifecycle_lock(&root, "test_lane")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_timeout_records_its_wait_and_names_the_queue() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let held = acquire_repository_lifecycle_lock(&root).unwrap();
        let queued_root = root.clone();
        // Queued first with the earlier deadline, so it stays ahead of the
        // claim below until it gives up after one second.
        let queued = thread::spawn(move || {
            let outcome =
                acquire_repository_lifecycle_lock_with_wait(&queued_root, Duration::from_secs(1));
            assert!(outcome.is_err(), "the lock stays held past its budget");
        });
        wait_for_tickets(&root, 1);

        let recorder = crate::phase_telemetry::Recorder::default();
        let error = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            acquire_repository_lifecycle_lock_with_wait(&root, Duration::from_millis(1500))
                .unwrap_err()
        };
        drop(held);
        queued.join().unwrap();

        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        let message = error.to_string();
        assert!(
            message.contains("0 other command(s) are waiting for it, 0 of them ahead of this one")
                && message.contains("(1 ahead when it queued)")
                && message.contains(&format!("pid {} (", std::process::id()))
                && message.contains("retry this command"),
            "{message}"
        );
        let (millis, counts) = recorder.totals();
        assert_eq!(counts.get("lifecycle_lock_wait"), Some(&1));
        assert!(
            millis
                .get("lifecycle_lock_wait")
                .is_some_and(|ms| *ms >= 1400)
        );
        assert!(!counts.contains_key("lifecycle_lock_held"));
        let attached = &recorder.attachments()["lifecycle_lock"];
        assert_eq!(attached["outcome"], "timeout");
        assert_eq!(attached["claim"], "foreground");
        assert_eq!(attached["queue_position_at_enqueue"], 1);
        assert_eq!(attached["queue_length_at_enqueue"], 1);
        assert_eq!(attached["queued"], true);
        assert!(attached["wait_ms"].as_u64().is_some_and(|ms| ms >= 1400));
        assert!(attached["waited_behind"].is_string());
        assert!(
            lifecycle_tickets(&root).is_empty(),
            "a waiter that gave up leaves the queue"
        );
    }

    #[test]
    fn an_acquired_claim_records_its_wait_and_its_hold() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let recorder = crate::phase_telemetry::Recorder::default();
        {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            let claim = acquire_repository_lifecycle_lock(&root).unwrap();
            thread::sleep(Duration::from_millis(30));
            drop(claim);
        }
        let (millis, counts) = recorder.totals();
        assert_eq!(counts.get("lifecycle_lock_wait"), Some(&1));
        assert_eq!(counts.get("lifecycle_lock_held"), Some(&1));
        assert!(
            millis
                .get("lifecycle_lock_held")
                .is_some_and(|ms| *ms >= 30)
        );
        let attached = &recorder.attachments()["lifecycle_lock"];
        assert_eq!(attached["outcome"], "acquired");
        assert_eq!(attached["queue_position_at_enqueue"], 0);
        assert!(attached["held_ms"].as_u64().is_some_and(|ms| ms >= 30));
        // The release clears the holder record before unlocking.
        let path = repository_lifecycle_lock_path(&root).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "");
    }

    #[test]
    fn a_holder_record_names_its_kind_and_session_for_the_refusal() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let path = repository_lifecycle_lock_path(&root).unwrap();
        let held = ExclusiveFileLock::acquire(open_lock_file_nofollow(&path).unwrap()).unwrap();
        held.record_holder(&format!(
            "pid={} since_ms=1 kind=foreground session=2184 operation=broker finish",
            std::process::id()
        ))
        .unwrap();
        let message =
            acquire_repository_lifecycle_lock_with_wait(&root, Duration::from_millis(100))
                .unwrap_err()
                .to_string();
        assert!(
            message.contains("(broker finish, session 2184, foreground) has held it for"),
            "{message}"
        );
        drop(held);
    }

    /// A live ticket for this process that sorts behind any claim made in
    /// the next three minutes.
    fn far_deadline_key() -> LifecycleTicketKey {
        let now = host_monotonic_ns();
        LifecycleTicketKey {
            deadline_ns: now + Duration::from_secs(180).as_nanos(),
            enqueue_ns: now,
            pid: std::process::id(),
            sequence: u64::MAX - LIFECYCLE_TICKET_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        }
    }

    #[test]
    fn a_short_budget_waiter_is_served_before_an_earlier_long_one() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let held = acquire_repository_lifecycle_lock(&root).unwrap();
        let (order_tx, order_rx) = std::sync::mpsc::channel();
        let long_root = root.clone();
        let long_tx = order_tx.clone();
        // Queued first, with the full three-minute budget a start has.
        let long = thread::spawn(move || {
            let claim = acquire_repository_lifecycle_lock(&long_root).unwrap();
            long_tx.send("long").unwrap();
            drop(claim);
        });
        wait_for_tickets(&root, 1);
        // Queued second, with the ten-second budget gate admission has.
        let short_root = root.clone();
        let short = thread::spawn(move || {
            let claim =
                acquire_repository_lifecycle_lock_with_wait(&short_root, Duration::from_secs(10))
                    .unwrap();
            order_tx.send("short").unwrap();
            thread::sleep(Duration::from_millis(100));
            drop(claim);
        });
        wait_for_tickets(&root, 2);
        drop(held);
        long.join().unwrap();
        short.join().unwrap();
        let order: Vec<_> = order_rx.try_iter().collect();
        assert_eq!(order, ["short", "long"]);
    }

    #[test]
    fn a_waiter_near_its_deadline_outranks_newer_arrivals_with_longer_budgets() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        // A waiter with five seconds left.
        let now = host_monotonic_ns();
        let waiting = LifecycleTicket::create(
            directory.clone(),
            LifecycleTicketKey {
                deadline_ns: now + Duration::from_secs(5).as_nanos(),
                enqueue_ns: now.saturating_sub(Duration::from_secs(170).as_nanos()),
                pid: std::process::id(),
                sequence: 0,
            },
            own_process_start(),
        )
        .unwrap();
        let arrival = |budget: Duration| {
            let at = host_monotonic_ns();
            LifecycleTicketKey {
                deadline_ns: at + budget.as_nanos(),
                enqueue_ns: at,
                pid: std::process::id(),
                sequence: u64::MAX,
            }
        };
        // Any newer arrival whose budget is at least the time it has left
        // queues behind it, however short that budget is.
        for budget in [Duration::from_secs(5), Duration::from_secs(10)] {
            let view = scan_lifecycle_queue(
                &directory,
                Some(LifecycleRank::fresh(arrival(budget))),
                None,
            );
            assert_eq!(view.ahead, 1, "{budget:?}");
        }
        // Only a budget shorter than its remaining time goes first.
        let view = scan_lifecycle_queue(
            &directory,
            Some(LifecycleRank::fresh(arrival(Duration::from_millis(100)))),
            None,
        );
        assert_eq!(view.ahead, 0);
        drop(waiting);
    }

    #[test]
    fn a_foreground_command_defers_session_retirement_to_the_detached_worker() {
        assert!(inline_session_retirement_allowed(), "library callers judge");
        let foreground = ForegroundCommand::enter();
        assert!(
            !inline_session_retirement_allowed(),
            "a CLI command only records due sessions"
        );
        {
            let _detached = DetachedBackgroundWorker::enter();
            assert!(
                inline_session_retirement_allowed(),
                "the detached worker is dispatched through the CLI and judges"
            );
        }
        assert!(!inline_session_retirement_allowed());
        drop(foreground);
        assert!(inline_session_retirement_allowed());
    }

    #[test]
    fn a_background_lane_queues_after_five_consecutive_yields() {
        let _detached = DetachedBackgroundWorker::enter();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        // A foreground waiter that never leaves: the queue is never empty.
        let foreground =
            LifecycleTicket::create(directory, far_deadline_key(), own_process_start()).unwrap();
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        for attempt in 1..=BACKGROUND_YIELD_COUNT {
            assert!(
                try_acquire_repository_lifecycle_lock(&root, "test_lane")
                    .unwrap()
                    .is_none(),
                "attempt {attempt} yields"
            );
            assert_eq!(
                recorder.attachments()["lifecycle_lock_background"]["consecutive_yields"],
                attempt
            );
        }
        let started = Instant::now();
        let claim = try_acquire_repository_lifecycle_lock(&root, "test_lane").unwrap();
        assert!(claim.is_some(), "the promoted lane takes its turn");
        assert!(started.elapsed() < BACKGROUND_PROMOTED_WAIT);
        let attached = &recorder.attachments()["lifecycle_lock_background"];
        assert_eq!(attached["outcome"], "promoted_acquired");
        assert_eq!(attached["consecutive_yields"], BACKGROUND_YIELD_COUNT);
        let (_, counts) = recorder.totals();
        assert_eq!(counts.get("lifecycle_lock_promoted_wait"), Some(&1));
        assert_eq!(
            counts.get("lifecycle_lock_yielded_to_waiter"),
            Some(&u64::from(BACKGROUND_YIELD_COUNT))
        );
        drop(claim);
        // The run of yields starts over after the promotion.
        assert!(
            try_acquire_repository_lifecycle_lock(&root, "test_lane")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            recorder.attachments()["lifecycle_lock_background"]["consecutive_yields"],
            1
        );
        drop(foreground);
    }

    #[test]
    fn a_background_lane_queues_after_a_minute_of_yielding() {
        let _detached = DetachedBackgroundWorker::enter();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        let foreground =
            LifecycleTicket::create(directory, far_deadline_key(), own_process_start()).unwrap();
        let record = background_yields_path(&root, "test_lane", true).unwrap();
        write_background_yields(
            &record,
            BackgroundYields {
                since_ns: host_monotonic_ns()
                    .saturating_sub((BACKGROUND_YIELD_LIMIT + Duration::from_secs(1)).as_nanos()),
                count: 1,
            },
        );
        let claim = try_acquire_repository_lifecycle_lock(&root, "test_lane").unwrap();
        assert!(claim.is_some(), "a minute of yielding promotes the lane");
        assert!(!record.exists(), "the promotion ends the run of yields");
        drop(claim);
        drop(foreground);
    }

    #[test]
    fn an_inline_lane_never_waits_and_leaves_promotion_to_the_detached_worker() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        let foreground =
            LifecycleTicket::create(directory, far_deadline_key(), own_process_start()).unwrap();
        // The lane is already due: five yields in a row.
        let record = background_yields_path(&root, "test_lane", true).unwrap();
        write_background_yields(
            &record,
            BackgroundYields {
                since_ns: host_monotonic_ns(),
                count: BACKGROUND_YIELD_COUNT,
            },
        );
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        let started = Instant::now();
        assert!(
            try_acquire_repository_lifecycle_lock(&root, "test_lane")
                .unwrap()
                .is_none(),
            "inline, a due lane still only yields"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "inline never waits"
        );
        let attached = &recorder.attachments()["lifecycle_lock_background"];
        assert_eq!(attached["context"], "inline");
        assert_eq!(attached["outcome"], "yielded");
        assert_eq!(attached["promotion_deferred_to_detached_worker"], true);
        assert_eq!(attached["consecutive_yields"], BACKGROUND_YIELD_COUNT + 1);
        let (_, counts) = recorder.totals();
        assert!(!counts.contains_key("lifecycle_lock_promoted_wait"));

        // The streak carries over: the next detached pass promotes at once.
        let _detached = DetachedBackgroundWorker::enter();
        let claim = try_acquire_repository_lifecycle_lock(&root, "test_lane").unwrap();
        assert!(claim.is_some());
        let attached = &recorder.attachments()["lifecycle_lock_background"];
        assert_eq!(attached["context"], "detached");
        assert_eq!(attached["outcome"], "promoted_acquired");
        assert_eq!(attached["promoted_capped_gc_lock"], false);
        drop(claim);
        drop(foreground);
    }

    #[test]
    fn a_promoted_wait_under_the_gc_lock_is_capped_below_the_foreground_gc_wait() {
        // A `start` at the checkout limit waits this long for the GC lock; a
        // background wait made while holding it must end well before.
        assert!(BACKGROUND_PROMOTED_WAIT_UNDER_GC_LOCK < crate::gc::GC_LOCK_FOREGROUND_WAIT / 2);
        let _detached = DetachedBackgroundWorker::enter();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        // The lifecycle lock stays held throughout, so the promoted lane can
        // only time out; taken before any ticket exists.
        let held = acquire_repository_lifecycle_lock(&root).unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        let foreground =
            LifecycleTicket::create(directory, far_deadline_key(), own_process_start()).unwrap();
        let record = background_yields_path(&root, "test_lane", true).unwrap();
        write_background_yields(
            &record,
            BackgroundYields {
                since_ns: host_monotonic_ns(),
                count: BACKGROUND_YIELD_COUNT,
            },
        );
        let gc_lock = crate::gc::GcLock::acquire(&root).unwrap();
        assert!(crate::gc::this_process_holds_gc_lock());
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        let started = Instant::now();
        let claim = try_acquire_repository_lifecycle_lock(&root, "test_lane").unwrap();
        let waited = started.elapsed();
        assert!(claim.is_none(), "the held lifecycle lock never came free");
        assert!(
            waited >= BACKGROUND_PROMOTED_WAIT_UNDER_GC_LOCK - Duration::from_millis(100)
                && waited < BACKGROUND_PROMOTED_WAIT_UNDER_GC_LOCK + Duration::from_secs(2),
            "{waited:?}"
        );
        let attached = &recorder.attachments()["lifecycle_lock_background"];
        assert_eq!(attached["promoted_capped_gc_lock"], true);
        assert_eq!(attached["outcome"], "promoted_timeout");
        drop(gc_lock);
        assert!(!crate::gc::this_process_holds_gc_lock());
        drop(foreground);
        drop(held);
    }

    #[test]
    fn a_long_waiter_ages_and_acquires_under_a_stream_of_short_budgets() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let held = acquire_repository_lifecycle_lock(&root).unwrap();
        let (order_tx, order_rx) = std::sync::mpsc::channel();
        let long_root = root.clone();
        let long_tx = order_tx.clone();
        let long = thread::spawn(move || {
            let claim = acquire_repository_lifecycle_lock(&long_root).unwrap();
            long_tx.send("long").unwrap();
            drop(claim);
        });
        wait_for_tickets(&root, 1);
        // A continuous stream of 10 s budgets, each holding the lock 400 ms:
        // by deadline alone every one of them would go first.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let producer_stop = stop.clone();
        let producer_root = root.clone();
        let producer = thread::spawn(move || {
            let mut shorts = Vec::new();
            while !producer_stop.load(Ordering::SeqCst) {
                let root = producer_root.clone();
                let order_tx = order_tx.clone();
                shorts.push(thread::spawn(move || {
                    if let Ok(claim) =
                        acquire_repository_lifecycle_lock_with_wait(&root, Duration::from_secs(10))
                    {
                        order_tx.send("short").ok();
                        thread::sleep(Duration::from_millis(400));
                        drop(claim);
                    }
                }));
                thread::sleep(Duration::from_millis(150));
            }
            for short in shorts {
                short.join().unwrap();
            }
        });
        // Let a few short arrivals queue ahead, then free the lock.
        wait_for_tickets(&root, 3);
        let released = Instant::now();
        drop(held);
        let mut served_before_long = 0;
        loop {
            match order_rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                "long" => break,
                _ => served_before_long += 1,
            }
        }
        let waited = released.elapsed();
        stop.store(true, Ordering::SeqCst);
        long.join().unwrap();
        producer.join().unwrap();
        assert!(
            served_before_long <= LIFECYCLE_TICKET_MAX_PASSES
                || waited <= LIFECYCLE_TICKET_MAX_PASSED_FOR + Duration::from_secs(5),
            "served {served_before_long} short claims first over {waited:?}"
        );
        assert!(
            waited <= LIFECYCLE_TICKET_MAX_PASSED_FOR + Duration::from_secs(5),
            "{waited:?}"
        );
    }

    #[test]
    fn an_aged_ticket_outranks_every_later_arrival_and_the_order_stays_total() {
        let at = |enqueue_s: u64, budget_s: u64, aged: bool| LifecycleRank {
            key: LifecycleTicketKey {
                deadline_ns: Duration::from_secs(enqueue_s + budget_s).as_nanos(),
                enqueue_ns: Duration::from_secs(enqueue_s).as_nanos(),
                pid: 1,
                sequence: enqueue_s,
            },
            aged,
        };
        let long = at(100, 180, false);
        let short = at(110, 10, false);
        assert!(short < long, "unaged: earliest deadline first");
        let long_aged = at(100, 180, true);
        assert!(
            long_aged < short,
            "aged: ahead of a later arrival whatever its budget"
        );
        assert!(long_aged < at(101, 1, false));
        // Among aged tickets, the earlier arrival goes first.
        assert!(at(90, 300, true) < long_aged);
        // A total order: sorting is stable and consistent in any input order.
        let mut ranks = vec![short, long_aged, at(90, 300, true), long, at(101, 1, false)];
        ranks.sort();
        let mut reversed = ranks.clone();
        reversed.reverse();
        reversed.sort();
        assert_eq!(ranks, reversed);
        assert_eq!(ranks[0], at(90, 300, true));
        assert_eq!(ranks[1], long_aged);
    }

    #[test]
    fn a_waiter_ages_after_three_passes_or_twenty_seconds() {
        let second = Duration::from_secs(1).as_nanos();
        let now = 1_000 * second;
        assert!(!lifecycle_ticket_has_aged(2, None, now));
        assert!(lifecycle_ticket_has_aged(
            LIFECYCLE_TICKET_MAX_PASSES,
            None,
            now
        ));
        assert!(!lifecycle_ticket_has_aged(0, Some(now - 19 * second), now));
        assert!(lifecycle_ticket_has_aged(0, Some(now - 20 * second), now));
        // The owner publishes the flag, and others rank by it.
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        let mut ticket =
            LifecycleTicket::create(directory, far_deadline_key(), own_process_start()).unwrap();
        let view = LifecycleQueueView {
            later_ahead: vec!["a".into(), "b".into(), "c".into()],
            ..LifecycleQueueView::default()
        };
        ticket.observe(&view);
        assert!(ticket.aged);
        assert!(lifecycle_ticket_aged(
            &std::fs::read_to_string(&ticket.path).unwrap()
        ));
        drop(ticket);
    }

    #[test]
    fn a_heartbeat_written_during_a_slow_scan_is_not_mistaken_for_a_pre_boot_ticket() {
        let now = host_monotonic_ns();
        let name = LifecycleTicketName {
            key: LifecycleTicketKey {
                deadline_ns: now + Duration::from_secs(180).as_nanos(),
                enqueue_ns: now,
                pid: std::process::id(),
                sequence: 0,
            },
            process_start: 0,
        };
        // A heartbeat 30 s past the clock reading the scan took earlier.
        let later = now + Duration::from_secs(30).as_nanos();
        assert_eq!(
            lifecycle_ticket_state(&name, Some(later), now),
            LifecycleTicketState::Live
        );
        // Far ahead of the clock means written before the last boot.
        let pre_boot = LifecycleTicketName {
            key: LifecycleTicketKey {
                enqueue_ns: now + Duration::from_secs(120).as_nanos(),
                ..name.key
            },
            process_start: 0,
        };
        assert_eq!(
            lifecycle_ticket_state(&pre_boot, None, now),
            LifecycleTicketState::Dead
        );
    }

    #[test]
    fn a_waiter_puts_back_a_ticket_someone_removed() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        let mut ticket =
            LifecycleTicket::create(directory, far_deadline_key(), own_process_start()).unwrap();
        let path = ticket.path.clone();
        std::fs::remove_file(&path).unwrap();
        ticket.refreshed = Instant::now() - LIFECYCLE_TICKET_HEARTBEAT;
        ticket.heartbeat();
        assert!(path.exists(), "the ticket is back under its own name");
        let heartbeat = lifecycle_ticket_heartbeat(&std::fs::read_to_string(&path).unwrap());
        assert!(heartbeat.is_some());
        drop(ticket);
        assert!(!path.exists());
    }

    #[test]
    fn a_ticket_with_an_old_modification_time_but_a_fresh_heartbeat_stays_live() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        let ticket =
            LifecycleTicket::create(directory.clone(), far_deadline_key(), own_process_start())
                .unwrap();
        // The wall clock jumped: the file now looks an hour old.
        std::fs::File::options()
            .write(true)
            .open(&ticket.path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        let view = scan_lifecycle_queue(&directory, None, None);
        assert_eq!(view.live, 1);
        assert!(ticket.path.exists());
        drop(ticket);
    }

    #[test]
    fn a_waiter_reuses_a_liveness_verdict_for_a_second_and_then_rechecks() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let directory = lifecycle_waiters_directory(&root).unwrap();
        let ticket =
            LifecycleTicket::create(directory.clone(), far_deadline_key(), own_process_start())
                .unwrap();
        let mut cache = LifecycleLivenessCache::default();
        assert_eq!(
            scan_lifecycle_queue(&directory, None, Some(&mut cache)).live,
            1
        );
        // The waiter falls silent; within the cache window the verdict holds.
        let silent = host_monotonic_ns().saturating_sub(Duration::from_secs(60).as_nanos());
        std::fs::write(&ticket.path, format!("heartbeat={silent:039}\n")).unwrap();
        assert_eq!(
            scan_lifecycle_queue(&directory, None, Some(&mut cache)).live,
            1
        );
        // Once the verdict expires the ticket is judged again: stalled.
        for (_, _, checked) in cache.verdicts.values_mut() {
            *checked -= LIFECYCLE_LIVENESS_CACHE;
        }
        assert_eq!(
            scan_lifecycle_queue(&directory, None, Some(&mut cache)).live,
            0
        );
        assert!(ticket.path.exists(), "a stalled live waiter is not removed");
        drop(ticket);
    }

    #[test]
    fn only_the_head_of_the_queue_polls_quickly() {
        assert_eq!(lifecycle_poll_interval(0), LIFECYCLE_LOCK_POLL);
        assert_eq!(lifecycle_poll_interval(1), LIFECYCLE_QUEUED_POLL);
        assert!(LIFECYCLE_QUEUED_POLL >= Duration::from_millis(250));
    }

    #[test]
    fn ticket_names_parse_only_in_the_exact_shape() {
        let parsed = parse_lifecycle_ticket_name("19.17.42.99.3").unwrap();
        assert_eq!(
            parsed.key,
            LifecycleTicketKey {
                deadline_ns: 19,
                enqueue_ns: 17,
                pid: 42,
                sequence: 3
            }
        );
        assert_eq!(parsed.process_start, 99);
        assert_eq!(lifecycle_ticket_file_name(parsed.key, 99), "19.17.42.99.3");
        for invalid in [
            "17.42.99.3",
            "19.17.42.99.3.1",
            "19.17.0.99.3",
            "x.17.42.99.3",
            ".DS_Store",
        ] {
            assert!(parse_lifecycle_ticket_name(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn adoption_claim_blocks_cleanup_claim() {
        lifecycle_claim_contends_in_order("adoption", "cleanup");
    }

    #[test]
    fn cleanup_claim_blocks_adoption_claim() {
        lifecycle_claim_contends_in_order("cleanup", "adoption");
    }

    #[test]
    fn session_commit_claims_are_exclusive_per_session_and_independent_between_sessions() {
        let repository = tempfile::tempdir().unwrap();
        let held = try_acquire_session_commit_lock(repository.path(), 42).unwrap();
        let busy = try_acquire_session_commit_lock(repository.path(), 42).unwrap_err();
        assert_eq!(busy.kind(), io::ErrorKind::WouldBlock);
        let independent = try_acquire_session_commit_lock(repository.path(), 43).unwrap();

        drop(held);
        try_acquire_session_commit_lock(repository.path(), 42).unwrap();
        drop(independent);
    }

    #[test]
    fn lifecycle_claim_times_out_with_a_retry_instruction() {
        let repository = tempfile::tempdir().unwrap();
        let _held = acquire_repository_lifecycle_lock(repository.path()).unwrap();

        let error = acquire_repository_lifecycle_lock_with_wait(
            repository.path(),
            Duration::from_millis(10),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(error.to_string().contains("retry this command"));
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_claim_aliases_the_same_canonical_primary_repository() {
        let holder = tempfile::tempdir().unwrap();
        let repository = holder.path().join("repository");
        let alias = holder.path().join("repository-alias");
        std::fs::create_dir(&repository).unwrap();
        std::os::unix::fs::symlink(&repository, &alias).unwrap();

        assert_eq!(
            repository_lifecycle_lock_path(&repository).unwrap(),
            repository_lifecycle_lock_path(&alias).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_claim_refuses_a_symlink_lockfile_without_touching_its_target() {
        let repository = tempfile::tempdir().unwrap();
        let path = repository_lifecycle_lock_path(repository.path()).unwrap();
        let target = repository.path().join("outside-lock-target");
        std::fs::write(&target, b"preserve target").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        assert!(acquire_repository_lifecycle_lock(repository.path()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve target");
    }
}
