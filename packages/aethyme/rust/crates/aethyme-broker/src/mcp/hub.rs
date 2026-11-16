//! The shared MCP hub: one process per host and build that runs one
//! upstream server per key and multiplexes many agents onto it.
//!
//! Discipline (the broker service's, see `docs/architecture/broker-service.md`):
//! - a private 0700 directory `<host state>/mcp/hub/`, a 0600 socket, a
//!   permanent singleton lock held for the hub's life, and a separate startup
//!   lock for whoever launches it ([`super::hub_client`]);
//! - started lazily by `connect`, never at boot and never pre-warmed, fully
//!   detached (its own session, never a child of an agent's process), with an
//!   environment reduced to what it needs;
//! - socket and lock names carry the build identity, so a hub from a newer
//!   build takes new clients while an older one drains and exits; a live hub
//!   is never killed;
//! - every connection is checked to come from this user (peer credentials),
//!   and connections are capped;
//! - request and response bodies are never logged.
//!
//! Wire protocol on the socket: the client sends one JSON line
//! `{"hub":1,"op":"attach","build",...,"key","argv","cwd","env"}` (or
//! `{"hub":1,"op":"status"}`), the hub answers `{"hub":1,"ok":true}` or
//! `{"hub":1,"ok":false,"reason":...}`. After an accepted attach the client
//! sends its MCP `initialize` and then plain MCP lines; the hub answers the
//! `initialize` from the upstream's cached result, or with one control line
//! `{"aethyme_hub":"fallback","reason":...}` when this client cannot share.
//!
//! The hub initializes every upstream itself, with no client capabilities
//! and its own `clientInfo`, so no server ever sends a client request meant
//! for one agent to another (see [`super::mux`]).
//!
//! Lifecycle: an upstream stops [`HubConfig::idle_timeout`] after its last
//! client leaves; the hub exits when it has had no upstream for
//! [`HubConfig::empty_grace`] (before its first attach,
//! [`HubConfig::startup_grace`]). A crashed upstream fails its in-flight
//! requests with a retryable error, restarts with exponential backoff (1 s
//! to 60 s), is initialized again and announces `tools/list_changed`; client
//! requests arriving meanwhile are held until it is ready. Five crashes
//! within five minutes, an initialize that never answers, or an upstream
//! that cannot be registered as a host lease mark the key unhealthy for ten
//! minutes, during which clients start private servers. On SIGTERM, SIGINT
//! or SIGHUP the hub stops every upstream process group before exiting; each
//! upstream's pid, process group and start time are recorded, and a new hub
//! stops the groups an earlier one left behind (only when pid and start time
//! both match). Nothing polls while idle: the lifecycle thread sleeps until
//! its next deadline.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::io::{self, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::jsonrpc::{self, Kind, read_message};
use super::mux::{Action, ClientId, HUB_INITIALIZE_ID, INVALID_REQUEST, Route, Router};
use super::telemetry::{self, Event};
use crate::file_lock::ExclusiveFileLock;
use crate::service::transport;

const CRASH_WINDOW: Duration = Duration::from_secs(300);
const CRASHES_UNHEALTHY: usize = 5;
const UNHEALTHY_FOR: Duration = Duration::from_secs(600);
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
const FIRST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_GRACE: Duration = Duration::from_secs(5);
const LOG_LIMIT: u64 = 8 * 1024 * 1024;
const MEMORY_SAMPLE_EVERY: Duration = Duration::from_secs(60);
/// Client requests held per upstream while it restarts.
const HELD_LIMIT: usize = 1024;
const RECORDS_DIRECTORY: &str = "upstreams";

/// The build this hub serves; clients of another build never attach.
pub fn build_identity() -> String {
    aethyme_engine::daemon::runtime_build_identity()
}

fn build_tag() -> String {
    let digest = format!("{:x}", Sha256::digest(build_identity().as_bytes()));
    digest[..12].to_string()
}

/// The hub's files in its private directory, named for this build.
#[derive(Debug, Clone)]
pub struct HubPaths {
    pub directory: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
    pub startup_lock: PathBuf,
    pub log: PathBuf,
    pub pid: PathBuf,
}

impl HubPaths {
    pub fn new(directory: &Path) -> Self {
        let tag = build_tag();
        Self {
            directory: directory.to_path_buf(),
            socket: directory.join(format!("h-{tag}.sock")),
            lock: directory.join(format!("h-{tag}.lock")),
            startup_lock: directory.join(format!("h-{tag}.start")),
            log: directory.join(format!("h-{tag}.log")),
            pid: directory.join(format!("h-{tag}.pid")),
        }
    }
}

/// How one hub behaves. Tests shorten the timings and limits here, never
/// through the environment.
#[derive(Debug, Clone)]
pub struct HubConfig {
    pub paths: HubPaths,
    /// Where audit events go (`<host state>/mcp`).
    pub events_dir: Option<PathBuf>,
    pub idle_timeout: Duration,
    /// How long the hub lives without upstreams once something attached.
    pub empty_grace: Duration,
    /// How long a fresh hub waits for its first attach.
    pub startup_grace: Duration,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub init_timeout: Duration,
    pub max_connections: usize,
    /// Per-client outgoing queue, by messages and by bytes.
    pub client_queue: usize,
    pub client_queue_bytes: usize,
    /// Per-upstream input queue, by messages and by bytes.
    pub upstream_queue: usize,
    pub upstream_queue_bytes: usize,
    /// Requests in flight per client, and per upstream; over either, the
    /// request gets a retryable error.
    pub per_client_in_flight: usize,
    pub global_in_flight: usize,
    /// Register each upstream as a host resource lease held by the hub; when
    /// registration fails the key is not shared.
    pub register_leases: bool,
    /// Handle SIGTERM, SIGINT and SIGHUP by stopping every upstream (only for
    /// a hub that owns its process).
    pub handle_signals: bool,
}

impl HubConfig {
    /// The production settings for a hub in `directory`.
    pub fn production(directory: &Path) -> Self {
        Self {
            paths: HubPaths::new(directory),
            events_dir: directory.parent().map(Path::to_path_buf),
            idle_timeout: Duration::from_secs(600),
            empty_grace: Duration::from_secs(60),
            startup_grace: Duration::from_secs(60),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            init_timeout: Duration::from_secs(30),
            max_connections: 256,
            client_queue: 256,
            client_queue_bytes: 64 * 1024 * 1024,
            upstream_queue: 1024,
            upstream_queue_bytes: 64 * 1024 * 1024,
            per_client_in_flight: 64,
            global_in_flight: 1024,
            register_leases: true,
            handle_signals: true,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A bounded queue, by messages and by bytes, with its writer's counter.
#[derive(Clone)]
struct Outbox {
    queue: SyncSender<Vec<u8>>,
    bytes: Arc<AtomicUsize>,
    cap: usize,
}

impl Outbox {
    fn new(count: usize, cap: usize) -> (Self, Receiver<Vec<u8>>) {
        let (queue, receiver) = sync_channel(count);
        (
            Self {
                queue,
                bytes: Arc::new(AtomicUsize::new(0)),
                cap,
            },
            receiver,
        )
    }

    /// Queue `message`; false when either bound is reached (never blocks).
    fn offer(&self, message: Vec<u8>) -> bool {
        let length = message.len();
        let before = self.bytes.fetch_add(length, Ordering::AcqRel);
        if before.saturating_add(length) > self.cap || self.queue.try_send(message).is_err() {
            self.bytes.fetch_sub(length, Ordering::AcqRel);
            return false;
        }
        true
    }

    /// The writer took `length` bytes off the queue.
    fn written(bytes: &AtomicUsize, length: usize) {
        bytes.fetch_sub(length, Ordering::AcqRel);
    }
}

/// Counters per key, kept in memory and reported by `status`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct KeyCounters {
    pub name: String,
    pub clients_connected: u64,
    pub clients_disconnected: u64,
    pub requests: u64,
    pub errors: u64,
    pub cancellations: u64,
    pub crashes: u64,
    pub restarts: u64,
    pub queue_overflows: u64,
    pub refused_server_requests: u64,
    pub dropped_log_messages: u64,
    pub fallbacks: BTreeMap<String, u64>,
    pub memory_kib: Option<u64>,
    #[serde(skip)]
    memory_sampled: Option<Instant>,
}

#[derive(Debug, Clone)]
struct Spec {
    name: String,
    argv: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    digest: String,
}

enum Init {
    NotStarted,
    Pending,
    Ready(Value),
    Failed(String),
}

struct ClientHandle {
    outbox: Outbox,
    stream: UnixStream,
}

struct Lease {
    lease_id: String,
    generation: u64,
    token: String,
}

struct UpstreamState {
    writer: Option<Outbox>,
    pid: Option<u32>,
    router: Router,
    clients: HashMap<ClientId, ClientHandle>,
    init: Init,
    protocol: Option<String>,
    announce_on_ready: bool,
    held: Vec<(ClientId, Value)>,
    idle_since: Option<Instant>,
    generation: u64,
    started_at: Option<Instant>,
    starting: bool,
    backoff: Duration,
    stopped: bool,
    lease: Option<Lease>,
    record: Option<PathBuf>,
    /// The last generation whose process was reaped: its pid (and process
    /// group id) may since belong to someone else.
    reaped: u64,
}

struct Upstream {
    key: String,
    key_short: String,
    spec: Spec,
    state: Mutex<UpstreamState>,
    ready: Condvar,
}

struct HubState {
    upstreams: HashMap<String, Arc<Upstream>>,
    unhealthy: HashMap<String, (Instant, String)>,
    crashes: HashMap<String, VecDeque<Instant>>,
    counters: BTreeMap<String, KeyCounters>,
    attaching: usize,
    ever_attached: bool,
    last_activity: Instant,
    stopping: bool,
}

/// Spawns every upstream from one long-lived thread, so Linux's
/// parent-death signal (tied to the spawning thread) fires only when the
/// hub process itself dies.
struct Spawner {
    requests: Mutex<Sender<(Command, Sender<io::Result<Child>>)>>,
}

impl Spawner {
    fn start() -> io::Result<Self> {
        let (requests, receiver) = channel::<(Command, Sender<io::Result<Child>>)>();
        thread::Builder::new()
            .name("mcp-hub-spawner".into())
            .spawn(move || {
                for (mut command, reply) in receiver {
                    let _sent = reply.send(command.spawn());
                }
            })?;
        Ok(Self {
            requests: Mutex::new(requests),
        })
    }

    fn spawn(&self, command: Command) -> io::Result<Child> {
        let (reply, answer) = channel();
        lock(&self.requests)
            .send((command, reply))
            .map_err(|_| io::Error::other("the MCP hub spawner stopped"))?;
        answer
            .recv()
            .map_err(|_| io::Error::other("the MCP hub spawner stopped"))?
    }
}

struct Hub {
    config: HubConfig,
    state: Mutex<HubState>,
    wake: Condvar,
    next_client: AtomicU64,
    connections: AtomicUsize,
    spawner: Spawner,
}

/// Runs in an upstream child between fork and exec: unblock the signals the
/// hub blocks for its signal thread, and on Linux die with the hub.
fn upstream_child_setup() -> io::Result<()> {
    // SAFETY: called in the forked child before exec, where only
    // async-signal-safe functions may run: sigemptyset, sigaddset,
    // sigprocmask and prctl are. `set` is a local, fully initialized by
    // sigemptyset before use.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaddset(&mut set, signal);
        }
        libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        #[cfg(any(target_os = "linux", target_os = "android"))]
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }
    Ok(())
}

/// The uid of the process at the other end of `stream`.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the descriptor is an open socket owned by `stream` for this
    // call; `credentials` and `length` are valid, writable and sized for
    // SO_PEERCRED's `ucred`.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

/// The uid of the process at the other end of `stream`.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: the descriptor is an open Unix socket owned by `stream` for
    // this call; `uid` and `gid` are valid writable locations.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

fn send_signal(process_group: u32, signal: libc::c_int) {
    let Ok(group) = libc::pid_t::try_from(process_group) else {
        return;
    };
    if group <= 1 {
        return;
    }
    // SAFETY: killpg takes no pointers; `group` is a process group this hub
    // created for its own upstream child (process_group(0) at spawn), or one
    // recorded by an earlier hub whose leader's pid and start time matched.
    let _signalled = unsafe { libc::killpg(group, signal) };
}

/// A process's start time as `ps` prints it, to tell a recorded upstream
/// from an unrelated process that later got the same pid.
pub(crate) fn process_start(pid: u32) -> Option<String> {
    let output = Command::new(crate::system_programs::ps())
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// An upstream as recorded on disk, so its group can be stopped after the
/// hub that started it died.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamRecord {
    pub pid: u32,
    pub process_group: u32,
    pub started: String,
}

pub(crate) fn write_record(
    directory: &Path,
    name: &str,
    record: &UpstreamRecord,
) -> io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let records = directory.join(RECORDS_DIRECTORY);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&records)?;
    let path = records.join(format!("{name}.json"));
    let mut temporary = tempfile::NamedTempFile::new_in(&records)?;
    temporary.write_all(&serde_json::to_vec(record).map_err(io::Error::other)?)?;
    temporary.persist(&path).map_err(|error| error.error)?;
    Ok(path)
}

/// Stop the process groups an earlier hub recorded, when each leader still
/// has the recorded pid and start time; forget every record. Returns the
/// groups signalled.
pub fn reap_recorded_upstreams(directory: &Path) -> Vec<u32> {
    let mut stopped = Vec::new();
    let mut matched: Vec<UpstreamRecord> = Vec::new();
    let Ok(entries) = fs::read_dir(directory.join(RECORDS_DIRECTORY)) else {
        return stopped;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let record: Option<UpstreamRecord> = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        if let Some(record) = record
            && process_start(record.pid).as_deref() == Some(record.started.as_str())
        {
            send_signal(record.process_group, libc::SIGTERM);
            stopped.push(record.process_group);
            matched.push(record);
        }
        crate::warn_unrecorded("forget an MCP upstream record", fs::remove_file(&path));
    }
    if !stopped.is_empty() {
        let survivors = matched.clone();
        let spawned = thread::Builder::new()
            .name("mcp-hub-reap".into())
            .spawn(move || {
                thread::sleep(STOP_GRACE);
                // Only a leader that is still the recorded process.
                for record in survivors {
                    if process_start(record.pid).as_deref() == Some(record.started.as_str()) {
                        send_signal(record.process_group, libc::SIGKILL);
                    }
                }
            });
        crate::warn_unrecorded("schedule an MCP upstream kill", spawned);
    }
    stopped
}

fn hello_reply(ok: bool, reason: Option<&str>) -> Vec<u8> {
    let mut reply = json!({"hub": 1, "ok": ok});
    if let Some(reason) = reason {
        reply["reason"] = json!(reason);
    }
    jsonrpc::encode(&reply)
}

fn spec_digest(argv: &[String], cwd: &Path, env: &[(String, String)]) -> String {
    let material = json!({"argv": argv, "cwd": cwd, "env": env});
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&material).unwrap_or_default())
    )
}

impl Hub {
    fn audit(&self, upstream: &Upstream, outcome: &str, reason: Option<&str>) {
        let Some(directory) = &self.config.events_dir else {
            return;
        };
        crate::warn_unrecorded(
            "record an MCP hub event",
            telemetry::append(
                directory,
                &Event {
                    at_ms: telemetry::now_ms(),
                    command: "hub".into(),
                    name: upstream.spec.name.clone(),
                    outcome: outcome.into(),
                    reason: reason.map(str::to_string),
                    copy: None,
                    key: Some(upstream.key_short.clone()),
                    phases_ms: BTreeMap::new(),
                    shadowed: false,
                    fallback: false,
                    warnings: Vec::new(),
                },
            ),
        );
    }

    fn count(&self, key: &str, update: impl FnOnce(&mut KeyCounters)) {
        let mut state = lock(&self.state);
        update(state.counters.entry(key.to_string()).or_default());
    }

    fn fallback(&self, key: &str, name: &str, reason: &str) {
        self.count(key, |counters| {
            counters.name = name.to_string();
            *counters.fallbacks.entry(reason.to_string()).or_default() += 1;
        });
    }

    /// Mark `key` unhealthy and forget its upstream (if it is this one).
    fn mark_unhealthy(&self, upstream: &Arc<Upstream>, reason: &str) {
        let now = Instant::now();
        let mut state = lock(&self.state);
        state.unhealthy.insert(
            upstream.key.clone(),
            (now + UNHEALTHY_FOR, reason.to_string()),
        );
        state.crashes.remove(&upstream.key);
        if state
            .upstreams
            .get(&upstream.key)
            .is_some_and(|current| Arc::ptr_eq(current, upstream))
        {
            state.upstreams.remove(&upstream.key);
        }
        state.last_activity = now;
        drop(state);
        self.wake.notify_all();
    }
}

/// Block the termination signals in this thread before any other thread
/// exists, so every thread inherits the mask and only the signal thread
/// receives them.
fn block_termination_signals() -> io::Result<libc::sigset_t> {
    // SAFETY: `set` is a local fully initialized by sigemptyset; the calls
    // take valid pointers to it and change only this thread's mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaddset(&mut set, signal);
        }
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(set)
    }
}

/// Wait for a termination signal and stop every upstream before exiting.
fn spawn_signal_thread(hub: &Arc<Hub>, set: libc::sigset_t) -> io::Result<()> {
    let signal_hub = Arc::clone(hub);
    thread::Builder::new()
        .name("mcp-hub-signals".into())
        .spawn(move || {
            let mut received: libc::c_int = 0;
            // SAFETY: `set` was initialized by block_termination_signals and
            // `received` is a valid writable location.
            let waited = unsafe { libc::sigwait(&set, &mut received) };
            if waited != 0 {
                return;
            }
            shutdown_on_signal(&signal_hub);
        })?;
    Ok(())
}

fn shutdown_on_signal(hub: &Arc<Hub>) {
    let upstreams: Vec<Arc<Upstream>> = {
        let mut state = lock(&hub.state);
        state.stopping = true;
        state
            .upstreams
            .drain()
            .map(|(_, upstream)| upstream)
            .collect()
    };
    let mut groups = Vec::new();
    for upstream in &upstreams {
        let mut state = lock(&upstream.state);
        state.stopped = true;
        state.writer = None;
        if let Some(pid) = state.pid.take() {
            groups.push(pid);
        }
        if let Some(record) = state.record.take() {
            let _removed = fs::remove_file(record);
        }
    }
    for group in &groups {
        send_signal(*group, libc::SIGTERM);
    }
    thread::sleep(Duration::from_millis(500));
    for group in &groups {
        send_signal(*group, libc::SIGKILL);
    }
    let _removed = fs::remove_file(&hub.config.paths.socket);
    let _removed = fs::remove_file(&hub.config.paths.pid);
    std::process::exit(0);
}

/// Run a hub until it has been empty for its grace period. Returns when it
/// stops; refuses when another hub of this build owns the directory.
pub fn serve(config: HubConfig) -> io::Result<()> {
    let paths = config.paths.clone();
    transport::private_directory(&paths.directory)?;
    let lock_file = transport::private_coordination_file(&paths.lock)?;
    let _singleton = ExclusiveFileLock::try_acquire(lock_file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrInUse,
            "an MCP hub of this build already owns this directory",
        )
    })?;
    // Before any thread starts (the spawner and the reaper included).
    let signals = if config.handle_signals {
        Some(block_termination_signals()?)
    } else {
        None
    };
    // Upstreams an earlier hub of this directory left behind.
    reap_recorded_upstreams(&paths.directory);
    match fs::symlink_metadata(&paths.socket) {
        Ok(metadata)
            if metadata.file_type().is_socket() && metadata.uid() == transport::owner() =>
        {
            match UnixStream::connect(&paths.socket) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "an MCP hub socket is live",
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    fs::remove_file(&paths.socket)?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the MCP hub socket path is not an owned socket",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&paths.socket)?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    let socket_identity =
        fs::symlink_metadata(&paths.socket).map(|metadata| (metadata.dev(), metadata.ino()))?;
    fs::write(&paths.pid, format!("{}\n", std::process::id()))?;

    let hub = Arc::new(Hub {
        config,
        state: Mutex::new(HubState {
            upstreams: HashMap::new(),
            unhealthy: HashMap::new(),
            crashes: HashMap::new(),
            counters: BTreeMap::new(),
            attaching: 0,
            ever_attached: false,
            last_activity: Instant::now(),
            stopping: false,
        }),
        wake: Condvar::new(),
        next_client: AtomicU64::new(1),
        connections: AtomicUsize::new(0),
        spawner: Spawner::start()?,
    });
    if let Some(set) = signals {
        spawn_signal_thread(&hub, set)?;
    }
    let lifecycle_hub = Arc::clone(&hub);
    thread::Builder::new()
        .name("mcp-hub-lifecycle".into())
        .spawn(move || lifecycle(&lifecycle_hub))?;

    for stream in listener.incoming() {
        if lock(&hub.state).stopping {
            break;
        }
        let Ok(stream) = stream else {
            continue;
        };
        // Same user only, on every connection.
        if peer_uid(&stream).ok() != Some(transport::owner()) {
            continue;
        }
        let connection_hub = Arc::clone(&hub);
        let spawned = thread::Builder::new()
            .name("mcp-hub-client".into())
            .spawn(move || handle_connection(&connection_hub, stream));
        crate::warn_unrecorded("start an MCP hub connection thread", spawned);
    }

    // Remove only the socket this hub bound.
    if fs::symlink_metadata(&paths.socket)
        .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == socket_identity)
    {
        crate::warn_unrecorded("remove the MCP hub socket", fs::remove_file(&paths.socket));
    }
    crate::warn_unrecorded("remove the MCP hub pid file", fs::remove_file(&paths.pid));
    Ok(())
}

fn sooner(deadline: Instant, next: &mut Option<Instant>) {
    *next = Some(next.map_or(deadline, |current| current.min(deadline)));
}

/// Sleep until the next deadline: idle upstreams, unhealthy marks, and the
/// hub's own grace. No deadline, no wakeup. Upstreams are stopped outside
/// the hub lock.
fn lifecycle(hub: &Arc<Hub>) {
    let mut state = lock(&hub.state);
    loop {
        if state.stopping {
            return;
        }
        let now = Instant::now();
        state.unhealthy.retain(|_, (until, _)| *until > now);
        let mut next: Option<Instant> = None;
        let mut expired = Vec::new();
        for (key, upstream) in &state.upstreams {
            if let Some(since) = lock(&upstream.state).idle_since {
                let deadline = since + hub.config.idle_timeout;
                if deadline <= now {
                    expired.push(key.clone());
                } else {
                    sooner(deadline, &mut next);
                }
            }
        }
        if !expired.is_empty() {
            let stopping: Vec<Arc<Upstream>> = expired
                .iter()
                .filter_map(|key| state.upstreams.remove(key))
                .collect();
            state.last_activity = now;
            drop(state);
            for upstream in stopping {
                stop_upstream(hub, &upstream, "idle");
            }
            state = lock(&hub.state);
            continue;
        }
        if state.upstreams.is_empty() && state.attaching == 0 {
            let grace = if state.ever_attached {
                hub.config.empty_grace
            } else {
                hub.config.startup_grace
            };
            let deadline = state.last_activity + grace;
            if deadline <= now {
                state.stopping = true;
                drop(state);
                // Wake the accept loop so it sees `stopping`.
                crate::warn_unrecorded(
                    "wake the MCP hub accept loop",
                    UnixStream::connect(&hub.config.paths.socket).map(drop),
                );
                return;
            }
            sooner(deadline, &mut next);
        }
        for (until, _) in state.unhealthy.values() {
            sooner(*until, &mut next);
        }
        state = match next {
            Some(deadline) => {
                hub.wake
                    .wait_timeout(state, deadline.saturating_duration_since(now))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => hub.wake.wait(state).unwrap_or_else(PoisonError::into_inner),
        };
    }
}

/// Counts open connections; refuses past the cap.
struct ConnectionSlot<'a>(&'a AtomicUsize);

impl Drop for ConnectionSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle_connection(hub: &Arc<Hub>, mut stream: UnixStream) {
    let open = hub.connections.fetch_add(1, Ordering::AcqRel) + 1;
    let _slot = ConnectionSlot(&hub.connections);
    if stream.set_read_timeout(Some(HELLO_TIMEOUT)).is_err() {
        return;
    }
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let Ok(Some(line)) = read_message(&mut reader) else {
        return;
    };
    let Ok(hello) = serde_json::from_slice::<Value>(&line) else {
        return;
    };
    if hello.get("hub").and_then(Value::as_u64) != Some(1) {
        return;
    }
    if open > hub.config.max_connections {
        crate::warn_unrecorded(
            "refuse an MCP hub connection",
            stream.write_all(&hello_reply(false, Some("too_many_connections"))),
        );
        return;
    }
    match hello.get("op").and_then(Value::as_str) {
        Some("status") => {
            crate::warn_unrecorded(
                "answer an MCP hub status request",
                stream.write_all(&jsonrpc::encode(&status(hub))),
            );
        }
        Some("attach") => attach(hub, stream, reader, &hello),
        _ => {}
    }
}

/// What `aethyme mcp status` shows: the hub, its upstreams and counters.
/// Memory is sampled outside the hub lock, at most once a minute per key.
fn status(hub: &Arc<Hub>) -> Value {
    let now = Instant::now();
    let to_sample: Vec<(String, u32)> = {
        let state = lock(&hub.state);
        state
            .upstreams
            .iter()
            .filter_map(|(key, upstream)| {
                let pid = lock(&upstream.state).pid?;
                let due = state.counters.get(key).is_none_or(|counters| {
                    counters
                        .memory_sampled
                        .is_none_or(|at| now.duration_since(at) >= MEMORY_SAMPLE_EVERY)
                });
                due.then(|| (key.clone(), pid))
            })
            .collect()
    };
    let samples: Vec<(String, Option<u64>)> = to_sample
        .into_iter()
        .map(|(key, pid)| (key, resident_kib(pid)))
        .collect();
    let mut state = lock(&hub.state);
    for (key, kib) in samples {
        let counters = state.counters.entry(key).or_default();
        counters.memory_sampled = Some(now);
        counters.memory_kib = kib;
    }
    let mut upstreams = Vec::new();
    for upstream in state.upstreams.values() {
        let upstream_state = lock(&upstream.state);
        let init = match &upstream_state.init {
            Init::NotStarted => "not_started",
            Init::Pending => "initializing",
            Init::Ready(_) => "ready",
            Init::Failed(_) => "failed",
        };
        upstreams.push(json!({
            "key_short": upstream.key_short,
            "name": upstream.spec.name,
            "pid": upstream_state.pid,
            "clients": upstream_state.clients.len(),
            "in_flight": upstream_state.router.in_flight(),
            "held": upstream_state.held.len(),
            "init": init,
            "protocol": upstream_state.protocol,
            "idle_for_ms": upstream_state
                .idle_since
                .map(|since| u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)),
        }));
    }
    let unhealthy: BTreeMap<String, Value> = state
        .unhealthy
        .iter()
        .map(|(key, (until, reason))| {
            (
                key[..12.min(key.len())].to_string(),
                json!({"seconds_left": until.saturating_duration_since(now).as_secs(), "reason": reason}),
            )
        })
        .collect();
    let counters: BTreeMap<String, &KeyCounters> = state
        .counters
        .iter()
        .map(|(key, counters)| (key[..12.min(key.len())].to_string(), counters))
        .collect();
    json!({
        "hub": 1,
        "pid": std::process::id(),
        "build": build_identity(),
        "connections": hub.connections.load(Ordering::Acquire),
        "upstreams": upstreams,
        "unhealthy": unhealthy,
        "counters": counters,
    })
}

/// Resident set size in KiB, from `ps`.
fn resident_kib(pid: u32) -> Option<u64> {
    let output = Command::new(crate::system_programs::ps())
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn spec_from_hello(hello: &Value) -> Option<(String, Spec)> {
    let key = hello.get("key")?.as_str()?.to_string();
    if key.len() < 12 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let argv: Vec<String> = hello
        .get("argv")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<_>>()?;
    if argv.is_empty() {
        return None;
    }
    let env: Vec<(String, String)> = hello
        .get("env")?
        .as_array()?
        .iter()
        .map(|pair| {
            let pair = pair.as_array()?;
            Some((
                pair.first()?.as_str()?.to_string(),
                pair.get(1)?.as_str()?.to_string(),
            ))
        })
        .collect::<Option<_>>()?;
    let cwd = PathBuf::from(hello.get("cwd")?.as_str()?);
    let digest = spec_digest(&argv, &cwd, &env);
    Some((
        key,
        Spec {
            name: hello.get("name")?.as_str()?.to_string(),
            argv,
            cwd,
            env,
            digest,
        },
    ))
}

fn new_upstream(hub: &Arc<Hub>, key: &str, spec: Spec) -> Arc<Upstream> {
    Arc::new(Upstream {
        key: key.to_string(),
        key_short: key[..12].to_string(),
        spec,
        state: Mutex::new(UpstreamState {
            writer: None,
            pid: None,
            router: Router::new(),
            clients: HashMap::new(),
            init: Init::NotStarted,
            protocol: None,
            announce_on_ready: false,
            held: Vec::new(),
            // Not idle while its first client attaches.
            idle_since: None,
            generation: 0,
            started_at: None,
            starting: true,
            backoff: hub.config.initial_backoff,
            stopped: false,
            lease: None,
            record: None,
            reaped: 0,
        }),
        ready: Condvar::new(),
    })
}

fn attach(
    hub: &Arc<Hub>,
    mut stream: UnixStream,
    mut reader: BufReader<UnixStream>,
    hello: &Value,
) {
    let refuse = |stream: &mut UnixStream, reason: &str| {
        crate::warn_unrecorded(
            "refuse an MCP hub attach",
            stream.write_all(&hello_reply(false, Some(reason))),
        );
    };
    if hello.get("build").and_then(Value::as_str) != Some(build_identity().as_str()) {
        refuse(&mut stream, "build_mismatch");
        return;
    }
    let Some((key, spec)) = spec_from_hello(hello) else {
        refuse(&mut stream, "invalid_attach");
        return;
    };
    let name = spec.name.clone();
    let digest = spec.digest.clone();
    // Only the hub's bookkeeping happens under its lock; the spawn, the lease
    // and audit events happen after.
    let decided: Result<(Arc<Upstream>, bool), &str> = {
        let mut state = lock(&hub.state);
        state.last_activity = Instant::now();
        if state.stopping {
            Err("stopping")
        } else if state
            .unhealthy
            .get(&key)
            .is_some_and(|(until, _)| *until > Instant::now())
        {
            Err("unhealthy")
        } else {
            let found = state.upstreams.get(&key).map(Arc::clone);
            match found {
                Some(upstream) if upstream.spec.digest != digest => Err("spec_mismatch"),
                Some(upstream) => {
                    state.attaching += 1;
                    state.ever_attached = true;
                    lock(&upstream.state).idle_since = None;
                    Ok((upstream, false))
                }
                None => {
                    let upstream = new_upstream(hub, &key, spec);
                    state.upstreams.insert(key.clone(), Arc::clone(&upstream));
                    state.attaching += 1;
                    state.ever_attached = true;
                    Ok((upstream, true))
                }
            }
        }
    };
    let (upstream, created) = match decided {
        Ok(decided) => decided,
        Err(reason) => {
            hub.fallback(&key, &name, reason);
            refuse(&mut stream, reason);
            return;
        }
    };
    let release_attaching = |hub: &Arc<Hub>| {
        let mut state = lock(&hub.state);
        state.attaching = state.attaching.saturating_sub(1);
        state.last_activity = Instant::now();
        drop(state);
        hub.wake.notify_all();
    };
    if created && let Err(reason) = first_start(hub, &upstream) {
        release_attaching(hub);
        hub.fallback(&key, &name, &reason);
        refuse(&mut stream, &reason);
        return;
    }
    let attached = attach_client(hub, &upstream, &mut stream, &mut reader);
    if attached.is_none() {
        let mut upstream_state = lock(&upstream.state);
        if upstream_state.clients.is_empty() && !upstream_state.stopped {
            upstream_state.idle_since = Some(Instant::now());
        }
    }
    release_attaching(hub);
    let Some(client) = attached else {
        return;
    };
    serve_client(hub, &upstream, client, &mut reader);
    client_gone(hub, &upstream, client, &stream);
}

/// Start a new upstream, outside the hub lock. On failure the key is marked
/// unhealthy (a later attach starts privately) and waiters are woken.
fn first_start(hub: &Arc<Hub>, upstream: &Arc<Upstream>) -> Result<(), String> {
    match start_process(hub, upstream) {
        Ok(()) => Ok(()),
        Err(reason) => {
            {
                let mut state = lock(&upstream.state);
                state.starting = false;
                state.stopped = true;
                state.init = Init::Failed(reason.clone());
                state.idle_since = Some(Instant::now());
            }
            upstream.ready.notify_all();
            eprintln!(
                "aethyme mcp hub: {}: could not start the upstream: {reason}",
                upstream.spec.name
            );
            hub.mark_unhealthy(upstream, &reason);
            hub.audit(upstream, "upstream_start_failed", Some(&reason));
            Err(reason)
        }
    }
}

/// Accept the client, answer its `initialize`, and register it. `None` when
/// it must start a private server instead (the control line says why).
fn attach_client(
    hub: &Arc<Hub>,
    upstream: &Arc<Upstream>,
    stream: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
) -> Option<ClientId> {
    stream.write_all(&hello_reply(true, None)).ok()?;
    stream.set_read_timeout(Some(FIRST_MESSAGE_TIMEOUT)).ok()?;
    let line = read_message(reader).ok()??;
    let initialize: Value = serde_json::from_slice(&line).ok()?;
    if initialize.get("method").and_then(Value::as_str) != Some("initialize") {
        return None;
    }
    let client_id = initialize.get("id").cloned().unwrap_or(Value::Null);
    let requested = initialize
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .map(str::to_string);
    let fallback = |stream: &mut UnixStream, reason: &str| {
        hub.fallback(&upstream.key, &upstream.spec.name, reason);
        crate::warn_unrecorded(
            "send an MCP hub fallback",
            stream.write_all(&jsonrpc::encode(
                &json!({"aethyme_hub": "fallback", "reason": reason}),
            )),
        );
    };
    let result = match initialized_result(hub, upstream, requested.as_deref()) {
        Ok(result) => result,
        Err(reason) => {
            fallback(stream, &reason);
            return None;
        }
    };
    stream.set_read_timeout(None).ok()?;
    let (outbox, receiver) = Outbox::new(hub.config.client_queue, hub.config.client_queue_bytes);
    let id = hub.next_client.fetch_add(1, Ordering::Relaxed);
    {
        let mut state = lock(&upstream.state);
        if state.protocol != requested {
            drop(state);
            fallback(stream, "protocol_version");
            return None;
        }
        if state.stopped {
            drop(state);
            fallback(stream, "upstream_stopped");
            return None;
        }
        state.clients.insert(
            id,
            ClientHandle {
                outbox: outbox.clone(),
                stream: stream.try_clone().ok()?,
            },
        );
        state.idle_since = None;
    }
    hub.count(&upstream.key, |counters| {
        counters.name = upstream.spec.name.clone();
        counters.clients_connected += 1;
    });
    let mut writer = stream.try_clone().ok()?;
    let written = Arc::clone(&outbox.bytes);
    let spawned = thread::Builder::new()
        .name("mcp-hub-client-writer".into())
        .spawn(move || {
            for bytes in receiver {
                let length = bytes.len();
                let failed = writer.write_all(&bytes).is_err();
                Outbox::written(&written, length);
                if failed {
                    break;
                }
            }
            // Already closed by client_gone or an overflow is fine.
            let _closed = writer.shutdown(std::net::Shutdown::Both);
        });
    if spawned.is_err() {
        return None;
    }
    // The client's initialize is answered from the cached result.
    outbox
        .offer(jsonrpc::encode(
            &json!({"jsonrpc": "2.0", "id": client_id, "result": result}),
        ))
        .then_some(id)
}

/// The params the hub initializes an upstream with: no client capabilities
/// and its own identity, whatever the first client declared.
fn hub_initialize_params(protocol: Option<&str>) -> Value {
    json!({
        "protocolVersion": protocol.unwrap_or("2025-06-18"),
        "capabilities": {},
        "clientInfo": {"name": "aethyme-mcp-hub", "version": env!("CARGO_PKG_VERSION")},
    })
}

/// The upstream's cached initialize result, initializing it first (with
/// the hub's own params) if nobody has. An initialize that never answers
/// marks the key unhealthy and stops the upstream; waiters fail at once.
fn initialized_result(
    hub: &Arc<Hub>,
    upstream: &Arc<Upstream>,
    requested: Option<&str>,
) -> Result<Value, String> {
    let deadline = Instant::now() + hub.config.init_timeout;
    let mut state = lock(&upstream.state);
    loop {
        // While the process starts, wait; the timeout below still applies.
        if !state.starting && matches!(state.init, Init::NotStarted) {
            if state.writer.is_none() || state.stopped {
                return Err("upstream_restarting".into());
            }
            state.protocol = requested.map(str::to_string);
            send_initialize(&mut state)?;
        }
        match &state.init {
            Init::Ready(result) => return Ok(result.clone()),
            Init::Failed(reason) => return Err(reason.clone()),
            Init::NotStarted | Init::Pending => {}
        }
        let now = Instant::now();
        if now >= deadline {
            state.init = Init::Failed("initialize_timeout".into());
            upstream.ready.notify_all();
            drop(state);
            hub.mark_unhealthy(upstream, "initialize_timeout");
            stop_upstream(hub, upstream, "initialize_timeout");
            return Err("initialize_timeout".into());
        }
        state = upstream
            .ready
            .wait_timeout(state, deadline - now)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

fn send_initialize(state: &mut UpstreamState) -> Result<(), String> {
    let message = json!({
        "jsonrpc": "2.0",
        "id": HUB_INITIALIZE_ID,
        "method": "initialize",
        "params": hub_initialize_params(state.protocol.as_deref()),
    });
    let writer = state
        .writer
        .as_ref()
        .ok_or_else(|| "upstream_restarting".to_string())?;
    if !writer.offer(jsonrpc::encode(&message)) {
        return Err("upstream_busy".into());
    }
    state.init = Init::Pending;
    Ok(())
}

/// Queue a reply the hub answers itself; false when the client is gone or
/// its queue is full.
fn reply_to(state: &UpstreamState, client: ClientId, reply: &Value) -> bool {
    state
        .clients
        .get(&client)
        .is_some_and(|handle| handle.outbox.offer(jsonrpc::encode(reply)))
}

/// Answer a client directly; an overflow when its queue is full.
fn answer(state: &UpstreamState, client: ClientId, reply: Value) -> Dispatch {
    if reply_to(state, client, &reply) {
        Dispatch::Handled
    } else {
        Dispatch::Overflow("client_queue")
    }
}

/// What happened to one client request.
enum Dispatch {
    Handled,
    /// The client cannot keep up (or the upstream queue is full): close it.
    Overflow(&'static str),
}

fn ready_for_requests(state: &UpstreamState) -> bool {
    state.writer.is_some() && matches!(state.init, Init::Ready(_))
}

/// Handle one client request under the upstream lock.
fn dispatch_request(
    config: &HubConfig,
    state: &mut UpstreamState,
    client: ClientId,
    message: Value,
) -> Dispatch {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    match message.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let result = match &state.init {
                Init::Ready(result) => result.clone(),
                _ => json!({}),
            };
            return answer(
                state,
                client,
                json!({"jsonrpc": "2.0", "id": id, "result": result}),
            );
        }
        Some("ping") => {
            return answer(
                state,
                client,
                json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            );
        }
        _ => {}
    }
    if !ready_for_requests(state) {
        // Restarting: hold the request until the upstream is initialized.
        if !state.stopped && state.held.len() < HELD_LIMIT {
            state.held.push((client, message));
            return Dispatch::Handled;
        }
        return answer(
            state,
            client,
            jsonrpc::retryable_error(id, "the upstream server is restarting; retry"),
        );
    }
    if state.router.client_in_flight(client) >= config.per_client_in_flight
        || state.router.in_flight() >= config.global_in_flight
    {
        return answer(
            state,
            client,
            jsonrpc::retryable_error(id, "too many requests in flight; retry"),
        );
    }
    match state.router.client_request(client, message) {
        Action::Reply(reply) => answer(state, client, reply),
        Action::Upstream(forwarded) => {
            let sent = state
                .writer
                .as_ref()
                .is_some_and(|writer| writer.offer(jsonrpc::encode(&forwarded)));
            if sent {
                Dispatch::Handled
            } else {
                Dispatch::Overflow("upstream_queue")
            }
        }
    }
}

/// Read the client's messages until it leaves (or overflows).
fn serve_client(
    hub: &Arc<Hub>,
    upstream: &Arc<Upstream>,
    client: ClientId,
    reader: &mut BufReader<UnixStream>,
) {
    loop {
        let Ok(Some(line)) = read_message(reader) else {
            return;
        };
        let Ok(message) = serde_json::from_slice::<Value>(&line) else {
            hub.count(&upstream.key, |counters| counters.errors += 1);
            continue;
        };
        let mut counted_request = false;
        let mut counted_cancel = false;
        let outcome = {
            let mut state = lock(&upstream.state);
            if message.is_array() {
                // JSON-RPC batches were removed from MCP: refuse, never drop.
                let error = jsonrpc::error_response(
                    Value::Null,
                    INVALID_REQUEST,
                    "batches are not supported",
                    None,
                );
                if reply_to(&state, client, &error) {
                    Dispatch::Handled
                } else {
                    Dispatch::Overflow("client_queue")
                }
            } else {
                match jsonrpc::kind(&message) {
                    Kind::Request => {
                        counted_request = true;
                        dispatch_request(&hub.config, &mut state, client, message)
                    }
                    Kind::Notification => {
                        counted_cancel = message.get("method").and_then(Value::as_str)
                            == Some("notifications/cancelled");
                        match state.router.client_notification(client, message) {
                            Some(notification) => {
                                let sent = state.writer.as_ref().is_none_or(|writer| {
                                    writer.offer(jsonrpc::encode(&notification))
                                });
                                if sent {
                                    Dispatch::Handled
                                } else {
                                    Dispatch::Overflow("upstream_queue")
                                }
                            }
                            None => {
                                counted_cancel = false;
                                Dispatch::Handled
                            }
                        }
                    }
                    // Clients answer no server requests: none are routed.
                    Kind::Response | Kind::Invalid => Dispatch::Handled,
                }
            }
        };
        if counted_request || counted_cancel {
            hub.count(&upstream.key, |counters| {
                if counted_request {
                    counters.requests += 1;
                }
                if counted_cancel {
                    counters.cancellations += 1;
                }
            });
        }
        if let Dispatch::Overflow(reason) = outcome {
            hub.count(&upstream.key, |counters| counters.queue_overflows += 1);
            hub.audit(upstream, "client_overflow", Some(reason));
            return;
        }
    }
}

fn client_gone(hub: &Arc<Hub>, upstream: &Arc<Upstream>, client: ClientId, stream: &UnixStream) {
    let _closed = stream.shutdown(std::net::Shutdown::Both);
    let became_idle = {
        let mut state = lock(&upstream.state);
        state.clients.remove(&client);
        state.held.retain(|(owner, _)| *owner != client);
        for message in state.router.client_gone(client) {
            if let Some(writer) = &state.writer {
                // A full queue only loses a cancellation; the reply is dropped anyway.
                let _queued = writer.offer(jsonrpc::encode(&message));
            }
        }
        if state.clients.is_empty() && !state.stopped {
            state.idle_since = Some(Instant::now());
            true
        } else {
            false
        }
    };
    hub.count(&upstream.key, |counters| counters.clients_disconnected += 1);
    if became_idle {
        hub.wake.notify_all();
    }
}

/// Start (or restart) the upstream process, register its lease and record
/// it. Called with no lock held. A key whose upstream cannot be registered
/// as a host lease is not shared.
fn start_process(hub: &Arc<Hub>, upstream: &Arc<Upstream>) -> Result<(), String> {
    let spec = &upstream.spec;
    let log_path = hub
        .config
        .paths
        .directory
        .join(format!("upstream-{}.log", upstream.key_short));
    if fs::metadata(&log_path).is_ok_and(|metadata| metadata.len() > LOG_LIMIT) {
        fs::remove_file(&log_path).map_err(|error| format!("upstream_log:{error}"))?;
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)
        .map_err(|error| format!("upstream_log:{error}"))?;
    let mut command = Command::new(&spec.argv[0]);
    command
        .args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log)
        .process_group(0);
    // SAFETY: `upstream_child_setup` only calls async-signal-safe functions
    // and touches no state shared with the parent.
    unsafe {
        command.pre_exec(upstream_child_setup);
    }
    let mut child = hub
        .spawner
        .spawn(command)
        .map_err(|error| format!("upstream_spawn_failed:{:?}", error.kind()))?;
    let pid = child.id();
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _killed = child.kill();
        let _reaped = child.wait();
        return Err("upstream_without_pipes".into());
    };
    let generation = lock(&upstream.state).generation + 1;
    let mut lease = match register_lease(hub, upstream, pid, generation) {
        Ok(lease) => lease,
        Err(reason) => {
            send_signal(pid, libc::SIGKILL);
            let _reaped = child.wait();
            return Err(reason);
        }
    };
    let record = process_start(pid).and_then(|started| {
        write_record(
            &hub.config.paths.directory,
            &format!("{}-{generation}", upstream.key_short),
            &UpstreamRecord {
                pid,
                process_group: pid,
                started,
            },
        )
        .ok()
    });
    let (writer, receiver) =
        Outbox::new(hub.config.upstream_queue, hub.config.upstream_queue_bytes);
    let written = Arc::clone(&writer.bytes);
    let installed = {
        let mut state = lock(&upstream.state);
        if state.stopped {
            // Stopped (idle or unhealthy) while this one was starting.
            None
        } else {
            state.writer = Some(writer);
            state.pid = Some(pid);
            state.generation = generation;
            state.started_at = Some(Instant::now());
            state.starting = false;
            state.lease = lease.take();
            state.record = record.clone();
            if matches!(state.init, Init::NotStarted) && state.protocol.is_some() {
                // A restart: initialize again, then announce list_changed.
                state.announce_on_ready = true;
                if let Err(reason) = send_initialize(&mut state) {
                    state.init = Init::Failed(reason);
                }
            }
            Some(())
        }
    };
    upstream.ready.notify_all();
    if installed.is_none() {
        send_signal(pid, libc::SIGKILL);
        let _reaped = child.wait();
        release_lease(lease.take());
        if let Some(record) = record {
            let _removed = fs::remove_file(record);
        }
        return Err("upstream_stopped".into());
    }
    thread::Builder::new()
        .name("mcp-hub-upstream-writer".into())
        .spawn(move || {
            for bytes in receiver {
                let length = bytes.len();
                let failed = stdin
                    .write_all(&bytes)
                    .and_then(|()| stdin.flush())
                    .is_err();
                Outbox::written(&written, length);
                if failed {
                    break;
                }
            }
            // Dropping stdin ends the upstream's input.
        })
        .map_err(|error| format!("upstream_thread:{error}"))?;
    let reader_hub = Arc::clone(hub);
    let reader_upstream = Arc::clone(upstream);
    thread::Builder::new()
        .name("mcp-hub-upstream-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(line)) = read_message(&mut reader) {
                if let Ok(message) = serde_json::from_slice::<Value>(&line) {
                    upstream_message(&reader_hub, &reader_upstream, message);
                }
            }
            let status = child.wait();
            upstream_exited(&reader_hub, &reader_upstream, generation, status.ok());
        })
        .map_err(|error| format!("upstream_thread:{error}"))?;
    hub.audit(upstream, "upstream_started", None);
    Ok(())
}

fn register_lease(
    hub: &Arc<Hub>,
    upstream: &Arc<Upstream>,
    pid: u32,
    generation: u64,
) -> Result<Option<Lease>, String> {
    if !hub.config.register_leases {
        return Ok(None);
    }
    let hub_pid = std::process::id();
    let name = format!("mcp-upstream-{hub_pid}-{}-{generation}", upstream.key_short);
    let request = crate::HostResourceRequest {
        schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id: name.clone(),
        repository: "aethyme-mcp-hub".into(),
        worktree_fingerprint: format!("upstream-{pid}"),
        run_id: format!("mcp-hub-{hub_pid}"),
        ttl_seconds: 86_400,
        holder_pid: Some(hub_pid),
        resources: vec![crate::HostResourceRequirement {
            key: "upstream".into(),
            resource: crate::HostResourceKind::ExclusiveKey { name },
        }],
    };
    crate::HostResourceCoordinator::open_default()
        .and_then(|mut coordinator| coordinator.acquire(&request))
        .map(|grant| {
            Some(Lease {
                lease_id: grant.lease.lease_id,
                generation: grant.lease.generation,
                token: grant.ownership_token,
            })
        })
        .map_err(|error| {
            eprintln!(
                "aethyme mcp hub: {}: the upstream could not be registered as a host lease, so \
                 its key is not shared: {error}",
                upstream.spec.name
            );
            "lease_unavailable".to_string()
        })
}

fn release_lease(lease: Option<Lease>) {
    let Some(lease) = lease else {
        return;
    };
    crate::warn_unrecorded(
        "release an MCP upstream lease",
        crate::HostResourceCoordinator::open_default().and_then(|mut coordinator| {
            coordinator.release(&lease.lease_id, lease.generation, &lease.token)
        }),
    );
}

/// Route one upstream message (the hub's own initialize reply included).
fn upstream_message(hub: &Arc<Hub>, upstream: &Arc<Upstream>, message: Value) {
    let mut state = lock(&upstream.state);
    if jsonrpc::kind(&message) == Kind::Response
        && message.get("id").and_then(Value::as_u64) == Some(HUB_INITIALIZE_ID)
    {
        let announce = std::mem::take(&mut state.announce_on_ready);
        if let Some(error) = message.get("error") {
            state.init = Init::Failed(format!(
                "upstream_initialize_error:{}",
                error.get("code").and_then(Value::as_i64).unwrap_or(0)
            ));
            upstream.ready.notify_all();
            return;
        }
        let result = message.get("result").cloned().unwrap_or(json!({}));
        state.init = Init::Ready(result);
        if let Some(writer) = &state.writer {
            let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
            let _queued = writer.offer(jsonrpc::encode(&initialized));
        }
        let mut overflowed = Vec::new();
        if announce {
            let changed = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
            for (client, handle) in &state.clients {
                if !handle.outbox.offer(jsonrpc::encode(&changed)) {
                    overflowed.push(*client);
                }
            }
        }
        // Requests held during the restart go out now, in arrival order.
        let held = std::mem::take(&mut state.held);
        for (client, request) in held {
            if !state.clients.contains_key(&client) {
                continue;
            }
            if let Dispatch::Overflow(_) =
                dispatch_request(&hub.config, &mut state, client, request)
            {
                overflowed.push(client);
            }
        }
        let streams = streams_of(&state, &overflowed);
        upstream.ready.notify_all();
        drop(state);
        close_overflowed(hub, upstream, streams);
        return;
    }
    let is_error = message.get("error").is_some();
    let before = state.router.dropped;
    let mut overflowed = Vec::new();
    for route in state.router.upstream_message(message) {
        match route {
            Route::Client(client, message) => {
                if let Some(handle) = state.clients.get(&client)
                    && !handle.outbox.offer(jsonrpc::encode(&message))
                {
                    overflowed.push(client);
                }
            }
            Route::Broadcast(message) => {
                let bytes = jsonrpc::encode(&message);
                for (client, handle) in &state.clients {
                    if !handle.outbox.offer(bytes.clone()) {
                        overflowed.push(*client);
                    }
                }
            }
            Route::Upstream(message) => {
                if let Some(writer) = &state.writer {
                    let _queued = writer.offer(jsonrpc::encode(&message));
                }
            }
        }
    }
    let after = state.router.dropped;
    let streams = streams_of(&state, &overflowed);
    drop(state);
    let refused = after.server_requests - before.server_requests;
    let dropped_logs = after.log_messages - before.log_messages;
    if is_error || refused > 0 || dropped_logs > 0 {
        hub.count(&upstream.key, |counters| {
            counters.errors += u64::from(is_error);
            counters.refused_server_requests += refused;
            counters.dropped_log_messages += dropped_logs;
        });
    }
    close_overflowed(hub, upstream, streams);
}

fn streams_of(state: &UpstreamState, clients: &[ClientId]) -> Vec<UnixStream> {
    clients
        .iter()
        .filter_map(|client| state.clients.get(client))
        .filter_map(|handle| handle.stream.try_clone().ok())
        .collect()
}

/// A client that cannot keep up is disconnected; the upstream never waits.
fn close_overflowed(hub: &Arc<Hub>, upstream: &Arc<Upstream>, streams: Vec<UnixStream>) {
    for stream in streams {
        hub.count(&upstream.key, |counters| counters.queue_overflows += 1);
        hub.audit(upstream, "client_overflow", Some("client_queue"));
        let _closed = stream.shutdown(std::net::Shutdown::Both);
    }
}

/// The upstream's output ended. Expected after a stop; otherwise a crash:
/// fail in-flight requests, then restart with backoff or mark unhealthy.
fn upstream_exited(
    hub: &Arc<Hub>,
    upstream: &Arc<Upstream>,
    generation: u64,
    status: Option<std::process::ExitStatus>,
) {
    let (failed, uptime, lease, lost) = {
        let mut state = lock(&upstream.state);
        state.reaped = state.reaped.max(generation);
        if state.generation != generation {
            return;
        }
        let lease = state.lease.take();
        if let Some(record) = state.record.take() {
            let _removed = fs::remove_file(record);
        }
        if state.stopped {
            drop(state);
            release_lease(lease);
            return;
        }
        state.writer = None;
        state.pid = None;
        if matches!(state.init, Init::Pending | Init::Ready(_)) {
            state.init = Init::NotStarted;
            upstream.ready.notify_all();
        }
        let failed = state
            .router
            .fail_all("the upstream server exited; retry the request");
        // A client that cannot even receive its error is disconnected.
        let mut lost = Vec::new();
        for (client, error) in &failed {
            if let Some(handle) = state.clients.get(client)
                && !handle.outbox.offer(jsonrpc::encode(error))
            {
                lost.push(*client);
            }
        }
        let lost = streams_of(&state, &lost);
        (
            failed.len(),
            state.started_at.map(|at| at.elapsed()),
            lease,
            lost,
        )
    };
    release_lease(lease);
    for stream in lost {
        let _closed = stream.shutdown(std::net::Shutdown::Both);
    }
    let reason = status.map(|status| status.to_string());
    hub.audit(upstream, "upstream_crashed", reason.as_deref());
    let now = Instant::now();
    let unhealthy = {
        let mut state = lock(&hub.state);
        let crashes = state.crashes.entry(upstream.key.clone()).or_default();
        crashes.push_back(now);
        while crashes
            .front()
            .is_some_and(|at| now.duration_since(*at) > CRASH_WINDOW)
        {
            crashes.pop_front();
        }
        let unhealthy = crashes.len() >= CRASHES_UNHEALTHY;
        let counters = state.counters.entry(upstream.key.clone()).or_default();
        counters.crashes += 1;
        counters.errors += failed as u64;
        unhealthy
    };
    if unhealthy {
        hub.mark_unhealthy(upstream, "crashes");
        hub.audit(upstream, "unhealthy", Some("crashes"));
        stop_upstream(hub, upstream, "unhealthy");
        return;
    }
    let backoff = {
        let mut state = lock(&upstream.state);
        // A run that lasted a minute resets the backoff.
        if uptime.is_some_and(|uptime| uptime >= Duration::from_secs(60)) {
            state.backoff = hub.config.initial_backoff;
        }
        let backoff = state.backoff;
        state.backoff = (state.backoff * 2).min(hub.config.max_backoff);
        backoff
    };
    let restart_hub = Arc::clone(hub);
    let restart_upstream = Arc::clone(upstream);
    let spawned = thread::Builder::new()
        .name("mcp-hub-restart".into())
        .spawn(move || {
            thread::sleep(backoff);
            if lock(&restart_upstream.state).stopped {
                return;
            }
            restart_hub.count(&restart_upstream.key, |counters| counters.restarts += 1);
            if let Err(reason) = start_process(&restart_hub, &restart_upstream) {
                eprintln!(
                    "aethyme mcp hub: {}: restart failed: {reason}",
                    restart_upstream.spec.name
                );
                restart_hub.mark_unhealthy(&restart_upstream, &reason);
                stop_upstream(&restart_hub, &restart_upstream, "restart_failed");
            }
        });
    crate::warn_unrecorded("schedule an MCP upstream restart", spawned);
}

/// Stop an upstream for good: held requests fail, clients are disconnected,
/// its input closes, its process group gets SIGTERM and, after a grace
/// period, SIGKILL.
fn stop_upstream(hub: &Arc<Hub>, upstream: &Arc<Upstream>, reason: &str) {
    let (pid, streams) = {
        let mut state = lock(&upstream.state);
        state.stopped = true;
        state.starting = false;
        state.writer = None;
        let held = std::mem::take(&mut state.held);
        for (client, request) in held {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let error = jsonrpc::retryable_error(id, "the upstream server stopped; retry");
            let _queued = reply_to(&state, client, &error);
        }
        let streams: Vec<UnixStream> = state
            .clients
            .values()
            .filter_map(|handle| handle.stream.try_clone().ok())
            .collect();
        (state.pid.take().map(|pid| (pid, state.generation)), streams)
    };
    upstream.ready.notify_all();
    for stream in streams {
        let _closed = stream.shutdown(std::net::Shutdown::Both);
    }
    if let Some((pid, generation)) = pid {
        send_signal(pid, libc::SIGTERM);
        let watched = Arc::clone(upstream);
        let spawned = thread::Builder::new()
            .name("mcp-hub-stop".into())
            .spawn(move || {
                thread::sleep(STOP_GRACE);
                // Once reaped, the group id may belong to another process.
                if lock(&watched.state).reaped < generation {
                    send_signal(pid, libc::SIGKILL);
                }
            });
        crate::warn_unrecorded("schedule an MCP upstream kill", spawned);
    }
    hub.audit(upstream, "upstream_stopped", Some(reason));
}
