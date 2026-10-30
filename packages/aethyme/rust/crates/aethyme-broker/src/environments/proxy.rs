//! The built-in wake-on-request proxy (#1214).
//!
//! The supervisor listens on loopback (`127.0.0.1:4180` by default,
//! `AETHYME_ENV_PROXY_PORT` overrides). A request is routed by its `Host`
//! header through the route table:
//!
//! - an **active** environment's component is forwarded to its reserved port;
//!   after the request head, bytes are copied both ways until either side
//!   closes, which also carries WebSocket upgrades;
//! - a **parked** environment is woken first and the request is **held** for
//!   at most its health timeouts, then answered with `503` naming the
//!   environment and `aethyme env logs` if the wake did not succeed;
//! - a **static** component's files are served directly, with no process.
//!
//! Routing is per connection: the first request's `Host` decides where the
//! whole connection goes.
//!
//! Control endpoints (for external proxies and scripts):
//!
//! - `GET /__aethyme/routes`: the route table as JSON, with an `ETag`
//!   (`If-None-Match` answers `304`);
//! - `POST /__aethyme/wake/<namespace>`: wake and answer once healthy (`200`)
//!   or failed (`503` with `Retry-After`). It runs repository commands, so it
//!   requires `Authorization: Bearer <token>`, the token in
//!   `<host state>/environments/proxy.token` (owner-only). It only acts on an
//!   existing environment: an unknown namespace is `404`, never a creation.
//!   Commands run are always the environment's recorded snapshot.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;

use super::lifecycle::Lifecycle;
use super::routes::{Route, etag, lookup, route_table};
use super::store::EnvironmentStore;
use crate::resources::HostResourceCoordinator;

pub const PROXY_PORT_ENV: &str = "AETHYME_ENV_PROXY_PORT";
pub const DEFAULT_PROXY_PORT: u16 = 4180;
pub const TOKEN_FILE: &str = "proxy.token";
const HEAD_LIMIT: usize = 64 * 1024;
const STATIC_LIMIT: u64 = 64 * 1024 * 1024;
const CONTROL_PREFIX: &str = "/__aethyme/";
/// Activity from proxied requests is recorded at most this often per
/// environment.
const TOUCH_DEBOUNCE: Duration = Duration::from_secs(1);

pub fn proxy_port() -> u16 {
    std::env::var(PROXY_PORT_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PROXY_PORT)
}

/// Read the wake token, creating it (owner-only) on first use.
pub fn ensure_token(root: &Path) -> io::Result<String> {
    let path = root.join(TOKEN_FILE);
    if let Ok(token) = std::fs::read_to_string(&path)
        && token.trim().len() == 64
    {
        return Ok(token.trim().to_string());
    }
    std::fs::create_dir_all(root)?;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    temporary.write_all(token.as_bytes())?;
    temporary.persist(&path).map_err(|error| error.error)?;
    crate::host_state::protect_host_state_path(&path, false)?;
    Ok(token)
}

pub struct ProxyConfig {
    pub root: PathBuf,
    pub store_path: PathBuf,
    pub host_db: PathBuf,
    pub token: String,
    /// Recorded as the lease holder when the proxy wakes an environment.
    pub holder_pid: Option<u32>,
}

impl ProxyConfig {
    fn routes(&self) -> Vec<Route> {
        match EnvironmentStore::open_read_only(&self.store_path) {
            Ok(Some(store)) => route_table(&store, &self.root).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn lifecycle(&self) -> Result<Lifecycle, String> {
        Ok(Lifecycle {
            store: EnvironmentStore::open(&self.store_path).map_err(|e| e.to_string())?,
            coordinator: HostResourceCoordinator::open(&self.host_db).map_err(|e| e.to_string())?,
            root: self.root.clone(),
            holder_pid: self.holder_pid,
            hook_timeout: super::exec::DEFAULT_HOOK_TIMEOUT,
            actor: "proxy".into(),
        })
    }
}

/// Accept connections until `stop` is set. Each connection gets a thread.
pub fn serve(
    listener: TcpListener,
    config: Arc<ProxyConfig>,
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                // Accepted sockets inherit the listener's non-blocking mode on
                // macOS; handlers read and copy with blocking I/O.
                if let Err(error) = stream.set_nonblocking(false) {
                    crate::warn_unrecorded(
                        "make a proxied connection blocking",
                        Err::<(), _>(error),
                    );
                    continue;
                }
                let config = config.clone();
                std::thread::spawn(move || {
                    crate::warn_unrecorded("serve a proxied connection", handle(stream, &config));
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

struct Head {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    /// Everything read so far: the head and any body bytes after it.
    raw: Vec<u8>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn read_head(stream: &mut TcpStream) -> io::Result<Option<Head>> {
    let mut raw = Vec::with_capacity(4096);
    let mut buffer = [0u8; 4096];
    let end = loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(None);
        }
        raw.extend_from_slice(&buffer[..read]);
        if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
        if raw.len() > HEAD_LIMIT {
            return Ok(None);
        }
    };
    let text = String::from_utf8_lossy(&raw[..end]).into_owned();
    let mut lines = text.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split_whitespace();
    let method = request.next().unwrap_or_default().to_string();
    let path = request.next().unwrap_or("/").to_string();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Ok(Some(Head {
        method,
        path,
        headers,
        raw,
    }))
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    extra: &[(&str, String)],
    body: &[u8],
) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn respond_json(
    stream: &mut TcpStream,
    status: &str,
    value: serde_json::Value,
    extra: &[(&str, String)],
) -> io::Result<()> {
    respond(
        stream,
        status,
        "application/json",
        extra,
        value.to_string().as_bytes(),
    )
}

/// One environment wakes at a time; concurrent requests for it wait on the
/// same lock and then find it active.
fn wake_lock(env_id: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.entry(env_id.to_string()).or_default().clone()
}

fn touch_debounced(root: &Path, env_id: &str) {
    static LAST: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let last = LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = last.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let due = guard
        .get(env_id)
        .is_none_or(|at| at.elapsed() >= TOUCH_DEBOUNCE);
    if due {
        guard.insert(env_id.to_string(), Instant::now());
        crate::warn_unrecorded(
            "record proxied activity",
            super::activity::touch(root, env_id),
        );
    }
}

/// Wake `env_id` if it is parked. `Err` carries the reason for the 503.
/// The environment's current phase, read fresh from the store.
fn phase_of(config: &ProxyConfig, env_id: &str) -> Option<&'static str> {
    let store = EnvironmentStore::open_read_only(&config.store_path).ok()??;
    store.get(env_id).ok()?.map(|record| record.phase.as_str())
}

/// Hold the request while the environment is mid-transition: a redeploy
/// swap in progress (#1217), or stopping, waking or starting. Returns the
/// phase it settled in, or `None` if it did not settle in time.
fn settle(config: &ProxyConfig, route: &Route) -> Option<&'static str> {
    let deadline = Instant::now() + Duration::from_secs(route.wake_timeout_seconds);
    loop {
        let swapping = super::shared::swap_in_progress(&config.root, &route.env_id);
        let phase = phase_of(config, &route.env_id)?;
        if !swapping && !matches!(phase, "stopping" | "waking" | "starting" | "provisioning") {
            return Some(phase);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn ensure_awake(config: &ProxyConfig, route: &Route) -> Result<(), String> {
    let state = if route.state == "active"
        && !super::shared::swap_in_progress(&config.root, &route.env_id)
    {
        "active"
    } else {
        settle(config, route).ok_or_else(|| {
            format!(
                "environment {} did not settle within {}s",
                route.env_id, route.wake_timeout_seconds
            )
        })?
    };
    if state == "active" {
        return Ok(());
    }
    if state != "parked" {
        return Err(format!("environment {} is {state}", route.env_id));
    }
    let lock = wake_lock(&route.env_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut lifecycle = config.lifecycle()?;
    lifecycle
        .wake(
            &route.env_id,
            Duration::from_secs(route.wake_timeout_seconds),
        )
        .map(|_| ())
        .map_err(|error| format!("{} ({})", error, error.code()))
}

fn handle(mut client: TcpStream, config: &ProxyConfig) -> io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(30)))?;
    let Some(head) = read_head(&mut client)? else {
        return Ok(());
    };
    if let Some(control) = head.path.strip_prefix(CONTROL_PREFIX) {
        return control_request(&mut client, config, &head, control);
    }
    let host = head.header("Host").unwrap_or_default().to_string();
    let routes = config.routes();
    let Some(route) = lookup(&routes, &host).cloned() else {
        return respond(
            &mut client,
            "404 Not Found",
            "text/plain; charset=utf-8",
            &[],
            format!("no environment route for {host:?}; see `aethyme env routes`\n").as_bytes(),
        );
    };
    if let Err(reason) = ensure_awake(config, &route) {
        return respond_json(
            &mut client,
            "503 Service Unavailable",
            json!({
                "env_id": route.env_id,
                "component": route.component,
                "error": reason,
                "next_action": format!("aethyme env logs {}", route.env_id),
            }),
            &[("Retry-After", "5".into())],
        );
    }
    touch_debounced(&config.root, &route.env_id);
    // Re-read: waking may have changed the phase (and so the upstream).
    let routes = config.routes();
    let Some(route) = lookup(&routes, &host).cloned() else {
        return respond(
            &mut client,
            "503 Service Unavailable",
            "text/plain",
            &[],
            b"route vanished\n",
        );
    };
    if let Some(directory) = &route.static_dir {
        return serve_static(&mut client, &head, directory, route.spa_fallback);
    }
    let Some(port) = route.upstream else {
        return respond(
            &mut client,
            "503 Service Unavailable",
            "text/plain",
            &[("Retry-After", "5".into())],
            b"environment is not active\n",
        );
    };
    let mut upstream = TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(5),
    )?;
    client.set_read_timeout(None)?;
    upstream.write_all(&head.raw)?;
    splice(client, upstream)
}

/// Copy bytes both ways until each side closes.
fn splice(client: TcpStream, upstream: TcpStream) -> io::Result<()> {
    let (mut client_read, mut upstream_write) = (client.try_clone()?, upstream.try_clone()?);
    let forward = std::thread::spawn(move || {
        let copied = io::copy(&mut client_read, &mut upstream_write);
        crate::warn_unrecorded(
            "half-close the upstream",
            upstream_write.shutdown(Shutdown::Write),
        );
        copied
    });
    let (mut upstream_read, mut client_write) = (upstream, client);
    let backward = io::copy(&mut upstream_read, &mut client_write);
    crate::warn_unrecorded(
        "half-close the client",
        client_write.shutdown(Shutdown::Write),
    );
    match forward.join() {
        Ok(result) => result.map(|_| ())?,
        Err(_) => return Err(io::Error::other("proxy forward thread panicked")),
    }
    backward.map(|_| ())
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn serve_static(
    client: &mut TcpStream,
    head: &Head,
    directory: &Path,
    spa_fallback: bool,
) -> io::Result<()> {
    if head.method != "GET" && head.method != "HEAD" {
        return respond(
            client,
            "405 Method Not Allowed",
            "text/plain",
            &[("Allow", "GET, HEAD".into())],
            b"",
        );
    }
    let relative = head.path.split(['?', '#']).next().unwrap_or("/");
    let relative = Path::new(relative.trim_start_matches('/'));
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return respond(client, "404 Not Found", "text/plain", &[], b"not found\n");
    }
    let mut path = directory.join(relative);
    if path.is_dir() {
        path = path.join("index.html");
    }
    let found = servable(directory, &path).or_else(|| {
        // A client-side route: no extension, so not a missing asset.
        (spa_fallback && relative.extension().is_none())
            .then(|| servable(directory, &directory.join("index.html")))
            .flatten()
    });
    let Some((path, size)) = found else {
        return respond(client, "404 Not Found", "text/plain", &[], b"not found\n");
    };
    let body = std::fs::read(&path)?;
    let body: &[u8] = if head.method == "HEAD" { &[] } else { &body };
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n",
        content_type(&path)
    )
    .into_bytes();
    response.extend_from_slice(body);
    client.write_all(&response)?;
    client.flush()
}

/// `path` and its size when it is a file inside `directory` (symlinks
/// resolved) small enough to serve.
fn servable(directory: &Path, path: &Path) -> Option<(PathBuf, u64)> {
    let inside = path
        .canonicalize()
        .ok()
        .zip(directory.canonicalize().ok())
        .is_some_and(|(file, root)| file.starts_with(root));
    let metadata = std::fs::metadata(path).ok()?;
    (inside && metadata.is_file() && metadata.len() <= STATIC_LIMIT)
        .then(|| (path.to_path_buf(), metadata.len()))
}

fn authorized(head: &Head, token: &str) -> bool {
    let Some(presented) = head
        .header("Authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    // Constant time over the expected length.
    presented.len() == token.len()
        && presented
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn control_request(
    client: &mut TcpStream,
    config: &ProxyConfig,
    head: &Head,
    control: &str,
) -> io::Result<()> {
    match (
        head.method.as_str(),
        control.split_once('/').unwrap_or((control, "")),
    ) {
        ("GET", ("routes", "")) => {
            let routes = config.routes();
            let tag = etag(&routes);
            if head.header("If-None-Match") == Some(tag.as_str()) {
                return respond(
                    client,
                    "304 Not Modified",
                    "application/json",
                    &[("ETag", tag)],
                    b"",
                );
            }
            respond_json(
                client,
                "200 OK",
                json!({ "schema": "aethyme.env-routes/v1", "etag": tag, "routes": routes }),
                &[("ETag", tag.clone())],
            )
        }
        ("POST", ("wake", namespace)) => {
            if !authorized(head, &config.token) {
                return respond_json(
                    client,
                    "401 Unauthorized",
                    json!({"error": "missing or wrong bearer token"}),
                    &[],
                );
            }
            let routes = config.routes();
            let Some(route) = routes.iter().find(|r| r.namespace == namespace) else {
                return respond_json(
                    client,
                    "404 Not Found",
                    json!({"error": format!("no environment with namespace {namespace:?}")}),
                    &[],
                );
            };
            match ensure_awake(config, route) {
                Ok(()) => {
                    touch_debounced(&config.root, &route.env_id);
                    respond_json(
                        client,
                        "200 OK",
                        json!({"env_id": route.env_id, "state": "active"}),
                        &[],
                    )
                }
                Err(reason) => respond_json(
                    client,
                    "503 Service Unavailable",
                    json!({"env_id": route.env_id, "error": reason, "next_action": format!("aethyme env logs {}", route.env_id)}),
                    &[("Retry-After", "5".into())],
                ),
            }
        }
        _ => respond(
            client,
            "404 Not Found",
            "text/plain",
            &[],
            b"unknown control endpoint\n",
        ),
    }
}
