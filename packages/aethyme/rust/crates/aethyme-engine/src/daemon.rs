//! Engine-side daemon: keeps `RepositoryMap` resident across calls.
//!
//! Why this exists
//! ---------------
//! Profiling on MediaWiki (commits 387b8d8, 5537f40, 7d4c3bc, e17ed5a) showed
//! the 14-second steady-state cost of `aethyme explore` is dominated by
//! per-call setup that has nothing to do with the user's request:
//!
//!   - `RepositoryMap` construction reconstructs the in-memory graph on
//!     every invocation (~70-130s on cold legacy builds; much lower when
//!     committed fragments are available).
//!   - The Python wrapper spawns the engine binary as a subprocess for
//!     `task-localize`, `task-anchors`, `symbol-batch`, etc. Each spawn
//!     pays the build cost from scratch.
//!
//! This module hosts an in-process daemon that builds the map ONCE on
//! startup, holds it in memory behind an `RwLock`, and serves queries
//! over a Unix domain socket. First call pays the full cost; every
//! subsequent call drops to the algorithmic minimum (5-50ms for typical
//! task-localize on a 12K-file repo with the map resident).
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
//! Single-threaded request handler for v1. `RepositoryMap` is wrapped in
//! `Arc<RwLock<...>>` so future thread-pool variants are a drop-in. At
//! typical agent rates (a few req/s), serial dispatch is fine.
//!
//! Lifecycle
//! ---------
//! - Daemon binds on start, builds map (~70s on MediaWiki), then listens.
//! - Idle watcher exits the daemon after `idle_timeout` of inactivity.
//! - Explicit `shutdown` command terminates the loop and removes the socket.
//! - Map is NEVER invalidated automatically — caller must restart the
//!   daemon to pick up filesystem changes. Future work: mtime-based revalidate.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::graph::navigation::{callers_view, task_anchors_view, task_next_view, task_scope_view};
use crate::graph::search::symbol_search;
use crate::map::RepositoryMap;
use crate::model::task::TaskInput;

const SOCKET_PREFIX: &str = "engine-";
const SOCKET_DIR_NAME: &str = "aethyme";
pub const DEFAULT_IDLE_TIMEOUT_SECONDS: u64 = 1800; // 30 min

/// Compute the unix socket path for `repo`.
///
/// macOS caps AF_UNIX paths at ~104 bytes, so the socket lives in `$TMPDIR`
/// keyed by a stable hash of the resolved repo path. The Python daemon's
/// socket lives at the same prefix with a `daemon-` filename — separate
/// namespaces so both daemons can coexist.
pub fn socket_path_for(repo: &Path) -> PathBuf {
    let canonical = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let tmp = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    tmp.join(SOCKET_DIR_NAME)
        .join(format!("{SOCKET_PREFIX}{hex}.sock"))
}

/// Compute the pidfile path inside the repo's `.aethyme/` directory.
pub fn pidfile_path_for(repo: &Path) -> PathBuf {
    repo.join(".aethyme").join("engine-daemon.pid")
}

/// Compute the logfile path inside the repo's `.aethyme/` directory.
pub fn logfile_path_for(repo: &Path) -> PathBuf {
    repo.join(".aethyme").join("engine-daemon.log")
}

/// Server state held across requests.
struct DaemonState {
    repo: PathBuf,
    map: RepositoryMap,
    last_activity: Instant,
}

/// Configurable parameters for daemon lifecycle.
pub struct DaemonConfig {
    pub repo: PathBuf,
    pub idle_timeout: Duration,
    pub no_cache: bool,
    pub use_fragments: bool,
    pub force_fragments: bool,
}

impl DaemonConfig {
    pub fn new(repo: PathBuf) -> Self {
        Self {
            repo,
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECONDS),
            no_cache: false,
            use_fragments: true,
            force_fragments: false,
        }
    }

    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    pub fn with_no_cache(mut self, no_cache: bool) -> Self {
        self.no_cache = no_cache;
        self
    }

    pub fn with_use_fragments(mut self, use_fragments: bool) -> Self {
        self.use_fragments = use_fragments;
        self
    }

    pub fn with_force_fragments(mut self, force_fragments: bool) -> Self {
        self.force_fragments = force_fragments;
        self
    }
}

/// Run the daemon for `config.repo` until idle timeout or `shutdown` request.
///
/// Returns when the listen loop exits cleanly (idle, shutdown, or socket
/// error). Build errors during the initial `RepositoryMap::build` are
/// returned immediately without ever opening the socket.
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
        "aethyme-engine-daemon: building map for {} ...",
        config.repo.display()
    );
    let build_started = Instant::now();
    if !config.use_fragments {
        return Err(
            "legacy pass pipeline was deleted in 4.7.12; daemon builds require .aethyme/graph fragments"
                .to_string(),
        );
    }

    let (map, build_profile) = if config.force_fragments {
        RepositoryMap::build_from_fragments(&config.repo)
            .map_err(|e| format!("build_from_fragments: {e}"))?
    } else {
        RepositoryMap::build_with_fragment_preference(&config.repo, config.no_cache, |_| {})
            .map_err(|e| format!("build_with_fragment_preference: {e}"))?
    };
    let build_mode = if build_profile
        .stages
        .iter()
        .any(|stage| stage.name == "populate_from_fragments")
    {
        "fragments"
    } else {
        "fragments"
    };
    eprintln!(
        "aethyme-engine-daemon: map ready ({} files, {} functions, mode {}, build {:?})",
        map.files.len(),
        map.functions.len(),
        build_mode,
        build_started.elapsed()
    );

    // Pre-warm GraphSignals so the first task-localize call doesn't pay
    // the ~17s parser_visibility O(F·E) sweep. Without this, call 1 is
    // slow even though calls 2+ are fast — bad UX for any agent that
    // makes only one or two queries before moving on.
    let signals_started = Instant::now();
    let _ = map.signals();
    eprintln!(
        "aethyme-engine-daemon: signals warm (took {:?})",
        signals_started.elapsed()
    );

    let state = Arc::new(Mutex::new(DaemonState {
        repo: config.repo.clone(),
        map,
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
            let s = state.lock().expect("daemon state lock poisoned");
            send_ok_value(
                &mut stream,
                serde_json::json!({
                    "ok": true,
                    "repo": s.repo.display().to_string(),
                    "files": s.map.files.len(),
                    "functions": s.map.functions.len(),
                    "edges": s.map.edges.len(),
                }),
            );
            false
        }
        "shutdown" => {
            send_ok_value(&mut stream, serde_json::json!({"ok": true}));
            true
        }
        "task-localize" => {
            let task_text = request.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let s = state.lock().expect("daemon state lock poisoned");
            let task = TaskInput::from_task_text(task_text);
            let anchors = task_anchors_view(&s.map, &task);
            let scope = task_scope_view(&s.map, &task);
            let next = task_next_view(&s.map, &task);
            let view = crate::json::task_localization_view(&anchors, &scope, &next);
            send_ok_raw(&mut stream, &view);
            false
        }
        "symbol-batch" => {
            // Run symbol_search against the resident map for each query.
            // Returns a JSON object keyed by query: {query → [SearchHit, ...]}.
            // Mirrors the `aethyme-engine-cli symbol-batch` shape so the
            // Python wrapper can route through the daemon transparently.
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

            let s = state.lock().expect("daemon state lock poisoned");
            let mut results: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
            for query in &queries {
                let hits = symbol_search(&s.map, query, limit);
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
            send_ok_value(
                &mut stream,
                serde_json::json!({
                    "ok": true,
                    "result": serde_json::Value::Object(results),
                }),
            );
            false
        }
        "callers-of" => {
            // Look up the callers of one or more symbols against the
            // resident graph. Returns map keyed by symbol id:
            // `{symbol_id → [{ "id": ..., "label": ..., "path": ... },
            // ...]}`. Symbols not found in the graph map to empty
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
            let s = state.lock().expect("daemon state lock poisoned");
            let mut results: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
            for target in &targets {
                let view = callers_view(&s.map, target);
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
            send_ok_value(
                &mut stream,
                serde_json::json!({
                    "ok": true,
                    "result": serde_json::Value::Object(results),
                }),
            );
            false
        }
        other => {
            send_error(&mut stream, &format!("unknown command: {other:?}"));
            false
        }
    }
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
    fn pidfile_and_logfile_paths_live_in_dotaethyme() {
        let pid = pidfile_path_for(Path::new("/some/repo"));
        assert_eq!(pid, Path::new("/some/repo/.aethyme/engine-daemon.pid"));
        let log = logfile_path_for(Path::new("/some/repo"));
        assert_eq!(log, Path::new("/some/repo/.aethyme/engine-daemon.log"));
    }
}
