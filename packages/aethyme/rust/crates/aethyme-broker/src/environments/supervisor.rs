//! The host supervisor: `aethyme env supervise` (#1222).
//!
//! Environments outlive the command that starts them. Something must keep
//! running to parent dev servers, renew leases, park idle environments and
//! answer wake requests, and the broker has no daemon. This is that process:
//! one per host user, started on demand by commands that need it, and gone
//! once nothing does.
//!
//! - **Singleton.** `run` holds an exclusive OS lock (`supervisor.lock`) for its
//!   whole life; a second instance exits immediately. Concurrent `ensure`
//!   callers serialise on a separate `spawn.lock` so they start one process
//!   between them, never several.
//! - **Observed, not probed.** `status` reads `supervisor.json` and checks the
//!   recorded process identity. It never try-locks `supervisor.lock`: a probe
//!   that briefly holds the lock would make a supervisor starting at that
//!   moment believe another one runs, and exit.
//! - **Lingers, then exits.** After `linger` with no environment that needs
//!   supervision, it exits on its own.
//! - **Degraded mode.** Without a supervisor, foreground commands still work;
//!   only background behaviour (idle parking, wake on request) stops, and
//!   `env status` says `supervisor: absent`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::identity::{Observation, ProcessIdentity};
use super::logs::open_for_append;
use crate::file_lock::{ExclusiveFileLock, open_lock_file};

pub const SUPERVISOR_LOCK: &str = "supervisor.lock";
pub const SPAWN_LOCK: &str = "spawn.lock";
pub const SUPERVISOR_STATE: &str = "supervisor.json";
pub const SUPERVISOR_LOG: &str = "supervisor.log";
pub const SUPERVISOR_EXIT: &str = "supervisor-exit.json";

pub const DEFAULT_LINGER: Duration = Duration::from_secs(300);
pub const DEFAULT_TICK: Duration = Duration::from_secs(15);
const SPAWN_WAIT: Duration = Duration::from_secs(5);

/// Test and operator overrides, read only by the supervisor process itself.
pub const LINGER_ENV: &str = "AETHYME_ENV_SUPERVISOR_LINGER_SECONDS";
pub const TICK_ENV: &str = "AETHYME_ENV_SUPERVISOR_TICK_MS";

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// `<host state>/environments`.
    pub root: PathBuf,
    pub linger: Duration,
    pub tick: Duration,
}

impl SupervisorConfig {
    pub fn new(root: PathBuf) -> Self {
        let read = |name: &str| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
        };
        Self {
            root,
            linger: read(LINGER_ENV)
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_LINGER),
            tick: read(TICK_ENV)
                .map(Duration::from_millis)
                .unwrap_or(DEFAULT_TICK)
                .max(Duration::from_millis(10)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorRecord {
    pub identity: ProcessIdentity,
    pub started_at_ms: i64,
    pub version: String,
}

/// Written by a supervisor as it exits (#1521), so `env status` can say why
/// none is running: an idle exit is expected, a crash is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitRecord {
    pub pid: i32,
    pub version: String,
    pub started_at_ms: i64,
    /// `None` when the supervisor left no exit record: it was killed, it
    /// crashed hard, or the host restarted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exited_at_ms: Option<i64>,
    pub reason: String,
}

/// The reason recorded for a supervisor whose process is gone but that
/// never wrote an exit record.
pub const UNRECORDED_EXIT: &str =
    "exited without recording a reason (killed, crashed, or the host restarted)";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupervisorStatus {
    /// `running`, `absent`, or `unproven` (a record exists but its process
    /// cannot be shown to be the supervisor that wrote it).
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// When absent: how the last supervisor ended, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exit: Option<ExitRecord>,
    /// When running: this binary's version, if the supervisor runs another.
    /// It keeps running the old code until it exits on its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
}

impl SupervisorStatus {
    pub fn running(&self) -> bool {
        self.state == "running"
    }

    /// One line for `env status`.
    pub fn describe(&self) -> String {
        let mut line = match self.pid {
            Some(pid) => format!("supervisor: {} (pid {pid}", self.state),
            None => format!("supervisor: {} (", self.state),
        };
        match (&self.pid, &self.last_exit) {
            (Some(_), _) => {
                if let Some(version) = &self.version {
                    line.push_str(&format!(", v{version}"));
                }
                line.push(')');
            }
            (None, Some(exit)) => {
                let at = exit
                    .exited_at_ms
                    .map(|ms| format!(" at {}", crate::cli::insights::timestamp_millis(ms)))
                    .unwrap_or_default();
                line.push_str(&format!(
                    "last exit{at}: {}, v{}; idle parking and wake on request are not running)",
                    exit.reason, exit.version
                ));
            }
            (None, None) => line.push_str("idle parking and wake on request are not running)"),
        }
        if let (Some(installed), Some(version)) = (&self.installed_version, &self.version) {
            line.push_str(&format!(
                "\nwarning: the supervisor runs v{version} but this binary is v{installed}; it keeps the old code until it exits (after {}s idle) or is stopped",
                DEFAULT_LINGER.as_secs()
            ));
        }
        line.push('\n');
        line
    }
}

fn read_exit(root: &Path) -> Option<ExitRecord> {
    serde_json::from_str(&fs::read_to_string(root.join(SUPERVISOR_EXIT)).ok()?).ok()
}

pub fn status(root: &Path) -> SupervisorStatus {
    let absent = |last_exit: Option<ExitRecord>| SupervisorStatus {
        state: "absent",
        pid: None,
        started_at_ms: None,
        version: None,
        last_exit,
        installed_version: None,
    };
    let record = fs::read_to_string(root.join(SUPERVISOR_STATE))
        .ok()
        .and_then(|text| serde_json::from_str::<SupervisorRecord>(&text).ok());
    let Some(record) = record else {
        return absent(read_exit(root));
    };
    let state = match record.identity.observe() {
        Observation::Alive => "running",
        Observation::Gone => {
            // A record whose process is gone: its own exit record says why,
            // and without one the supervisor never got to write it.
            let exit = read_exit(root)
                .filter(|exit| {
                    exit.pid == record.identity.pid && exit.started_at_ms == record.started_at_ms
                })
                .unwrap_or_else(|| ExitRecord {
                    pid: record.identity.pid,
                    version: record.version.clone(),
                    started_at_ms: record.started_at_ms,
                    exited_at_ms: None,
                    reason: UNRECORDED_EXIT.into(),
                });
            return absent(Some(exit));
        }
        Observation::Unproven => "unproven",
    };
    let installed = env!("CARGO_PKG_VERSION");
    SupervisorStatus {
        state,
        pid: Some(record.identity.pid),
        started_at_ms: Some(record.started_at_ms),
        installed_version: (record.version != installed).then(|| installed.to_string()),
        version: Some(record.version),
        last_exit: None,
    }
}

/// Start a supervisor unless one is running, and wait until it has
/// published its record. Concurrent callers start exactly one.
pub fn ensure_running(root: &Path, executable: &Path) -> io::Result<SupervisorStatus> {
    let current = status(root);
    if current.running() {
        return Ok(current);
    }
    fs::create_dir_all(root)?;
    crate::host_state::protect_host_state_path(root, true)?;
    let spawn_lock = ExclusiveFileLock::acquire(open_lock_file(&root.join(SPAWN_LOCK))?)?;
    let current = status(root);
    if current.running() {
        return Ok(current);
    }
    let log = open_for_append(&root.join(SUPERVISOR_LOG))?;
    let mut command = Command::new(executable);
    command
        .args(["env", "supervise"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    detach(&mut command);
    let mut child = command.spawn()?;
    let child_pid = child.id() as i32;
    let deadline = Instant::now() + SPAWN_WAIT;
    loop {
        let observed = status(root);
        if observed.running() && observed.pid == Some(child_pid) {
            drop(spawn_lock);
            // Reap the child if it ever exits so it never lingers as a zombie
            // of this short-lived command; the supervisor is in its own group.
            std::thread::spawn(move || {
                crate::warn_unrecorded("reap environment supervisor process", child.wait());
            });
            return Ok(observed);
        }
        if let Ok(Some(exit)) = child.try_wait() {
            // It found another live supervisor (and exited), or failed.
            let observed = status(root);
            if observed.running() {
                return Ok(observed);
            }
            return Err(io::Error::other(format!(
                "supervisor exited during start ({exit}); see {}",
                root.join(SUPERVISOR_LOG).display()
            )));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "supervisor did not publish its record within {}s; see {}",
                    SPAWN_WAIT.as_secs(),
                    root.join(SUPERVISOR_LOG).display()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // Its own process group: a terminal's ^C or a caller's group kill does
    // not take the supervisor (and the environments it parents) with it.
    command.process_group(0);
}

#[cfg(not(unix))]
fn detach(_: &mut Command) {}

/// One unit of supervisor work. Lifecycle issues add work here; the
/// supervisor itself only owns the lock, the record, the cadence and linger.
pub trait SupervisorWork {
    /// Do one tick of work and say whether supervision is still needed.
    fn tick(&mut self) -> TickOutcome;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickOutcome {
    pub needs_supervisor: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    /// Another supervisor holds the lock.
    AlreadyRunning,
    /// Nothing needed supervision for the linger period.
    Linger,
    /// The caller's stop condition (tests) fired.
    Stopped,
    /// SIGTERM, SIGINT or SIGHUP, once [`install_signal_handlers`] ran.
    Signal,
}

/// The last terminating signal received, 0 for none.
static SIGNALLED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn record_signal(signal: libc::c_int) {
    // Only a lock-free atomic store: async-signal-safe.
    SIGNALLED.store(signal, std::sync::atomic::Ordering::Relaxed);
}

/// Make SIGTERM, SIGINT and SIGHUP end the supervisor loop cleanly, so its
/// exit is recorded with the signal as the reason. Only the `env supervise`
/// process calls this; tests run the loop in-process without it.
#[cfg(unix)]
pub fn install_signal_handlers() -> io::Result<()> {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: a zeroed sigaction is valid; its mask is emptied below and
        // the handler only stores to a static atomic.
        let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        // SAFETY: `sa_mask` is live storage that sigemptyset initialises.
        if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0 {
            return Err(io::Error::last_os_error());
        }
        action.sa_sigaction = record_signal as *const () as libc::sighandler_t;
        // SAFETY: `action` is a live, initialised sigaction for the call.
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn install_signal_handlers() -> io::Result<()> {
    Ok(())
}

fn signal_name(signal: i32) -> String {
    #[cfg(unix)]
    {
        match signal {
            libc::SIGTERM => return "SIGTERM".into(),
            libc::SIGINT => return "SIGINT".into(),
            libc::SIGHUP => return "SIGHUP".into(),
            _ => {}
        }
    }
    format!("signal {signal}")
}

/// Why the supervisor exited, as written to its log and exit record.
fn describe_exit(reason: ExitReason, config: &SupervisorConfig) -> String {
    match reason {
        ExitReason::AlreadyRunning => "another supervisor is running".into(),
        ExitReason::Linger => format!(
            "idle linger elapsed ({}s with nothing to supervise)",
            config.linger.as_secs()
        ),
        ExitReason::Stopped => "stopped".into(),
        ExitReason::Signal => signal_name(SIGNALLED.load(std::sync::atomic::Ordering::Relaxed)),
    }
}

/// Run the supervisor loop in this process until it lingers out.
///
/// Its start and exit are written to stderr (the supervisor log) and the exit
/// to [`SUPERVISOR_EXIT`], including a panic in the work (#1521).
pub fn run(
    config: &SupervisorConfig,
    work: &mut dyn SupervisorWork,
    stop: &dyn Fn() -> bool,
) -> io::Result<ExitReason> {
    fs::create_dir_all(&config.root)?;
    crate::host_state::protect_host_state_path(&config.root, true)?;
    let Some(lock) =
        ExclusiveFileLock::try_acquire(open_lock_file(&config.root.join(SUPERVISOR_LOCK))?)?
    else {
        return Ok(ExitReason::AlreadyRunning);
    };
    let pid = std::process::id() as i32;
    let identity = ProcessIdentity::capture(pid)
        .ok_or_else(|| io::Error::other("cannot read this process's own identity"))?;
    let record = SupervisorRecord {
        identity,
        started_at_ms: now_ms(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    publish(&config.root, &record)?;
    eprintln!(
        "aethyme env supervisor started: version {}, pid {pid}, linger {}s, tick {}ms",
        record.version,
        config.linger.as_secs(),
        config.tick.as_millis()
    );
    let looped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        supervise(config, work, stop)
    }));
    let reason = match &looped {
        Ok(reason) => describe_exit(*reason, config),
        Err(payload) => format!("panic: {}", panic_message(payload.as_ref())),
    };
    eprintln!("aethyme env supervisor exiting: {reason}");
    crate::warn_unrecorded(
        "record the supervisor's exit",
        record_exit(&config.root, &record, &reason),
    );
    withdraw(&config.root, pid);
    drop(lock);
    match looped {
        Ok(reason) => Ok(reason),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

fn supervise(
    config: &SupervisorConfig,
    work: &mut dyn SupervisorWork,
    stop: &dyn Fn() -> bool,
) -> ExitReason {
    let signalled = || SIGNALLED.load(std::sync::atomic::Ordering::Relaxed) != 0;
    let mut idle_since: Option<Instant> = None;
    loop {
        if stop() {
            return ExitReason::Stopped;
        }
        if signalled() {
            return ExitReason::Signal;
        }
        let outcome = work.tick();
        if outcome.needs_supervisor {
            idle_since = None;
        } else {
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= config.linger {
                return ExitReason::Linger;
            }
        }
        // Sleep in short slices so a signal ends the loop promptly.
        let woke = Instant::now() + config.tick;
        while Instant::now() < woke && !signalled() {
            std::thread::sleep(config.tick.min(Duration::from_millis(200)));
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string payload".into())
}

fn record_exit(root: &Path, record: &SupervisorRecord, reason: &str) -> io::Result<()> {
    let exit = ExitRecord {
        pid: record.identity.pid,
        version: record.version.clone(),
        started_at_ms: record.started_at_ms,
        exited_at_ms: Some(now_ms()),
        reason: reason.to_string(),
    };
    let path = root.join(SUPERVISOR_EXIT);
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    serde_json::to_writer(&mut temporary, &exit)?;
    temporary.persist(&path).map_err(|error| error.error)?;
    crate::host_state::protect_host_state_path(&path, false)?;
    Ok(())
}

fn publish(root: &Path, record: &SupervisorRecord) -> io::Result<()> {
    let path = root.join(SUPERVISOR_STATE);
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    serde_json::to_writer(&mut temporary, record)?;
    temporary.persist(&path).map_err(|error| error.error)?;
    crate::host_state::protect_host_state_path(&path, false)?;
    Ok(())
}

/// Remove the record only if it is still ours.
fn withdraw(root: &Path, pid: i32) {
    let path = root.join(SUPERVISOR_STATE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            crate::warn_unrecorded("read supervisor state before removal", Err::<(), _>(error));
            return;
        }
    };
    let record = match serde_json::from_str::<SupervisorRecord>(&text) {
        Ok(record) => record,
        Err(error) => {
            crate::warn_unrecorded("parse supervisor state before removal", Err::<(), _>(error));
            return;
        }
    };
    if record.identity.pid != pid {
        return;
    }
    let result = match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    };
    crate::warn_unrecorded("remove supervisor state file", result);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The default work: watch the store and stay up while any environment
/// needs supervision. A store that cannot be read counts as "needed", so a
/// transient error never makes the supervisor walk away from running
/// environments.
pub struct StoreWatch {
    pub store_path: PathBuf,
}

impl SupervisorWork for StoreWatch {
    fn tick(&mut self) -> TickOutcome {
        let needs = match super::store::EnvironmentStore::open_read_only(&self.store_path) {
            Ok(None) => false,
            Ok(Some(store)) => store.any_needs_supervisor().unwrap_or(true),
            Err(_) => true,
        };
        TickOutcome {
            needs_supervisor: needs,
        }
    }
}
