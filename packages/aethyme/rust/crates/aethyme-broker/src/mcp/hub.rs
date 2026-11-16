//! The shared MCP hub: one process per host and build that runs one
//! upstream server per key and multiplexes many agents onto it.
//!
//! Discipline (the broker service's, see `docs/architecture/broker-service.md`):
//! - a private 0700 directory `<host state>/mcp/hub/`, a 0600 socket, a
//!   permanent singleton lock held for the hub's life, and a separate startup
//!   lock for whoever launches it ([`super::hub_client`]);
//! - started lazily by `connect`, never at boot and never pre-warmed; it
//!   launches its own executable by absolute path in its own process group;
//! - socket and lock names carry the build identity, so a hub from a newer
//!   build takes new clients while an older one drains and exits; a live hub
//!   is never killed;
//! - every connection is checked to come from this user (peer credentials);
//! - request and response bodies are never logged.
//!
//! Wire protocol on the socket: the client sends one JSON line
//! `{"hub":1,"op":"attach","build",...,"key","argv","cwd","env"}` (or
//! `{"hub":1,"op":"status"}`), the hub answers `{"hub":1,"ok":true}` or
//! `{"hub":1,"ok":false,"reason":...}`. After an accepted attach the client
//! sends its MCP `initialize` and then plain MCP lines; the hub answers the
//! `initialize` from the upstream's cached result, or with one control line
//! `{"aethyme_hub":"fallback","reason":...}` when this client cannot share
//! (another protocol version, an upstream that needs client features).
//!
//! Lifecycle: an upstream stops [`HubConfig::idle_timeout`] after its last
//! client leaves; the hub exits when it has had no upstream for
//! [`HubConfig::empty_grace`]. An upstream that crashes fails its in-flight
//! requests with a retryable error, restarts with exponential backoff
//! (1 s to 60 s), is initialized again and announces `tools/list_changed`;
//! five crashes within five minutes mark its key unhealthy for ten minutes,
//! during which clients start private servers. Nothing polls faster than
//! once a second while idle: the lifecycle thread sleeps until its next
//! deadline.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::jsonrpc::{self, Kind, read_message};
use super::mux::{ClientId, HUB_INITIALIZE_ID, Route, Router};
use super::telemetry::{self, Event};
use crate::file_lock::ExclusiveFileLock;
use crate::service::transport;

/// Messages queued for one client before it is disconnected.
pub const CLIENT_QUEUE: usize = 256;
/// Messages queued for one upstream before the sending client is
/// disconnected (the upstream is never blocked).
pub const UPSTREAM_QUEUE: usize = 1024;
/// Client requests in flight per upstream before the sending client is
/// disconnected.
pub const MAX_IN_FLIGHT: usize = 256;
const CRASH_WINDOW: Duration = Duration::from_secs(300);
const CRASHES_UNHEALTHY: usize = 5;
const UNHEALTHY_FOR: Duration = Duration::from_secs(600);
const INIT_TIMEOUT: Duration = Duration::from_secs(30);
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
const FIRST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_GRACE: Duration = Duration::from_secs(5);
const LOG_LIMIT: u64 = 8 * 1024 * 1024;
const MEMORY_SAMPLE_EVERY: Duration = Duration::from_secs(60);

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

/// How one hub behaves. Tests shorten the timings here, never through the
/// environment.
#[derive(Debug, Clone)]
pub struct HubConfig {
    pub paths: HubPaths,
    /// Where audit events go (`<host state>/mcp`).
    pub events_dir: Option<PathBuf>,
    pub idle_timeout: Duration,
    pub empty_grace: Duration,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    /// Register each upstream as a host resource lease held by the hub.
    pub register_leases: bool,
}

impl HubConfig {
    /// The production settings for a hub in `directory`.
    pub fn production(directory: &Path) -> Self {
        Self {
            paths: HubPaths::new(directory),
            events_dir: directory.parent().map(Path::to_path_buf),
            idle_timeout: Duration::from_secs(600),
            empty_grace: Duration::from_secs(60),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            register_leases: true,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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
}

enum Init {
    NotStarted,
    Pending,
    Ready(Value),
    Failed(String),
}

struct ClientHandle {
    queue: SyncSender<Vec<u8>>,
    stream: UnixStream,
}

struct Lease {
    lease_id: String,
    generation: u64,
    token: String,
}

struct UpstreamState {
    writer: Option<SyncSender<Vec<u8>>>,
    pid: Option<u32>,
    router: Router,
    clients: HashMap<ClientId, ClientHandle>,
    init: Init,
    init_params: Option<Value>,
    protocol: Option<String>,
    announce_on_ready: bool,
    idle_since: Option<Instant>,
    generation: u64,
    started_at: Option<Instant>,
    backoff: Duration,
    stopped: bool,
    lease: Option<Lease>,
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
    unhealthy: HashMap<String, Instant>,
    private_only: HashSet<String>,
    crashes: HashMap<String, VecDeque<Instant>>,
    counters: BTreeMap<String, KeyCounters>,
    attaching: usize,
    last_activity: Instant,
    stopping: bool,
}

struct Hub {
    config: HubConfig,
    state: Mutex<HubState>,
    wake: Condvar,
    next_client: AtomicU64,
}

/// The uid of the process at the other end of `stream`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
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
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
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
    // SAFETY: killpg takes no pointers; `group` is the process group this hub
    // created for its own upstream child (process_group(0) at spawn).
    let _signalled = unsafe { libc::killpg(group, signal) };
}

fn hello_reply(ok: bool, reason: Option<&str>) -> Vec<u8> {
    let mut reply = json!({"hub": 1, "ok": ok});
    if let Some(reason) = reason {
        reply["reason"] = json!(reason);
    }
    jsonrpc::encode(&reply)
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
            private_only: HashSet::new(),
            crashes: HashMap::new(),
            counters: BTreeMap::new(),
            attaching: 0,
            last_activity: Instant::now(),
            stopping: false,
        }),
        wake: Condvar::new(),
        next_client: AtomicU64::new(1),
    });
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
/// hub's own empty grace. No deadline, no wakeup.
fn lifecycle(hub: &Arc<Hub>) {
    let mut state = lock(&hub.state);
    loop {
        if state.stopping {
            return;
        }
        let now = Instant::now();
        state.unhealthy.retain(|_, until| *until > now);
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
        for key in expired {
            if let Some(upstream) = state.upstreams.remove(&key) {
                stop_upstream(hub, &upstream, "idle");
                state.last_activity = now;
            }
        }
        if state.upstreams.is_empty() && state.attaching == 0 {
            let deadline = state.last_activity + hub.config.empty_grace;
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
        for until in state.unhealthy.values() {
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

fn handle_connection(hub: &Arc<Hub>, stream: UnixStream) {
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
    match hello.get("op").and_then(Value::as_str) {
        Some("status") => {
            let mut stream = stream;
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
fn status(hub: &Arc<Hub>) -> Value {
    let mut state = lock(&hub.state);
    let now = Instant::now();
    let mut upstreams = Vec::new();
    let mut samples: Vec<(String, u32)> = Vec::new();
    for (key, upstream) in &state.upstreams {
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
            "init": init,
            "protocol": upstream_state.protocol,
            "idle_for_ms": upstream_state.idle_since.map(|since| since.elapsed().as_millis() as u64),
        }));
        if let Some(pid) = upstream_state.pid {
            samples.push((key.clone(), pid));
        }
    }
    // Memory footprint, sampled at most once a minute per key.
    for (key, pid) in samples {
        let counters = state.counters.entry(key).or_default();
        if counters
            .memory_sampled
            .is_none_or(|at| now.duration_since(at) >= MEMORY_SAMPLE_EVERY)
        {
            counters.memory_sampled = Some(now);
            counters.memory_kib = resident_kib(pid);
        }
    }
    let unhealthy: BTreeMap<String, u64> = state
        .unhealthy
        .iter()
        .map(|(key, until)| {
            (
                key[..12.min(key.len())].to_string(),
                until.saturating_duration_since(now).as_secs(),
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
        "upstreams": upstreams,
        "unhealthy_seconds_left": unhealthy,
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
    Some((
        key,
        Spec {
            name: hello.get("name")?.as_str()?.to_string(),
            argv,
            cwd: PathBuf::from(hello.get("cwd")?.as_str()?),
            env,
        },
    ))
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
    let upstream = {
        let mut state = lock(&hub.state);
        state.last_activity = Instant::now();
        let refusal = if state.stopping {
            Some("stopping")
        } else if state
            .unhealthy
            .get(&key)
            .is_some_and(|until| *until > Instant::now())
        {
            Some("unhealthy")
        } else if state.private_only.contains(&key) {
            Some("not_shareable")
        } else {
            None
        };
        if let Some(reason) = refusal {
            drop(state);
            hub.fallback(&key, &name, reason);
            refuse(&mut stream, reason);
            return;
        }
        let upstream = match state.upstreams.get(&key) {
            Some(upstream) => Arc::clone(upstream),
            None => {
                let upstream = Arc::new(Upstream {
                    key: key.clone(),
                    key_short: key[..12].to_string(),
                    spec,
                    state: Mutex::new(UpstreamState {
                        writer: None,
                        pid: None,
                        router: Router::new(),
                        clients: HashMap::new(),
                        init: Init::NotStarted,
                        init_params: None,
                        protocol: None,
                        announce_on_ready: false,
                        // Idle until a client registers, so an upstream
                        // nobody attaches to still stops.
                        idle_since: Some(Instant::now()),
                        generation: 0,
                        started_at: None,
                        backoff: hub.config.initial_backoff,
                        stopped: false,
                        lease: None,
                    }),
                    ready: Condvar::new(),
                });
                if let Err(error) = start_process(hub, &upstream) {
                    drop(state);
                    eprintln!("aethyme mcp hub: {name}: could not start the upstream: {error}");
                    hub.fallback(&key, &name, "upstream_start_failed");
                    refuse(&mut stream, "upstream_start_failed");
                    return;
                }
                state.upstreams.insert(key.clone(), Arc::clone(&upstream));
                upstream
            }
        };
        state.attaching += 1;
        // Not idle while a client attaches (initialize may take a while).
        lock(&upstream.state).idle_since = None;
        upstream
    };
    let attached = attach_client(hub, &upstream, &mut stream, &mut reader);
    if attached.is_none() {
        let mut upstream_state = lock(&upstream.state);
        if upstream_state.clients.is_empty() && !upstream_state.stopped {
            upstream_state.idle_since = Some(Instant::now());
        }
    }
    {
        let mut state = lock(&hub.state);
        state.attaching = state.attaching.saturating_sub(1);
        state.last_activity = Instant::now();
    }
    hub.wake.notify_all();
    let Some(client) = attached else {
        return;
    };
    serve_client(hub, &upstream, client, &mut reader);
    client_gone(hub, &upstream, client, &stream);
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
    let params = initialize.get("params").cloned().unwrap_or(json!({}));
    let requested = params
        .get("protocolVersion")
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
    let result = match initialized_result(hub, upstream, &params) {
        Ok(result) => result,
        Err(reason) => {
            fallback(stream, &reason);
            return None;
        }
    };
    stream.set_read_timeout(None).ok()?;
    let (queue, receiver) = sync_channel::<Vec<u8>>(CLIENT_QUEUE);
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
                queue: queue.clone(),
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
    let spawned = thread::Builder::new()
        .name("mcp-hub-client-writer".into())
        .spawn(move || {
            for bytes in receiver {
                if writer.write_all(&bytes).is_err() {
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
    queue
        .try_send(jsonrpc::encode(
            &json!({"jsonrpc": "2.0", "id": client_id, "result": result}),
        ))
        .ok()?;
    Some(id)
}

/// The upstream's cached initialize result, initializing it with these
/// params first if nobody has.
fn initialized_result(
    hub: &Arc<Hub>,
    upstream: &Arc<Upstream>,
    params: &Value,
) -> Result<Value, String> {
    let mut state = lock(&upstream.state);
    if matches!(state.init, Init::NotStarted) {
        if state.writer.is_none() {
            return Err("upstream_restarting".into());
        }
        state.init_params = Some(params.clone());
        state.protocol = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .map(str::to_string);
        send_initialize(&mut state)?;
    }
    let deadline = Instant::now() + INIT_TIMEOUT;
    loop {
        match &state.init {
            Init::Ready(result) => return Ok(result.clone()),
            Init::Failed(reason) => return Err(reason.clone()),
            Init::NotStarted => return Err("upstream_restarting".into()),
            Init::Pending => {}
        }
        let now = Instant::now();
        if now >= deadline {
            drop(state);
            hub.audit(upstream, "initialize_timeout", None);
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
    let params = state.init_params.clone().unwrap_or(json!({}));
    let message = json!({
        "jsonrpc": "2.0",
        "id": HUB_INITIALIZE_ID,
        "method": "initialize",
        "params": params,
    });
    let writer = state
        .writer
        .as_ref()
        .ok_or_else(|| "upstream_restarting".to_string())?;
    writer
        .try_send(jsonrpc::encode(&message))
        .map_err(|_| "upstream_busy".to_string())?;
    state.init = Init::Pending;
    Ok(())
}

/// Queue a reply the hub answers itself; false when the client is gone or full.
fn reply_to(state: &UpstreamState, client: ClientId, reply: Value) -> bool {
    state
        .clients
        .get(&client)
        .is_some_and(|handle| handle.queue.try_send(jsonrpc::encode(&reply)).is_ok())
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
        let mut state = lock(&upstream.state);
        let to_upstream = match jsonrpc::kind(&message) {
            Kind::Request => {
                let id = message.get("id").cloned().unwrap_or(Value::Null);
                match message.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        let result = match &state.init {
                            Init::Ready(result) => result.clone(),
                            _ => json!({}),
                        };
                        if !reply_to(
                            &state,
                            client,
                            json!({"jsonrpc": "2.0", "id": id, "result": result}),
                        ) {
                            return;
                        }
                        continue;
                    }
                    Some("ping") => {
                        if !reply_to(
                            &state,
                            client,
                            json!({"jsonrpc": "2.0", "id": id, "result": {}}),
                        ) {
                            return;
                        }
                        continue;
                    }
                    _ => {}
                }
                if state.writer.is_none() {
                    let error =
                        jsonrpc::retryable_error(id, "the upstream server is restarting; retry");
                    if !reply_to(&state, client, error) {
                        return;
                    }
                    continue;
                }
                if state.router.in_flight() >= MAX_IN_FLIGHT {
                    drop(state);
                    hub.count(&upstream.key, |counters| counters.queue_overflows += 1);
                    hub.audit(upstream, "client_overflow", Some("in_flight"));
                    return;
                }
                drop(state);
                hub.count(&upstream.key, |counters| counters.requests += 1);
                state = lock(&upstream.state);
                Some(state.router.client_request(client, message))
            }
            Kind::Notification => {
                let cancelled = message.get("method").and_then(Value::as_str)
                    == Some("notifications/cancelled");
                let routed = state.router.client_notification(client, message);
                if cancelled && routed.is_some() {
                    drop(state);
                    hub.count(&upstream.key, |counters| counters.cancellations += 1);
                    state = lock(&upstream.state);
                }
                routed
            }
            Kind::Response => state.router.client_response(client, message),
            Kind::Invalid => None,
        };
        if let Some(message) = to_upstream {
            let sent = state.writer.as_ref().is_some_and(|writer| {
                !matches!(
                    writer.try_send(jsonrpc::encode(&message)),
                    Err(TrySendError::Full(_))
                )
            });
            if !sent {
                drop(state);
                hub.count(&upstream.key, |counters| counters.queue_overflows += 1);
                hub.audit(upstream, "client_overflow", Some("upstream_queue"));
                return;
            }
        }
    }
}

fn client_gone(hub: &Arc<Hub>, upstream: &Arc<Upstream>, client: ClientId, stream: &UnixStream) {
    crate::warn_unrecorded(
        "close an MCP hub client",
        stream.shutdown(std::net::Shutdown::Both).or_else(|error| {
            if error.kind() == io::ErrorKind::NotConnected {
                Ok(())
            } else {
                Err(error)
            }
        }),
    );
    let became_idle = {
        let mut state = lock(&upstream.state);
        state.clients.remove(&client);
        for message in state.router.client_gone(client) {
            if let Some(writer) = &state.writer {
                // A full queue only loses a cancellation; the reply is dropped anyway.
                let _queued = writer.try_send(jsonrpc::encode(&message));
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

/// Start (or restart) the upstream process. The caller holds no upstream lock.
fn start_process(hub: &Arc<Hub>, upstream: &Arc<Upstream>) -> io::Result<()> {
    let spec = &upstream.spec;
    let log_path = hub
        .config
        .paths
        .directory
        .join(format!("upstream-{}.log", upstream.key_short));
    if fs::metadata(&log_path).is_ok_and(|metadata| metadata.len() > LOG_LIMIT) {
        fs::remove_file(&log_path)?;
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)?;
    let mut child = Command::new(&spec.argv[0])
        .args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log)
        .process_group(0)
        .spawn()?;
    let pid = child.id();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let (Some(mut stdin), Some(stdout)) = (stdin, stdout) else {
        crate::warn_unrecorded("stop an MCP upstream without pipes", child.kill());
        return Err(io::Error::other("the upstream has no stdio pipes"));
    };
    let (writer, receiver) = sync_channel::<Vec<u8>>(UPSTREAM_QUEUE);
    thread::Builder::new()
        .name("mcp-hub-upstream-writer".into())
        .spawn(move || {
            for bytes in receiver {
                if stdin
                    .write_all(&bytes)
                    .and_then(|()| stdin.flush())
                    .is_err()
                {
                    break;
                }
            }
            // Dropping stdin ends the upstream's input.
        })?;
    let generation = {
        let mut state = lock(&upstream.state);
        state.writer = Some(writer);
        state.pid = Some(pid);
        state.generation += 1;
        state.started_at = Some(Instant::now());
        if state.init_params.is_some() {
            // A restart: initialize again with the first client's params.
            state.announce_on_ready = true;
            if let Err(reason) = send_initialize(&mut state) {
                state.init = Init::Failed(reason);
            }
        }
        state.generation
    };
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
        })?;
    register_lease(hub, upstream, pid, generation);
    hub.audit(upstream, "upstream_started", None);
    Ok(())
}

fn register_lease(hub: &Arc<Hub>, upstream: &Arc<Upstream>, pid: u32, generation: u64) {
    if !hub.config.register_leases {
        return;
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
    let granted = crate::HostResourceCoordinator::open_default()
        .and_then(|mut coordinator| coordinator.acquire(&request));
    match granted {
        Ok(grant) => {
            lock(&upstream.state).lease = Some(Lease {
                lease_id: grant.lease.lease_id,
                generation: grant.lease.generation,
                token: grant.ownership_token,
            });
        }
        Err(error) => eprintln!(
            "aethyme mcp hub: {}: upstream not registered as a host lease: {error}",
            upstream.spec.name
        ),
    }
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
        } else {
            let result = message.get("result").cloned().unwrap_or(json!({}));
            let needs_client_features = result
                .get("capabilities")
                .and_then(Value::as_object)
                .is_some_and(|capabilities| {
                    ["sampling", "roots", "elicitation"]
                        .iter()
                        .any(|feature| capabilities.contains_key(*feature))
                });
            if needs_client_features {
                // Refuse new attaches first, then wake the waiting ones.
                drop(state);
                let removed = {
                    let mut hub_state = lock(&hub.state);
                    hub_state.private_only.insert(upstream.key.clone());
                    hub_state.upstreams.remove(&upstream.key)
                };
                lock(&upstream.state).init = Init::Failed("not_shareable".into());
                upstream.ready.notify_all();
                if let Some(removed) = removed {
                    stop_upstream(hub, &removed, "not_shareable");
                }
                hub.wake.notify_all();
                return;
            }
            state.init = Init::Ready(result);
            if let Some(writer) = &state.writer {
                let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
                let _queued = writer.try_send(jsonrpc::encode(&initialized));
            }
            if announce {
                let changed =
                    json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
                for handle in state.clients.values() {
                    let _queued = handle.queue.try_send(jsonrpc::encode(&changed));
                }
            }
        }
        upstream.ready.notify_all();
        return;
    }
    let is_error = message.get("error").is_some();
    let mut overflowed: Vec<UnixStream> = Vec::new();
    for route in state.router.upstream_message(message) {
        match route {
            Route::Client(client, message) => {
                if let Some(handle) = state.clients.get(&client)
                    && handle.queue.try_send(jsonrpc::encode(&message)).is_err()
                    && let Ok(stream) = handle.stream.try_clone()
                {
                    overflowed.push(stream);
                }
            }
            Route::Broadcast(message) => {
                let bytes = jsonrpc::encode(&message);
                for handle in state.clients.values() {
                    if handle.queue.try_send(bytes.clone()).is_err()
                        && let Ok(stream) = handle.stream.try_clone()
                    {
                        overflowed.push(stream);
                    }
                }
            }
            Route::Upstream(message) => {
                if let Some(writer) = &state.writer {
                    let _queued = writer.try_send(jsonrpc::encode(&message));
                }
            }
        }
    }
    drop(state);
    if is_error {
        hub.count(&upstream.key, |counters| counters.errors += 1);
    }
    // A client that cannot keep up is disconnected; the upstream never waits.
    for stream in overflowed {
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
    let (failed, uptime, lease) = {
        let mut state = lock(&upstream.state);
        if state.generation != generation {
            return;
        }
        let lease = state.lease.take();
        if state.stopped {
            drop(state);
            release_lease(lease);
            return;
        }
        state.writer = None;
        state.pid = None;
        if matches!(state.init, Init::Pending) {
            state.init = Init::NotStarted;
            upstream.ready.notify_all();
        } else if matches!(state.init, Init::Ready(_)) {
            state.init = Init::NotStarted;
        }
        let failed = state
            .router
            .fail_all("the upstream server exited; retry the request");
        for (client, error) in &failed {
            if let Some(handle) = state.clients.get(client) {
                let _queued = handle.queue.try_send(jsonrpc::encode(error));
            }
        }
        (failed.len(), state.started_at.map(|at| at.elapsed()), lease)
    };
    release_lease(lease);
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
        if unhealthy {
            state
                .unhealthy
                .insert(upstream.key.clone(), now + UNHEALTHY_FOR);
            state.crashes.remove(&upstream.key);
            if state
                .upstreams
                .get(&upstream.key)
                .is_some_and(|current| Arc::ptr_eq(current, upstream))
            {
                state.upstreams.remove(&upstream.key);
            }
            state.last_activity = now;
        }
        unhealthy
    };
    if unhealthy {
        hub.audit(upstream, "unhealthy", None);
        let streams: Vec<UnixStream> = {
            let mut state = lock(&upstream.state);
            state.stopped = true;
            state
                .clients
                .values()
                .filter_map(|handle| handle.stream.try_clone().ok())
                .collect()
        };
        for stream in streams {
            let _closed = stream.shutdown(std::net::Shutdown::Both);
        }
        hub.wake.notify_all();
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
            if let Err(error) = start_process(&restart_hub, &restart_upstream) {
                eprintln!(
                    "aethyme mcp hub: {}: restart failed: {error}",
                    restart_upstream.spec.name
                );
                let mut state = lock(&restart_hub.state);
                state
                    .unhealthy
                    .insert(restart_upstream.key.clone(), Instant::now() + UNHEALTHY_FOR);
                state.upstreams.remove(&restart_upstream.key);
                lock(&restart_upstream.state).stopped = true;
                drop(state);
                restart_hub.wake.notify_all();
            }
        });
    crate::warn_unrecorded("schedule an MCP upstream restart", spawned);
}

/// Stop an upstream for good: its input closes, its process group gets
/// SIGTERM and, after a grace period, SIGKILL.
fn stop_upstream(hub: &Arc<Hub>, upstream: &Arc<Upstream>, reason: &str) {
    let (pid, streams) = {
        let mut state = lock(&upstream.state);
        state.stopped = true;
        state.writer = None;
        let streams: Vec<UnixStream> = state
            .clients
            .values()
            .filter_map(|handle| handle.stream.try_clone().ok())
            .collect();
        (state.pid.take(), streams)
    };
    for stream in streams {
        let _closed = stream.shutdown(std::net::Shutdown::Both);
    }
    if let Some(pid) = pid {
        send_signal(pid, libc::SIGTERM);
        let spawned = thread::Builder::new()
            .name("mcp-hub-stop".into())
            .spawn(move || {
                thread::sleep(STOP_GRACE);
                send_signal(pid, libc::SIGKILL);
            });
        crate::warn_unrecorded("schedule an MCP upstream kill", spawned);
    }
    hub.audit(upstream, "upstream_stopped", Some(reason));
}
