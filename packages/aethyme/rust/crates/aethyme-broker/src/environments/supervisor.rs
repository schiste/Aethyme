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
use super::logs::{LOG_CAP_BYTES, open_for_append};
use crate::file_lock::{ExclusiveFileLock, open_lock_file};

pub const SUPERVISOR_LOCK: &str = "supervisor.lock";
pub const SPAWN_LOCK: &str = "spawn.lock";
pub const SUPERVISOR_STATE: &str = "supervisor.json";
pub const SUPERVISOR_LOG: &str = "supervisor.log";

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
}

impl SupervisorStatus {
    pub fn running(&self) -> bool {
        self.state == "running"
    }
}

pub fn status(root: &Path) -> SupervisorStatus {
    let absent = SupervisorStatus {
        state: "absent",
        pid: None,
        started_at_ms: None,
        version: None,
    };
    let Ok(text) = fs::read_to_string(root.join(SUPERVISOR_STATE)) else {
        return absent;
    };
    let Ok(record) = serde_json::from_str::<SupervisorRecord>(&text) else {
        return absent;
    };
    let state = match record.identity.observe() {
        Observation::Alive => "running",
        Observation::Gone => return absent,
        Observation::Unproven => "unproven",
    };
    SupervisorStatus {
        state,
        pid: Some(record.identity.pid),
        started_at_ms: Some(record.started_at_ms),
        version: Some(record.version),
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
    let log = open_for_append(&root.join(SUPERVISOR_LOG), LOG_CAP_BYTES)?;
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
                let _ = child.wait();
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
}

/// Run the supervisor loop in this process until it lingers out.
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
    let mut idle_since: Option<Instant> = None;
    let reason = loop {
        if stop() {
            break ExitReason::Stopped;
        }
        let outcome = work.tick();
        if outcome.needs_supervisor {
            idle_since = None;
        } else {
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= config.linger {
                break ExitReason::Linger;
            }
        }
        std::thread::sleep(config.tick);
    };
    withdraw(&config.root, pid);
    drop(lock);
    Ok(reason)
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
    let ours = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<SupervisorRecord>(&text).ok())
        .is_some_and(|record| record.identity.pid == pid);
    if ours {
        let _ = fs::remove_file(path);
    }
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
