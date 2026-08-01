//! Engine-side daemon: serves read-only redb graph queries over a socket.
//!
//! Why this exists
//! ---------------
//! The daemon used to keep a resident `RepositoryMap` warm to avoid rebuilding
//! it for every `task-localize`, `symbol-batch`, and caller query. Those
//! surfaces now read `.aethyme/graph_store.redb` directly. The daemon remains
//! as stable socket/process plumbing for clients that want a long-lived engine
//! process, but it no longer constructs or owns the legacy in-memory map.
//!
//! Startup opens the redb store read-only and performs a tiny overview read so
//! missing or incompatible stores fail before the socket is advertised. Each
//! request opens a fresh read-only handle, preserving the same read/write
//! boundary as the CLI query commands.
//!
//! Wire protocol (JSON line-delimited over AF_UNIX)
//! ------------------------------------------------
//! Request:
//!   `{"command": "task-localize", "task": "<text>"}\n`
//!   `{"command": "ping"}\n`
//!   `{"command": "shutdown"}\n`
//!
//! Response:
//!   `{"ok": true, "result": <command-specific JSON>}\n`
//!   `{"ok": false, "error": "<message>"}\n`
//!
//! Each TCP-style accept handles exactly one request then closes. Stateless
//! from the client's perspective.
//!
//! Concurrency
//! -----------
//! Single-threaded request handler for v1. At typical agent rates (a few
//! req/s), serial dispatch is fine. Future thread-pool variants should keep
//! using read-only redb handles and avoid reintroducing map-backed query paths.
//!
//! Lifecycle
//! ---------
//! - Daemon binds on start after confirming the redb store is readable.
//! - Idle watcher exits the daemon after `idle_timeout` of inactivity.
//! - Explicit `shutdown` command terminates the loop and removes the socket.
//! - Store updates are explicit: rerun `aethyme-engine-cli index --repo <repo>`
//!   after fragment changes.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::graph::navigation::{
    callers_view_redb, task_anchors_view_redb, task_next_view_redb, task_scope_view_redb,
};
use crate::graph::search::symbol_search_redb;
use crate::model::task::TaskInput;
use crate::store::redb::graph_store::{GraphStore, OverviewV2Limits, ReadOnlyGraphStore};

/// Filename prefix for daemon sockets. Originally a disambiguator: the
/// retired Python daemon (`src/daemon.py`, removed 2026-07-13) shared
/// this `$TMPDIR/aethyme` namespace with its own `aethyme.sock`. There is
/// only one daemon now, so the prefix is merely descriptive — kept as-is
/// because renaming the socket buys nothing and orphans any daemon
/// running across the upgrade.
const SOCKET_PREFIX: &str = "engine-";
const SOCKET_DIR_NAME: &str = "aethyme";
pub const DEFAULT_IDLE_TIMEOUT_SECONDS: u64 = 1800; // 30 min

/// Compute the unix socket path for `repo`.
///
/// macOS caps AF_UNIX paths at ~104 bytes, so the default socket lives in
/// `$TMPDIR/aethyme` keyed by a stable hash of the resolved repo path. Sandboxed
/// runners can set `AETHYME_ENGINE_SOCKET_DIR` to a writable short path such as
/// `/tmp/aethyme-engine-sockets`; the client and detached daemon both resolve
/// this function, so the override stays in sync across process boundaries.
pub fn socket_path_for(repo: &Path) -> PathBuf {
    socket_path_in_dir(repo, &socket_dir())
}

fn socket_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("AETHYME_ENGINE_SOCKET_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        return dir;
    }
    let tmp = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    tmp.join(SOCKET_DIR_NAME)
}

fn socket_path_in_dir(repo: &Path, socket_dir: &Path) -> PathBuf {
    let canonical = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    socket_dir.join(format!("{SOCKET_PREFIX}{hex}.sock"))
}

/// Compute the pidfile path inside the repo's `.aethyme/` directory.
pub fn pidfile_path_for(repo: &Path) -> PathBuf {
    repo.join(".aethyme").join("engine-daemon.pid")
}

/// Compute the logfile path inside the repo's `.aethyme/` directory.
pub fn logfile_path_for(repo: &Path) -> PathBuf {
    repo.join(".aethyme").join("engine-daemon.log")
}

/// Options for [`start_detached`].
#[derive(Default)]
pub struct StartOptions {
    pub idle_timeout: Option<String>,
}

/// Outcome of [`start_detached`].
pub enum StartOutcome {
    /// A live daemon already owns the pidfile.
    AlreadyRunning(i32),
    /// A new server process was spawned (detached, own session). The
    /// caller receives the [`std::process::Child`] handle: a spawned
    /// server that dies on startup becomes a zombie of the *caller*
    /// until reaped, so liveness must be checked with `try_wait()` on
    /// this handle — a `kill(pid, 0)` probe reports zombies as alive.
    Spawned(std::process::Child),
}

/// Spawn a detached engine-daemon server for `repo`, using `serve_exe` as
/// the server binary (it must understand `daemon serve --repo <path>` —
/// i.e. `aethyme-engine-cli`). Shared by both front-end binaries so the
/// pidfile/logfile/setsid lifecycle cannot drift between them.
///
/// Idempotent: returns [`StartOutcome::AlreadyRunning`] when a live pid
/// holds the pidfile; stale pidfiles are cleaned and respawned.
pub fn start_detached(
    repo: &Path,
    serve_exe: &Path,
    opts: &StartOptions,
) -> Result<StartOutcome, String> {
    let pidfile = pidfile_path_for(repo);
    if pidfile.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pidfile) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                let alive = unsafe { libc::kill(pid, 0) };
                if alive == 0 {
                    return Ok(StartOutcome::AlreadyRunning(pid));
                }
            }
        }
        let _ = std::fs::remove_file(&pidfile);
    }

    let aethyme_dir = repo.join(".aethyme");
    std::fs::create_dir_all(&aethyme_dir).map_err(|e| format!("create .aethyme: {e}"))?;

    let log_path = logfile_path_for(repo);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("open logfile {}: {}", log_path.display(), e))?;
    let log_stdout = log_file
        .try_clone()
        .map_err(|e| format!("clone log fd: {e}"))?;
    let log_stderr = log_file
        .try_clone()
        .map_err(|e| format!("clone log fd: {e}"))?;

    let mut cmd = std::process::Command::new(serve_exe);
    cmd.arg("daemon").arg("serve").arg("--repo").arg(repo);
    if let Some(idle) = &opts.idle_timeout {
        cmd.arg("--idle-timeout").arg(idle);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log_stdout))
        .stderr(std::process::Stdio::from(log_stderr));

    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    let pid = child.id();
    std::fs::write(&pidfile, pid.to_string()).map_err(|e| format!("write pidfile: {e}"))?;
    Ok(StartOutcome::Spawned(child))
}

/// Outcome of [`wait_until_ready`].
#[derive(PartialEq, Eq, Debug)]
pub enum ReadyOutcome {
    /// The daemon accepted a socket connection.
    Ready,
    /// The watched server process exited before the socket appeared —
    /// failing fast beats sitting out the timeout (a daemon that dies on
    /// startup, e.g. un-indexed repo, dies within milliseconds).
    ProcessExited,
    /// Deadline passed with the process still alive but no socket.
    TimedOut,
}

/// Block until the daemon for `repo` accepts a socket connection, the
/// watched process dies, or the deadline passes. The socket binds only
/// after the redb store has been opened and smoke-read.
pub fn wait_until_ready(
    repo: &Path,
    mut watch: Option<&mut std::process::Child>,
    timeout: std::time::Duration,
) -> ReadyOutcome {
    let socket = socket_path_for(repo);
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            return ReadyOutcome::Ready;
        }
        // try_wait() reaps: a kill(pid, 0) probe would report the child
        // as alive forever once it zombifies under the waiting caller.
        if let Some(child) = watch.as_deref_mut()
            && matches!(child.try_wait(), Ok(Some(_)))
        {
            return ReadyOutcome::ProcessExited;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    ReadyOutcome::TimedOut
}

/// Last few lines of the daemon logfile — the diagnostic callers show
/// when startup fails.
pub fn log_tail(repo: &Path, lines: usize) -> String {
    let path = logfile_path_for(repo);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            let start = all.len().saturating_sub(lines);
            all[start..].join("\n")
        }
        Err(_) => format!("(no log at {})", path.display()),
    }
}

/// Server state held across requests.
struct DaemonState {
    repo: PathBuf,
    last_activity: Instant,
}

/// Configurable parameters for daemon lifecycle.
pub struct DaemonConfig {
    pub repo: PathBuf,
    pub idle_timeout: Duration,
}

impl DaemonConfig {
    pub fn new(repo: PathBuf) -> Self {
        Self {
            repo,
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECONDS),
        }
    }

    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }
}

/// Run the daemon for `config.repo` until idle timeout or `shutdown` request.
///
/// Returns when the listen loop exits cleanly (idle, shutdown, or socket
/// error). Redb open/read errors are returned immediately without ever
/// opening the socket.
pub fn serve_forever(config: DaemonConfig) -> Result<(), String> {
    let socket_path = socket_path_for(&config.repo);
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create socket dir {}: {}", parent.display(), e))?;
    }
    if socket_path.exists() {
        // Stale socket from a previous daemon — try connecting; if connection
        // fails, the previous daemon is gone and we can safely unlink.
        match UnixStream::connect(&socket_path) {
            Ok(_) => {
                return Err(format!(
                    "engine daemon already running at {}",
                    socket_path.display()
                ));
            }
            Err(_) => {
                let _ = std::fs::remove_file(&socket_path);
            }
        }
    }

    eprintln!(
        "aethyme-engine-daemon: opening redb graph store for {} ...",
        config.repo.display()
    );
    let open_started = Instant::now();
    let store = open_daemon_store(&config.repo)?;
    let overview = store
        .overview_v2(OverviewV2Limits::default())
        .map_err(|e| format!("overview_v2: {e}"))?;
    let file_count = overview
        .repo
        .as_ref()
        .map(|repo| repo.file_count)
        .unwrap_or(0);
    let function_sample_count = overview.functions.len();
    drop(store);
    eprintln!(
        "aethyme-engine-daemon: redb ready ({} files, {} sampled functions, open {:?})",
        file_count,
        function_sample_count,
        open_started.elapsed()
    );

    let state = Arc::new(Mutex::new(DaemonState {
        repo: config.repo.clone(),
        last_activity: Instant::now(),
    }));

    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| format!("bind {}: {}", socket_path.display(), e))?;
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("set_nonblocking: {e}"))?;
    eprintln!(
        "aethyme-engine-daemon: listening on {} (idle timeout {:?})",
        socket_path.display(),
        config.idle_timeout
    );

    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Idle-watcher thread: shuts the daemon down after inactivity. We wake
    // up periodically rather than blocking on a long sleep so SIGTERM-driven
    // shutdown is responsive.
    {
        let state = Arc::clone(&state);
        let shutdown = Arc::clone(&shutdown);
        let socket_path = socket_path.clone();
        let idle_timeout = config.idle_timeout;
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(15));
                if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let idle = {
                    let s = state.lock().expect("daemon state lock poisoned");
                    s.last_activity.elapsed()
                };
                if idle >= idle_timeout {
                    eprintln!("aethyme-engine-daemon: idle for {:?}, shutting down", idle);
                    shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
                    // Wake the accept() blocking call by connecting to ourself.
                    let _ = UnixStream::connect(&socket_path);
                    break;
                }
            }
        });
    }

    for incoming in listener.incoming() {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let stream = match incoming {
            Ok(s) => s,
            Err(e) => {
                eprintln!("aethyme-engine-daemon: accept error: {e}");
                continue;
            }
        };
        let request_shutdown = handle_request(stream, &state);
        if request_shutdown {
            shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
            break;
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    eprintln!("aethyme-engine-daemon: stopped");
    Ok(())
}

/// Handle one request. Returns `true` if the request was a shutdown command.
fn handle_request(mut stream: UnixStream, state: &Arc<Mutex<DaemonState>>) -> bool {
    // Bound the client's request size so a misbehaving client can't pin us.
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') || buf.len() > 64 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if buf.is_empty() {
        return false;
    }

    let line = String::from_utf8_lossy(&buf).into_owned();
    let line = line.split('\n').next().unwrap_or("").trim();

    let request: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            send_error(&mut stream, &format!("invalid JSON request: {e}"));
            return false;
        }
    };

    let command = request
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    {
        let mut s = state.lock().expect("daemon state lock poisoned");
        s.last_activity = Instant::now();
    }

    match command {
        "ping" => {
            let repo = repo_from_state(state);
            match open_daemon_store(&repo).and_then(|store| daemon_ping_payload(&repo, &store)) {
                Ok(payload) => send_ok_value(&mut stream, payload),
                Err(e) => send_error(&mut stream, &e),
            }
            false
        }
        "shutdown" => {
            send_ok_value(&mut stream, serde_json::json!({"ok": true}));
            true
        }
        "task-localize" => {
            let task_text = request.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let repo = repo_from_state(state);
            let task = TaskInput::from_task_text(task_text);
            match open_daemon_store(&repo).and_then(|store| {
                let anchors = task_anchors_view_redb(&store, &task).map_err(|e| e.to_string())?;
                let scope = task_scope_view_redb(&store, &task).map_err(|e| e.to_string())?;
                let next = task_next_view_redb(&store, &task).map_err(|e| e.to_string())?;
                Ok(crate::json::task_localization_view(&anchors, &scope, &next))
            }) {
                Ok(view) => send_ok_raw(&mut stream, &view),
                Err(e) => send_error(&mut stream, &e),
            }
            false
        }
        "symbol-batch" => {
            // Run the redb-backed V2 matcher for each query.
            // Returns a JSON object keyed by query: {query → [SearchHit, ...]}.
            // Mirrors the `aethyme-engine-cli symbol-batch` shape so a client
            // can route through the daemon or the CLI interchangeably.
            let queries: Vec<String> = request
                .get("queries")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let limit = request.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

            if queries.is_empty() {
                send_error(&mut stream, "missing or empty `queries` array");
                return false;
            }

            let repo = repo_from_state(state);
            match open_daemon_store(&repo).and_then(|store| {
                let mut results: serde_json::Map<String, serde_json::Value> =
                    serde_json::Map::new();
                for query in &queries {
                    let hits =
                        symbol_search_redb(&store, query, limit).map_err(|e| e.to_string())?;
                    let arr: Vec<serde_json::Value> = hits
                        .into_iter()
                        .map(|h| {
                            serde_json::json!({
                                "id": h.id,
                                "name": h.name,
                                "kind": h.kind,
                                "file": h.file,
                                "line": h.line,
                                "score": h.score,
                                "reason": h.reason,
                            })
                        })
                        .collect();
                    results.insert(query.clone(), serde_json::Value::Array(arr));
                }
                Ok(serde_json::json!({
                    "ok": true,
                    "result": serde_json::Value::Object(results),
                }))
            }) {
                Ok(payload) => send_ok_value(&mut stream, payload),
                Err(e) => send_error(&mut stream, &e),
            }
            false
        }
        "callers-of" => {
            // Look up callers through redb relation views. Returns map keyed by symbol id:
            // `{symbol_id → [{ "id": ..., "label": ..., "path": ... },
            // ...]}`. Symbols not found in the graph store map to empty
            // arrays (not errors — common when a symbol-search hit
            // came from a file with no callgraph edges yet).
            let targets: Vec<String> = request
                .get("targets")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if targets.is_empty() {
                send_error(&mut stream, "missing or empty `targets` array");
                return false;
            }
            let repo = repo_from_state(state);
            match open_daemon_store(&repo).and_then(|store| {
                let mut results: serde_json::Map<String, serde_json::Value> =
                    serde_json::Map::new();
                for target in &targets {
                    let view = callers_view_redb(&store, target).map_err(|e| e.to_string())?;
                    let arr: Vec<serde_json::Value> = view
                        .items
                        .into_iter()
                        .map(|item| {
                            serde_json::json!({
                                "id": item.id,
                                "kind": item.kind,
                                "display": item.display,
                                "relation": item.relation,
                                "confidence": item.confidence,
                            })
                        })
                        .collect();
                    results.insert(target.clone(), serde_json::Value::Array(arr));
                }
                Ok(serde_json::json!({
                    "ok": true,
                    "result": serde_json::Value::Object(results),
                }))
            }) {
                Ok(payload) => send_ok_value(&mut stream, payload),
                Err(e) => send_error(&mut stream, &e),
            }
            false
        }
        other => {
            send_error(&mut stream, &format!("unknown command: {other:?}"));
            false
        }
    }
}

fn repo_from_state(state: &Arc<Mutex<DaemonState>>) -> PathBuf {
    let s = state.lock().expect("daemon state lock poisoned");
    s.repo.clone()
}

fn open_daemon_store(repo: &Path) -> Result<ReadOnlyGraphStore, String> {
    GraphStore::open_read_only(repo).map_err(|e| e.to_string())
}

fn daemon_ping_payload(
    repo: &Path,
    store: &ReadOnlyGraphStore,
) -> Result<serde_json::Value, String> {
    let overview = store
        .overview_v2(OverviewV2Limits::default())
        .map_err(|e| e.to_string())?;
    let files = overview
        .repo
        .as_ref()
        .map(|repo| repo.file_count)
        .unwrap_or(0);
    let functions = store
        .functions_under_path("")
        .map_err(|e| e.to_string())?
        .len();
    let edges = store.edge_count().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "ok": true,
        "repo": repo.display().to_string(),
        "backend": "redb",
        "files": files,
        "functions": functions,
        "edges": edges,
    }))
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    ok: bool,
    error: &'a str,
}

fn send_error(stream: &mut UnixStream, message: &str) {
    let env = ErrorEnvelope {
        ok: false,
        error: message,
    };
    if let Ok(s) = serde_json::to_string(&env) {
        let _ = stream.write_all(s.as_bytes());
        let _ = stream.write_all(b"\n");
    }
}

fn send_ok_value(stream: &mut UnixStream, value: serde_json::Value) {
    if let Ok(s) = serde_json::to_string(&value) {
        let _ = stream.write_all(s.as_bytes());
        let _ = stream.write_all(b"\n");
    }
}

/// Send `{"ok": true, "result": <raw_json>}` without re-parsing `raw_json`.
fn send_ok_raw(stream: &mut UnixStream, raw_json: &str) {
    let _ = stream.write_all(br#"{"ok":true,"result":"#);
    let _ = stream.write_all(raw_json.as_bytes());
    let _ = stream.write_all(b"}\n");
}

/// Convenience client: send one JSON request to the daemon and read the
/// response line. Used by the CLI's `daemon status` and by tests.
pub fn send_request(socket: &Path, request: &serde_json::Value) -> Result<String, String> {
    let mut stream =
        UnixStream::connect(socket).map_err(|e| format!("connect {}: {}", socket.display(), e))?;
    let bytes = serde_json::to_vec(request).map_err(|e| format!("encode: {e}"))?;
    stream
        .write_all(&bytes)
        .map_err(|e| format!("write: {e}"))?;
    stream.write_all(b"\n").map_err(|e| format!("write: {e}"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| format!("shutdown write: {e}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| format!("read: {e}"))?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_is_in_aethyme_namespace() {
        let path = socket_path_for(Path::new("/some/repo/path"));
        let parent = path.parent().expect("socket has a parent");
        assert_eq!(
            parent.file_name().and_then(|n| n.to_str()),
            Some(SOCKET_DIR_NAME)
        );
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(SOCKET_PREFIX));
        assert!(name.ends_with(".sock"));
    }

    #[test]
    fn socket_path_is_stable_for_same_repo() {
        let a = socket_path_for(Path::new("/some/repo"));
        let b = socket_path_for(Path::new("/some/repo"));
        assert_eq!(a, b);
    }

    #[test]
    fn socket_path_differs_per_repo() {
        let a = socket_path_for(Path::new("/some/repo"));
        let b = socket_path_for(Path::new("/some/other-repo"));
        assert_ne!(a, b);
    }

    #[test]
    fn socket_path_can_use_explicit_socket_dir() {
        let dir = Path::new("/tmp/aethyme-codex-engine-sockets");
        let path = socket_path_in_dir(Path::new("/some/repo"), dir);
        assert_eq!(path.parent(), Some(dir));
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(SOCKET_PREFIX));
        assert!(name.ends_with(".sock"));
    }

    #[test]
    fn pidfile_and_logfile_paths_live_in_dotaethyme() {
        let pid = pidfile_path_for(Path::new("/some/repo"));
        assert_eq!(pid, Path::new("/some/repo/.aethyme/engine-daemon.pid"));
        let log = logfile_path_for(Path::new("/some/repo"));
        assert_eq!(log, Path::new("/some/repo/.aethyme/engine-daemon.log"));
    }
}
