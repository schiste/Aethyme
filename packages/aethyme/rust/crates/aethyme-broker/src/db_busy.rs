//! One lock-wait policy for every broker-database connection.
//!
//! The broker database is shared by every agent process on a repository:
//! short CLI commands, `broker advanced exec` wrappers that live for hours,
//! GC workers and watchers. Connections used to wait at most 5 s for another
//! writer (SQLite's `busy_timeout`), and on the busiest repository 63 commands
//! failed with `database is locked` in six hours, with nothing in their
//! metric lines to say how long they had waited or who held the lock.
//!
//! This module gives every connection the same policy:
//!
//! - a busy handler that retries with jittered exponential backoff for up to
//!   [`BUSY_WAIT_BUDGET`] (normal contention clears well inside it);
//! - writable connections start every `transaction()` with `BEGIN
//!   IMMEDIATE`, so a transaction that reads before it writes cannot fail at
//!   once with `SQLITE_BUSY_SNAPSHOT`, which SQLite never retries;
//! - a holder record (`<database>.writer`: pid, process start, command,
//!   since) written as soon as a connection holds the write lock (after
//!   `BEGIN IMMEDIATE`, or at the first changed row of an implicit write) and
//!   removed when the transaction commits or rolls back or the connection
//!   closes, so a timed-out waiter and `broker status` can name the writer;
//! - telemetry: `broker_db_busy_wait` (time slept waiting, one occurrence per
//!   retry), `broker_db_busy_retry` and `broker_db_busy_failure` counters, and
//!   the `broker_db_busy_wait_by_holder` attachment keyed by the holder's
//!   command name (never a path or an id).

use std::cell::{Cell, RefCell};
use std::os::raw::{c_int, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::{Connection, TransactionBehavior, ffi};

/// Longest a statement waits for another connection's lock before failing.
/// Long enough to absorb ordinary contention between many agents, short
/// enough that a wedged holder surfaces as a named error instead of a hang.
pub(crate) const BUSY_WAIT_BUDGET: Duration = Duration::from_secs(30);

/// First retry delay; doubles per retry up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_millis(2);
const MAX_BACKOFF: Duration = Duration::from_millis(250);

/// Suffix of the holder record beside the database (`broker.db.writer`).
pub(crate) const HOLDER_SUFFIX: &str = ".writer";

/// The longest command label kept in a holder record.
const MAX_COMMAND_LABEL: usize = 96;

thread_local! {
    /// When the current locking event started waiting.
    static WAIT_STARTED: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The holder's command name read when the current wait started.
    static WAIT_HOLDER: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The last wait on this thread that ran out of budget, until the error
    /// it caused is converted (or a new wait starts).
    static LAST_FAILURE: RefCell<Option<BusyFailure>> = const { RefCell::new(None) };
    static JITTER_STATE: Cell<u64> = const { Cell::new(0) };
    #[cfg(test)]
    static TEST_BUDGET: Cell<Option<Duration>> = const { Cell::new(None) };
}

/// The command this process runs, as its metric surface (`gc.__trash-catchup`).
static PROCESS_COMMAND: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// One busy-handler context per database path, kept for the process's life
/// so SQLite may hold a plain pointer to it.
static BUSY_TARGETS: OnceLock<Mutex<Vec<&'static BusyTarget>>> = OnceLock::new();

/// What a connection's busy handler knows about its database.
struct BusyTarget {
    db_path: PathBuf,
    holder_path: Option<PathBuf>,
}

/// SQLite constants used here (stable since 3.14 and 3.34).
const TRACE_PROFILE: c_uint = 0x02;
const TXN_WRITE: c_int = 2;

/// A failure older than this is not attached to an error: it belongs to an
/// earlier statement whose error was handled some other way.
const FAILURE_FRESHNESS: Duration = Duration::from_secs(1);

/// A wait that ran out of budget, as the error message reports it.
#[derive(Clone, Debug)]
pub(crate) struct BusyFailure {
    pub(crate) waited: Duration,
    pub(crate) holder: Option<WriterHolder>,
    at: Instant,
}

/// Who holds (or last took) the database write lock.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WriterHolder {
    pub(crate) pid: u32,
    /// The holder's process start token (`gates::process_start_time`), so a
    /// record left by a crashed process is not mistaken for a later process
    /// that reused its pid. `None` where the platform cannot say.
    #[serde(default)]
    pub(crate) start: Option<u64>,
    /// The holder's command surface (`gc.__trash-catchup`), or `unknown`.
    pub(crate) command: String,
    pub(crate) since_ms: i64,
}

impl WriterHolder {
    /// How the holder's command reads on a command line.
    pub(crate) fn command_line(&self) -> String {
        if self.command == "unknown" {
            return "unknown command".to_owned();
        }
        format!("aethyme broker {}", self.command.replace('.', " "))
    }

    pub(crate) fn held_for(&self, now_ms: i64) -> Duration {
        Duration::from_millis(u64::try_from(now_ms.saturating_sub(self.since_ms)).unwrap_or(0))
    }

    /// "pid 123 (`aethyme broker gc __trash-catchup`) for 42 s"
    pub(crate) fn describe(&self, now_ms: i64) -> String {
        format!(
            "pid {} (`{}`) for {} s",
            self.pid,
            self.command_line(),
            self.held_for(now_ms).as_secs()
        )
    }
}

/// Name the command this process runs, for holder records. `surface` is the
/// command's metric surface, which carries no paths, ids or task text.
pub(crate) fn set_process_command(surface: Option<String>) {
    let slot = PROCESS_COMMAND.get_or_init(|| Mutex::new(None));
    if let Ok(mut command) = slot.lock() {
        *command = surface.map(|label| sanitize_label(&label));
    }
}

fn process_command() -> String {
    PROCESS_COMMAND
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|command| command.clone()))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn sanitize_label(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(MAX_COMMAND_LABEL)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_owned()
    } else {
        cleaned
    }
}

/// Install the bounded busy handler for the database at `db_path`. Every
/// broker-database connection gets it, read-only ones included, and code
/// that temporarily disables waiting (`busy_timeout(ZERO)` for a try-lock)
/// restores it with this.
pub(crate) fn install_handler(conn: &Connection, db_path: &Path) -> rusqlite::Result<()> {
    let target: *const BusyTarget = busy_target(db_path);
    // SAFETY: `handle` is this open connection's database handle, used only
    // for this call. The context is a `'static` BusyTarget that is never
    // freed, so SQLite may keep the pointer for the connection's life.
    let rc = unsafe {
        ffi::sqlite3_busy_handler(conn.handle(), Some(busy_callback), target as *mut c_void)
    };
    if rc == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(ffi::Error::new(rc), None))
    }
}

fn busy_target(db_path: &Path) -> &'static BusyTarget {
    let targets = BUSY_TARGETS.get_or_init(|| Mutex::new(Vec::new()));
    let mut targets = match targets.lock() {
        Ok(targets) => targets,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(known) = targets.iter().find(|target| target.db_path == db_path) {
        return known;
    }
    let target: &'static BusyTarget = Box::leak(Box::new(BusyTarget {
        db_path: db_path.to_path_buf(),
        holder_path: holder_path(db_path),
    }));
    targets.push(target);
    target
}

unsafe extern "C" fn busy_callback(context: *mut c_void, count: c_int) -> c_int {
    // SAFETY: the context was registered by `install_handler` and points to a
    // leaked, never-freed BusyTarget.
    let target = unsafe { &*(context as *const BusyTarget) };
    let retry = std::panic::catch_unwind(|| busy_wait(count, target.holder_path.as_deref()))
        .unwrap_or(false);
    c_int::from(retry)
}

/// Configure a writable broker-database connection: the busy handler,
/// `BEGIN IMMEDIATE` for every `transaction()`, and the holder record.
pub(crate) fn configure_writer(conn: &mut Connection, db_path: &Path) -> rusqlite::Result<()> {
    install_handler(conn, db_path)?;
    conn.set_transaction_behavior(TransactionBehavior::Immediate);
    if let Some(holder_path) = holder_path(db_path) {
        install_holder_hooks(conn, holder_path);
    }
    Ok(())
}

/// Where the holder record of `db_path` lives, `None` for in-memory databases.
pub(crate) fn holder_path(db_path: &Path) -> Option<PathBuf> {
    let name = db_path.file_name()?.to_str()?;
    if name.is_empty() || name == ":memory:" {
        return None;
    }
    Some(db_path.with_file_name(format!("{name}{HOLDER_SUFFIX}")))
}

/// This process's start token, read once.
fn own_start() -> Option<u64> {
    static START: OnceLock<Option<u64>> = OnceLock::new();
    *START.get_or_init(|| {
        i32::try_from(std::process::id())
            .ok()
            .and_then(crate::gates::process_start_time)
    })
}

/// Per-connection holder state shared by the hooks and the statement trace.
/// Dropped when the connection closes (rusqlite drops its hooks first), which
/// unregisters the trace and removes a record a transaction abandoned
/// mid-way would otherwise leave behind.
struct HolderState {
    path: PathBuf,
    armed: AtomicBool,
    /// The connection's `sqlite3*`, for the trace; 0 when none is registered.
    db: usize,
}

impl HolderState {
    /// After each statement: a connection that now holds the write lock
    /// (`BEGIN IMMEDIATE` returned, or an implicit write is under way) records
    /// itself before it does anything else.
    fn after_statement(&self) {
        if self.db == 0 || self.armed.load(Ordering::Relaxed) {
            return;
        }
        // SAFETY: `db` is the open connection whose statement just finished;
        // the trace is unregistered before the connection closes.
        let state =
            unsafe { ffi::sqlite3_txn_state(self.db as *mut ffi::sqlite3, c"main".as_ptr()) };
        if state == TXN_WRITE {
            self.arm();
        }
    }

    fn arm(&self) {
        if self.armed.load(Ordering::Relaxed) || self.armed.swap(true, Ordering::AcqRel) {
            return;
        }
        let holder = WriterHolder {
            pid: std::process::id(),
            start: own_start(),
            command: process_command(),
            since_ms: crate::clock::epoch_ms(),
        };
        // Best effort: a failed record write must never fail the transaction.
        if write_holder(&self.path, &holder).is_err() {
            crate::phase_telemetry::add("broker_db_holder_record_failed", Duration::ZERO);
        }
    }

    fn disarm(&self) {
        if self.armed.swap(false, Ordering::AcqRel) {
            remove_record_file(&self.path);
        }
    }
}

impl Drop for HolderState {
    fn drop(&mut self) {
        if self.db != 0 {
            // SAFETY: rusqlite drops the hooks holding this state before it
            // closes the connection, so `db` is still open here; clearing the
            // trace keeps SQLite from calling into freed state.
            unsafe {
                ffi::sqlite3_trace_v2(self.db as *mut ffi::sqlite3, 0, None, std::ptr::null_mut());
            }
        }
        self.disarm();
    }
}

unsafe extern "C" fn trace_callback(
    mask: c_uint,
    context: *mut c_void,
    _statement: *mut c_void,
    _detail: *mut c_void,
) -> c_int {
    if mask & TRACE_PROFILE != 0 {
        // SAFETY: the context is the HolderState that registered this trace
        // and unregisters it in its Drop, so it is alive here.
        let state = unsafe { &*(context as *const HolderState) };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.after_statement()))
            .unwrap_or(());
    }
    0
}

fn install_holder_hooks(conn: &Connection, path: PathBuf) {
    // SAFETY: only the handle's address is kept; it is used while the
    // connection is open (see HolderState::drop).
    let db = unsafe { conn.handle() } as usize;
    let state = Arc::new(HolderState {
        path,
        armed: AtomicBool::new(false),
        db,
    });
    let context = Arc::as_ptr(&state) as *mut c_void;
    // SAFETY: `context` stays valid until HolderState::drop, which clears
    // this trace first.
    unsafe {
        ffi::sqlite3_trace_v2(
            db as *mut ffi::sqlite3,
            TRACE_PROFILE,
            Some(trace_callback),
            context,
        );
    }
    // A changed row means this connection holds the write lock until the
    // transaction ends. Only the main database takes that lock.
    let on_update = Arc::clone(&state);
    conn.update_hook(Some(
        move |_action: rusqlite::hooks::Action, database: &str, _table: &str, _row: i64| {
            if database == "main" {
                on_update.arm();
            }
        },
    ));
    let on_commit = Arc::clone(&state);
    conn.commit_hook(Some(move || {
        on_commit.disarm();
        false
    }));
    conn.rollback_hook(Some(move || state.disarm()));
}

/// Largest holder record read back; a real one is under 200 bytes.
const MAX_HOLDER_RECORD_BYTES: u64 = 4096;

/// Distinguishes this process's temporary record files.
static HOLDER_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// When this process last swept leftover temporary record files.
static LAST_TEMP_SWEEP_MS: AtomicI64 = AtomicI64::new(0);

/// A temporary record file older than this was left by a writer that died
/// between creating and renaming it.
const TEMP_LEFTOVER_AGE: Duration = Duration::from_secs(60);

/// Remove `<record>.*.tmp` files older than [`TEMP_LEFTOVER_AGE`], at most
/// once a minute per process.
fn sweep_temporary_records(path: &Path, name: &str) {
    let now_ms = crate::clock::epoch_ms();
    let last = LAST_TEMP_SWEEP_MS.load(Ordering::Relaxed);
    let interval = i64::try_from(TEMP_LEFTOVER_AGE.as_millis()).unwrap_or(i64::MAX);
    if now_ms.saturating_sub(last) < interval
        || LAST_TEMP_SWEEP_MS
            .compare_exchange(last, now_ms, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    remove_temporary_leftovers(path, name);
}

fn remove_temporary_leftovers(path: &Path, name: &str) {
    let Some(directory) = path.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let prefix = format!("{name}.");
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if !file_name.starts_with(&prefix) || !file_name.ends_with(".tmp") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= TEMP_LEFTOVER_AGE);
        if old && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            remove_record_file(&entry.path());
        }
    }
}

/// Write the record to a fresh private file beside it and rename that into
/// place. Nothing at the record's name is ever opened for writing, so a
/// symlink planted there is replaced, not followed.
fn write_holder(path: &Path, holder: &WriterHolder) -> std::io::Result<()> {
    use std::io::Write as _;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("holder record path has no name"))?;
    sweep_temporary_records(path, name);
    let sequence = HOLDER_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!("{name}.{}.{sequence}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let written = options.open(&temporary).and_then(|mut file| {
        file.write_all(serde_json::to_string(holder)?.as_bytes())?;
        std::fs::rename(&temporary, path)
    });
    if written.is_err() {
        remove_record_file(&temporary);
    }
    written
}

/// Remove a holder record (or its temporary file); an absent file is fine,
/// any other failure is counted, never fatal to the transaction.
fn remove_record_file(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::phase_telemetry::add("broker_db_holder_record_failed", Duration::ZERO);
    }
}

/// The recorded write-lock holder of `db_path`, if its record names a live
/// process. A record left by a process that died is ignored.
pub(crate) fn recorded_holder(db_path: &Path) -> Option<WriterHolder> {
    read_live_holder(&holder_path(db_path)?)
}

/// How long a write lock must be held before `broker status` reports it:
/// shorter holds are ordinary traffic, not something to act on.
const STATUS_REPORT_AFTER: Duration = Duration::from_secs(5);

/// A write lock held long enough for `broker status` to report.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct LongHeldWriteLock {
    pub(crate) pid: u32,
    pub(crate) command: String,
    pub(crate) held_ms: u64,
    pub(crate) message: String,
}

/// The recorded write-lock holder of `db_path` when it has held the lock for
/// at least [`STATUS_REPORT_AFTER`]. Reads one small file; takes no lock.
pub(crate) fn long_held_write_lock(db_path: &Path) -> Option<LongHeldWriteLock> {
    let holder = recorded_holder(db_path)?;
    let now_ms = crate::clock::epoch_ms();
    let held = holder.held_for(now_ms);
    (held >= STATUS_REPORT_AFTER).then(|| LongHeldWriteLock {
        pid: holder.pid,
        message: format!("Database write lock held by {}.", holder.describe(now_ms)),
        command: holder.command,
        held_ms: u64::try_from(held.as_millis()).unwrap_or(u64::MAX),
    })
}

fn read_live_holder(path: &Path) -> Option<WriterHolder> {
    let holder = parse_holder(&read_holder_bytes(path)?)?;
    holder_process_matches(&holder).then_some(holder)
}

/// The record's process is the one running now: alive, and, when the record
/// carries a start token, started at that same time (not a reused pid).
fn holder_process_matches(holder: &WriterHolder) -> bool {
    if !process_alive(holder.pid) {
        return false;
    }
    match holder.start {
        Some(recorded) => {
            i32::try_from(holder.pid)
                .ok()
                .and_then(crate::gates::process_start_time)
                == Some(recorded)
        }
        None => true,
    }
}

/// The record's bytes, only from a private regular file this user owns, not
/// through a symlink, and at most [`MAX_HOLDER_RECORD_BYTES`].
fn read_holder_bytes(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: geteuid takes no arguments and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if metadata.uid() != euid || metadata.mode() & 0o022 != 0 || metadata.nlink() != 1 {
            return None;
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_HOLDER_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_HOLDER_RECORD_BYTES).then_some(bytes)
}

/// A record is accepted only when every field is one this module writes:
/// its command must already be a clean label, so nothing read from the file
/// (escape sequences, control characters, paths) reaches an error or status.
fn parse_holder(bytes: &[u8]) -> Option<WriterHolder> {
    let holder: WriterHolder = serde_json::from_slice(bytes).ok()?;
    let valid = holder.pid > 0
        && holder.since_ms > 0
        && !holder.command.is_empty()
        && holder.command.len() <= MAX_COMMAND_LABEL
        && sanitize_label(&holder.command) == holder.command;
    valid.then_some(holder)
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    crate::blockers::process_alive(i64::from(pid))
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

fn budget() -> Duration {
    #[cfg(test)]
    if let Some(budget) = TEST_BUDGET.with(Cell::get) {
        return budget;
    }
    BUSY_WAIT_BUDGET
}

/// Run `f` with a shorter wait budget on this thread (tests only).
#[cfg(test)]
pub(crate) fn with_test_budget<T>(budget: Duration, f: impl FnOnce() -> T) -> T {
    let previous = TEST_BUDGET.with(|slot| slot.replace(Some(budget)));
    let result = f();
    TEST_BUDGET.with(|slot| slot.set(previous));
    result
}

/// A pseudo-random factor in [0.5, 1.5), so waiters that collided once do
/// not retry in lockstep.
fn jitter(delay: Duration) -> Duration {
    let next = JITTER_STATE.with(|state| {
        let mut x = state.get();
        if x == 0 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(0);
            x = nanos ^ (u64::from(std::process::id()) << 32) ^ 0x9E37_79B9_7F4A_7C15;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        state.set(x);
        x
    });
    let factor_permille = 500 + next % 1000;
    delay * u32::try_from(factor_permille).unwrap_or(1000) / 1000
}

fn backoff(count: i32) -> Duration {
    let shift = u32::try_from(count.clamp(0, 16)).unwrap_or(0);
    let base = FIRST_BACKOFF
        .checked_mul(1u32 << shift)
        .unwrap_or(MAX_BACKOFF)
        .min(MAX_BACKOFF);
    jitter(base).max(Duration::from_millis(1))
}

/// SQLite calls this while another connection holds a lock this statement
/// needs; `count` is how often it already did for the same locking event.
/// Returning `true` retries, `false` fails the statement with SQLITE_BUSY.
/// `holder_path` is the record of this connection's own database.
fn busy_wait(count: i32, holder_path: Option<&Path>) -> bool {
    let now = Instant::now();
    let holder_of_this_database = || holder_path.and_then(read_live_holder);
    let started = WAIT_STARTED.with(|slot| match slot.get() {
        Some(started) if count > 0 => started,
        _ => {
            slot.set(Some(now));
            now
        }
    });
    if count == 0 {
        // A new wait: an earlier failure no error claimed is stale now.
        LAST_FAILURE.with(|slot| slot.borrow_mut().take());
        let holder = holder_of_this_database().map(|holder| holder.command);
        WAIT_HOLDER.with(|slot| *slot.borrow_mut() = holder);
    }
    let waited = now.saturating_duration_since(started);
    let budget = budget();
    if waited >= budget {
        WAIT_STARTED.with(|slot| slot.set(None));
        let holder = holder_of_this_database();
        crate::phase_telemetry::add("broker_db_busy_failure", Duration::ZERO);
        LAST_FAILURE.with(|slot| {
            *slot.borrow_mut() = Some(BusyFailure {
                waited,
                holder,
                at: Instant::now(),
            });
        });
        return false;
    }
    let delay = backoff(count).min(budget.saturating_sub(waited));
    std::thread::sleep(delay);
    crate::phase_telemetry::add("broker_db_busy_wait", delay);
    crate::phase_telemetry::add("broker_db_busy_retry", Duration::ZERO);
    let holder = WAIT_HOLDER.with(|slot| slot.borrow().clone());
    crate::phase_telemetry::add_keyed(
        "broker_db_busy_wait_by_holder",
        holder.as_deref().unwrap_or("unrecorded"),
        delay,
    );
    true
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// Attach the wait that just failed to a busy error, when the error is
/// created: how long this thread waited and who held the write lock of that
/// connection's database. The failure is consumed, so a later busy error on
/// this thread never reports it again. Other errors pass through unchanged.
pub(crate) fn with_busy_context(error: rusqlite::Error) -> rusqlite::Error {
    if !is_busy(&error) {
        return error;
    }
    let Some(failure) = LAST_FAILURE.with(|slot| slot.borrow_mut().take()) else {
        return error;
    };
    if failure.at.elapsed() > FAILURE_FRESHNESS {
        return error;
    }
    match error {
        rusqlite::Error::SqliteFailure(code, message) => {
            let base = message.unwrap_or_else(|| code.to_string());
            rusqlite::Error::SqliteFailure(
                code,
                Some(format!("{base}{}", describe_failure(&failure))),
            )
        }
        other => other,
    }
}

fn describe_failure(failure: &BusyFailure) -> String {
    let mut context = format!(" after waiting {} s", failure.waited.as_secs());
    match &failure.holder {
        Some(holder) => {
            context.push_str("; database write lock held by ");
            context.push_str(&holder.describe(crate::clock::epoch_ms()));
        }
        None => context.push_str(
            "; the write-lock holder was not recorded (an older aethyme, or a connection \
             outside the broker)",
        ),
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(dir: &tempfile::TempDir) -> PathBuf {
        let path = dir.path().join("broker.db");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "wal").unwrap();
        conn.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();
        path
    }

    fn writer(path: &Path) -> Connection {
        let mut conn = Connection::open(path).unwrap();
        configure_writer(&mut conn, path).unwrap();
        conn
    }

    #[test]
    fn a_waiter_outlasts_a_short_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            holder.execute_batch("COMMIT").unwrap();
        });
        let conn = writer(&path);
        conn.execute("INSERT INTO t VALUES (1)", []).unwrap();
        release.join().unwrap();
    }

    #[test]
    fn a_timed_out_wait_names_the_recorded_holder() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        set_process_command(Some("gc.__trash-catchup".into()));
        let holder = writer(&path);
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        holder.execute("INSERT INTO t VALUES (1)", []).unwrap();
        let record = recorded_holder(&path).expect("holder recorded once it holds the lock");
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.command, "gc.__trash-catchup");

        let waiter = writer(&path);
        let error = with_test_budget(Duration::from_millis(150), || {
            waiter.execute("INSERT INTO t VALUES (2)", []).unwrap_err()
        });
        let message = crate::BrokerError::from(error).to_string();
        assert!(message.contains("database is locked"), "{message}");
        assert!(
            message.contains(&format!(
                "database write lock held by pid {} (`aethyme broker gc __trash-catchup`)",
                std::process::id()
            )),
            "{message}"
        );
        assert!(message.contains("after waiting 0 s"), "{message}");

        holder.execute_batch("COMMIT").unwrap();
        assert!(
            !holder_path(&path).unwrap().exists(),
            "commit removes the holder record"
        );
    }

    #[test]
    fn rollback_and_close_remove_the_holder_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        let record = holder_path(&path).unwrap();
        let conn = writer(&path);
        conn.execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES (1);")
            .unwrap();
        assert!(record.exists());
        conn.execute_batch("ROLLBACK").unwrap();
        assert!(!record.exists());

        conn.execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES (1);")
            .unwrap();
        assert!(record.exists());
        drop(conn);
        assert!(
            !record.exists(),
            "closing mid-transaction removes the record"
        );
    }

    #[test]
    fn writer_transactions_begin_immediate_so_read_then_write_waits() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        let mut conn = writer(&path);
        let other = writer(&path);
        // A deferred transaction that read before another connection
        // committed would fail its first write with SQLITE_BUSY_SNAPSHOT,
        // which no busy handler retries.
        let tx = conn.transaction().unwrap();
        let _: i64 = tx
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .unwrap();
        let blocked = with_test_budget(Duration::from_millis(100), || {
            other.execute("INSERT INTO t VALUES (9)", []).unwrap_err()
        });
        assert!(
            is_busy(&blocked),
            "the immediate transaction holds the lock"
        );
        tx.execute("INSERT INTO t VALUES (1)", []).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn a_dead_holder_record_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        let stale = WriterHolder {
            pid: i32::MAX as u32,
            start: None,
            command: "gc".into(),
            since_ms: 1,
        };
        write_holder(&holder_path(&path).unwrap(), &stale).unwrap();
        assert_eq!(recorded_holder(&path), None);
    }

    #[test]
    fn status_reports_only_a_long_held_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        let record = holder_path(&path).unwrap();
        let mut holder = WriterHolder {
            pid: std::process::id(),
            start: own_start(),
            command: "gc.__continue-artifacts".into(),
            since_ms: crate::clock::epoch_ms(),
        };
        write_holder(&record, &holder).unwrap();
        assert!(
            long_held_write_lock(&path).is_none(),
            "a fresh hold is traffic"
        );
        holder.since_ms -= 42_000;
        write_holder(&record, &holder).unwrap();
        let lock = long_held_write_lock(&path).unwrap();
        assert_eq!(lock.command, "gc.__continue-artifacts");
        assert!(lock.held_ms >= 42_000);
        assert!(
            lock.message
                .contains("(`aethyme broker gc __continue-artifacts`) for 42 s"),
            "{}",
            lock.message
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_holder_path_is_replaced_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        let record = holder_path(&path).unwrap();
        let target = dir.path().join("victim");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, &record).unwrap();
        assert_eq!(
            recorded_holder(&path),
            None,
            "a symlinked record is not read"
        );

        let conn = writer(&path);
        conn.execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES (1);")
            .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        let metadata = std::fs::symlink_metadata(&record).unwrap();
        assert!(metadata.file_type().is_file(), "the link was replaced");
        assert_eq!(
            recorded_holder(&path).map(|holder| holder.pid),
            Some(std::process::id())
        );
        conn.execute_batch("COMMIT").unwrap();
        assert!(!record.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    }

    #[test]
    fn a_record_with_escape_sequences_or_control_characters_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        let record = holder_path(&path).unwrap();
        // JSON escapes: ESC sequences and a newline decode into the field.
        for command in [
            r"\u001b[31mgc\u001b[0m",
            r"gc\nforged line",
            "../../etc",
            "",
        ] {
            let json = format!(
                "{{\"pid\":{},\"command\":\"{command}\",\"since_ms\":1}}",
                std::process::id()
            );
            let _ = std::fs::remove_file(&record);
            std::fs::write(&record, json).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            assert_eq!(recorded_holder(&path), None, "{command:?}");
        }
        let oversized = format!(
            "{{\"pid\":{},\"command\":\"gc\",\"since_ms\":1,\"pad\":\"{}\"}}",
            std::process::id(),
            "x".repeat(8192)
        );
        std::fs::write(&record, oversized).unwrap();
        assert_eq!(
            recorded_holder(&path),
            None,
            "an oversized record is rejected"
        );
    }

    #[test]
    fn a_failed_wait_is_reported_once_and_only_while_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let waiter = writer(&path);
        let quick = Connection::open(&path).unwrap();
        quick.busy_timeout(Duration::ZERO).unwrap();
        let wait = || {
            with_test_budget(Duration::from_millis(80), || {
                waiter.execute("INSERT INTO t VALUES (1)", []).unwrap_err()
            })
        };
        let quick_message = || {
            crate::BrokerError::from(quick.execute("INSERT INTO t VALUES (2)", []).unwrap_err())
                .to_string()
        };

        let first = crate::BrokerError::from(wait()).to_string();
        assert!(first.contains("after waiting 0 s"), "{first}");
        let later = quick_message();
        assert!(later.contains("database is locked"), "{later}");
        assert!(!later.contains("after waiting"), "consumed once: {later}");

        // A failure no error claimed goes stale instead of being reported
        // by an unrelated later error.
        drop(wait());
        std::thread::sleep(FAILURE_FRESHNESS + Duration::from_millis(100));
        let stale = quick_message();
        assert!(!stale.contains("after waiting"), "{stale}");
        holder.execute_batch("COMMIT").unwrap();
    }

    #[test]
    fn a_wait_names_only_its_own_databases_holder() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let recorded = database(&first);
        let unrecorded = database(&second);
        let recorded_writer = writer(&recorded);
        recorded_writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(recorded_holder(&recorded).is_some());
        let plain = Connection::open(&unrecorded).unwrap();
        plain.execute_batch("BEGIN IMMEDIATE").unwrap();

        let waiter = writer(&unrecorded);
        let error = with_test_budget(Duration::from_millis(80), || {
            waiter.execute("INSERT INTO t VALUES (1)", []).unwrap_err()
        });
        let message = crate::BrokerError::from(error).to_string();
        assert!(message.contains("was not recorded"), "{message}");
        assert!(!message.contains("held by pid"), "{message}");
        plain.execute_batch("COMMIT").unwrap();
        recorded_writer.execute_batch("COMMIT").unwrap();
    }

    #[test]
    fn a_record_whose_process_start_differs_is_a_reused_pid() {
        let Some(start) = own_start() else {
            return; // the platform cannot tell process start times apart
        };
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        let record = holder_path(&path).unwrap();
        let mut holder = WriterHolder {
            pid: std::process::id(),
            start: Some(start + 1),
            command: "gc".into(),
            since_ms: crate::clock::epoch_ms(),
        };
        write_holder(&record, &holder).unwrap();
        assert_eq!(recorded_holder(&path), None);
        holder.start = Some(start);
        write_holder(&record, &holder).unwrap();
        assert_eq!(recorded_holder(&path), Some(holder));
    }

    #[test]
    fn the_record_is_written_when_the_write_lock_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        let record = holder_path(&path).unwrap();
        let mut conn = writer(&path);

        // A deferred read takes no write lock and records nothing.
        conn.execute_batch("BEGIN DEFERRED").unwrap();
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert!(!record.exists());
        conn.execute_batch("COMMIT").unwrap();

        // An immediate transaction that only reads (or computes) before it
        // writes is recorded from BEGIN on.
        let tx = conn.transaction().unwrap();
        assert!(record.exists(), "recorded at BEGIN IMMEDIATE");
        let _: i64 = tx
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            recorded_holder(&path).map(|holder| holder.pid),
            Some(std::process::id())
        );
        tx.commit().unwrap();
        assert!(!record.exists());
    }

    #[test]
    fn old_temporary_records_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(&dir);
        let record = holder_path(&path).unwrap();
        let old = dir.path().join("broker.db.writer.1.0.tmp");
        let fresh = dir.path().join("broker.db.writer.2.0.tmp");
        let unrelated = dir.path().join("other.1.0.tmp");
        for file in [&old, &fresh, &unrelated] {
            std::fs::write(file, b"{}").unwrap();
        }
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(120);
        for file in [&old, &unrelated] {
            std::fs::File::options()
                .write(true)
                .open(file)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }
        remove_temporary_leftovers(&record, "broker.db.writer");
        assert!(!old.exists(), "a leftover older than a minute is removed");
        assert!(fresh.exists(), "a writer may still be renaming a fresh one");
        assert!(unrelated.exists(), "only this record's leftovers");
    }

    #[test]
    fn labels_keep_only_command_characters() {
        assert_eq!(sanitize_label("gc.__trash-catchup"), "gc.__trash-catchup");
        assert_eq!(sanitize_label("../x y/z"), "..xyz");
        assert_eq!(sanitize_label(""), "unknown");
    }

    #[test]
    fn backoff_is_bounded_and_grows() {
        assert!(backoff(0) <= Duration::from_millis(3));
        assert!(backoff(30) <= MAX_BACKOFF * 3 / 2);
        assert!(backoff(30) >= MAX_BACKOFF / 2);
    }
}
