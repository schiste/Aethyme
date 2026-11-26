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
//! - a holder record (`<database>.writer`: pid, command, since) written when
//!   a transaction first changes a row and removed when it commits or rolls
//!   back, so a timed-out waiter and `broker status` can name the writer;
//! - telemetry: `broker_db_busy_wait` (time slept waiting, one occurrence per
//!   retry), `broker_db_busy_retry` and `broker_db_busy_failure` counters, and
//!   the `broker_db_busy_wait_by_holder` attachment keyed by the holder's
//!   command name (never a path or an id).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::{Connection, TransactionBehavior};

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
    /// The last wait on this thread that ran out of budget.
    static LAST_FAILURE: RefCell<Option<BusyFailure>> = const { RefCell::new(None) };
    static JITTER_STATE: Cell<u64> = const { Cell::new(0) };
    #[cfg(test)]
    static TEST_BUDGET: Cell<Option<Duration>> = const { Cell::new(None) };
}

/// The command this process runs, as its metric surface (`gc.__trash-catchup`).
static PROCESS_COMMAND: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// Holder-record paths of every database this process configured, so a
/// timed-out wait (whose handler cannot know its connection) can find them.
static HOLDER_PATHS: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();

/// A wait that ran out of budget, as the error message reports it.
#[derive(Clone, Debug)]
pub(crate) struct BusyFailure {
    pub(crate) waited: Duration,
    pub(crate) holder: Option<WriterHolder>,
}

/// Who holds (or last took) the database write lock.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WriterHolder {
    pub(crate) pid: u32,
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

/// Install the bounded busy handler. Every broker-database connection gets
/// it, read-only ones included, and code that temporarily disables waiting
/// (`busy_timeout(ZERO)` for a try-lock) restores it with this.
pub(crate) fn install_handler(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_handler(Some(busy_handler))
}

/// Configure a writable broker-database connection: the busy handler,
/// `BEGIN IMMEDIATE` for every `transaction()`, and the holder record.
pub(crate) fn configure_writer(conn: &mut Connection, db_path: &Path) -> rusqlite::Result<()> {
    install_handler(conn)?;
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

fn register_holder_path(path: &Path) {
    let paths = HOLDER_PATHS.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut paths) = paths.lock()
        && !paths.iter().any(|known| known == path)
    {
        paths.push(path.to_path_buf());
    }
}

/// Per-connection holder state shared by the three hooks. Dropped when the
/// connection closes (rusqlite drops its hooks then), which removes a record
/// a transaction abandoned mid-way would otherwise leave behind.
struct HolderState {
    path: PathBuf,
    armed: AtomicBool,
}

impl HolderState {
    fn arm(&self) {
        if self.armed.load(Ordering::Relaxed) || self.armed.swap(true, Ordering::AcqRel) {
            return;
        }
        let holder = WriterHolder {
            pid: std::process::id(),
            command: process_command(),
            since_ms: crate::clock::epoch_ms(),
        };
        // Best effort: a failed record write must never fail the transaction.
        let _ = write_holder(&self.path, &holder);
    }

    fn disarm(&self) {
        if self.armed.swap(false, Ordering::AcqRel) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Drop for HolderState {
    fn drop(&mut self) {
        self.disarm();
    }
}

fn install_holder_hooks(conn: &Connection, path: PathBuf) {
    register_holder_path(&path);
    let state = Arc::new(HolderState {
        path,
        armed: AtomicBool::new(false),
    });
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

fn write_holder(path: &Path, holder: &WriterHolder) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(serde_json::to_string(holder)?.as_bytes())
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
    let bytes = std::fs::read(path).ok()?;
    let holder: WriterHolder = serde_json::from_slice(&bytes).ok()?;
    process_alive(holder.pid).then_some(holder)
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    crate::blockers::process_alive(i64::from(pid))
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// The live holder of any database this process configured, oldest first.
fn current_holder() -> Option<WriterHolder> {
    let paths = HOLDER_PATHS
        .get()
        .and_then(|paths| paths.lock().ok().map(|paths| paths.clone()))
        .unwrap_or_default();
    paths
        .iter()
        .filter_map(|path| read_live_holder(path))
        .min_by_key(|holder| holder.since_ms)
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
fn busy_handler(count: i32) -> bool {
    let now = Instant::now();
    let started = WAIT_STARTED.with(|slot| match slot.get() {
        Some(started) if count > 0 => started,
        _ => {
            slot.set(Some(now));
            now
        }
    });
    if count == 0 {
        let holder = current_holder().map(|holder| holder.command);
        WAIT_HOLDER.with(|slot| *slot.borrow_mut() = holder);
    }
    let waited = now.saturating_duration_since(started);
    let budget = budget();
    if waited >= budget {
        WAIT_STARTED.with(|slot| slot.set(None));
        let holder = current_holder();
        crate::phase_telemetry::add("broker_db_busy_failure", Duration::ZERO);
        LAST_FAILURE.with(|slot| *slot.borrow_mut() = Some(BusyFailure { waited, holder }));
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

/// Extra context for a broker-database error: for a busy error, how long
/// this thread waited and who holds the write lock; empty otherwise.
pub(crate) fn busy_context(error: &rusqlite::Error) -> String {
    if !is_busy(error) {
        return String::new();
    }
    let now_ms = crate::clock::epoch_ms();
    let failure = LAST_FAILURE.with(|slot| slot.borrow().clone());
    let (waited, holder) = match failure {
        Some(failure) => (Some(failure.waited), failure.holder.or_else(current_holder)),
        None => (None, current_holder()),
    };
    let mut context = String::new();
    if let Some(waited) = waited {
        context.push_str(&format!(" after waiting {} s", waited.as_secs()));
    }
    match holder {
        Some(holder) => {
            context.push_str("; database write lock held by ");
            context.push_str(&holder.describe(now_ms));
        }
        None => context.push_str(
            "; the write-lock holder was not recorded (it changed no row yet, or runs an older aethyme)",
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
        let record = recorded_holder(&path).expect("holder recorded at the first changed row");
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.command, "gc.__trash-catchup");

        let waiter = writer(&path);
        let error = with_test_budget(Duration::from_millis(150), || {
            waiter.execute("INSERT INTO t VALUES (2)", []).unwrap_err()
        });
        let message = crate::BrokerError::from(error).to_string();
        assert!(message.contains("database is locked"), "{message}");
        // Other tests in this process may hold records too; the oldest live
        // one is named, and every one of them is this process.
        assert!(
            message.contains(&format!(
                "database write lock held by pid {}",
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
