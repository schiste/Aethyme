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
//! whole connection goes. A route with `paths` (#1296) sends a request whose
//! path is under one of them (longest prefix wins, the prefix is kept) to
//! that path's target instead: another route of the table, held and woken
//! like a host route, or an external `http://` URL, whose `Host` header the
//! proxy rewrites. Upgrades are spliced the same way, so `/ws` works.
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
use super::routes::{Route, etag, lookup, route_table, select_path};
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
    fn routes(&self) -> Result<Vec<Route>, String> {
        match EnvironmentStore::open_read_only(&self.store_path) {
            Ok(Some(store)) => route_table(&store, &self.root).map_err(|error| error.to_string()),
            Ok(None) => Ok(Vec::new()),
            Err(error) => Err(error.to_string()),
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

fn route_snapshot_unavailable(client: &mut TcpStream, reason: &str) -> io::Result<()> {
    respond_json(
        client,
        "503 Service Unavailable",
        json!({
            "error": reason,
            "next_action": "inspect `aethyme env routes` and retry after the shared redeploy settles"
        }),
        &[("Retry-After", "1".into())],
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

/// Where one request goes once its host's route is known.
enum Target {
    Route(Route),
    Url(String),
}

/// The target for `path` on `route`: the longest matching `paths` entry, or
/// the route itself. `Err` names a path target missing from the table.
fn target(routes: &[Route], route: &Route, path: &str) -> Result<Target, String> {
    let Some(entry) = select_path(route, path) else {
        return Ok(Target::Route(route.clone()));
    };
    if let Some(url) = &entry.url {
        return Ok(Target::Url(url.clone()));
    }
    let host = entry.host.as_deref().unwrap_or_default();
    lookup(routes, host)
        .cloned()
        .map(Target::Route)
        .ok_or_else(|| {
            format!(
                "path {} of {} routes to {host}, which has no route; is it up?",
                entry.prefix, route.host
            )
        })
}

fn path_target_unavailable(client: &mut TcpStream, route: &Route, reason: &str) -> io::Result<()> {
    respond_json(
        client,
        "503 Service Unavailable",
        json!({
            "env_id": route.env_id,
            "component": route.component,
            "error": reason,
            "next_action": "aethyme env routes",
        }),
        &[("Retry-After", "5".into())],
    )
}

/// The request head with its `Host` header replaced by `host`.
fn with_host(raw: &[u8], host: &str) -> Vec<u8> {
    let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return raw.to_vec();
    };
    let head = String::from_utf8_lossy(&raw[..end]);
    let mut out = String::new();
    for (index, line) in head.split("\r\n").enumerate() {
        let is_host = index > 0
            && line
                .split_once(':')
                .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("host"));
        if is_host {
            out.push_str(&format!("Host: {host}"));
        } else {
            out.push_str(line);
        }
        out.push_str("\r\n");
    }
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&raw[end + 2..]);
    bytes
}

/// Forward the request to an external `http://host[:port]` (#1296).
fn forward_url(client: TcpStream, head: &Head, url: &str) -> io::Result<()> {
    let authority = url
        .strip_prefix("http://")
        .unwrap_or(url)
        .trim_end_matches('/');
    let address = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:80")
    };
    let addr = std::net::ToSocketAddrs::to_socket_addrs(&address)?
        .next()
        .ok_or_else(|| io::Error::other(format!("{address} does not resolve")))?;
    let mut upstream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    client.set_read_timeout(None)?;
    upstream.write_all(&with_host(&head.raw, authority))?;
    splice(client, upstream)
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
    let routes = match config.routes() {
        Ok(routes) => routes,
        Err(reason) => return route_snapshot_unavailable(&mut client, &reason),
    };
    let Some(host_route) = lookup(&routes, &host).cloned() else {
        return respond(
            &mut client,
            "404 Not Found",
            "text/plain; charset=utf-8",
            &[],
            format!("no environment route for {host:?}; see `aethyme env routes`\n").as_bytes(),
        );
    };
    let route = match target(&routes, &host_route, &head.path) {
        Ok(Target::Route(route)) => route,
        Ok(Target::Url(url)) => {
            touch_debounced(&config.root, &host_route.env_id);
            return forward_url(client, &head, &url);
        }
        Err(reason) => return path_target_unavailable(&mut client, &host_route, &reason),
    };
    // A redeploy can stop the upstream at any point below, between this check
    // and the upstream's first response byte. Each lost attempt starts again
    // here, where the request is held until the environment has settled.
    let mut route = route;
    let mut attempt = 1;
    loop {
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
        // Re-read just before connecting: waking or a blue/green flip may have
        // changed the selected slot and its upstream.
        let routes = match config.routes() {
            Ok(routes) => routes,
            Err(reason) => return route_snapshot_unavailable(&mut client, &reason),
        };
        let Some(host_route) = lookup(&routes, &host).cloned() else {
            return respond(
                &mut client,
                "503 Service Unavailable",
                "text/plain",
                &[],
                b"route vanished\n",
            );
        };
        route = match target(&routes, &host_route, &head.path) {
            Ok(Target::Route(route)) => route,
            Ok(Target::Url(url)) => return forward_url(client, &head, &url),
            Err(reason) => return path_target_unavailable(&mut client, &host_route, &reason),
        };
        touch_debounced(&config.root, &route.env_id);
        if host_route.env_id != route.env_id {
            touch_debounced(&config.root, &host_route.env_id);
        }
        if let Some(directory) = &route.static_dir {
            return serve_static(&mut client, &head, directory, route.spa_fallback);
        }
        let Some(port) = route.upstream else {
            if attempt < UPSTREAM_ATTEMPTS {
                attempt += 1;
                continue;
            }
            return respond(
                &mut client,
                "503 Service Unavailable",
                "text/plain",
                &[("Retry-After", "5".into())],
                b"environment is not active\n",
            );
        };
        match open_upstream(port, &head)? {
            Upstream::Ready(upstream) => {
                client.set_read_timeout(None)?;
                return splice(client, upstream);
            }
            Upstream::Lost(_) if attempt < UPSTREAM_ATTEMPTS => attempt += 1,
            Upstream::Lost(error) => return Err(error),
        }
    }
}

/// Tries for one request. A redeploy can cost two: one to a server it is
/// stopping, and one to a route read before the swap began.
const UPSTREAM_ATTEMPTS: usize = 3;

/// How long a replayable request waits to see the upstream answer before it is
/// committed to that upstream. A slower response streams normally, but cannot be
/// replayed if the upstream stops before it starts.
const FIRST_BYTE_WINDOW: Duration = Duration::from_secs(5);

enum Upstream {
    /// Connected, and the request is sent.
    Ready(TcpStream),
    /// Gone with no response, and the request is safe to send again.
    Lost(io::Error),
}

/// Whether a request can be sent twice: it has no body, and its method promises
/// no effect. This is the only case where a lost upstream can be hidden from the
/// client, since a server may have acted on anything else before it stopped.
fn replayable(head: &Head) -> bool {
    matches!(head.method.as_str(), "GET" | "HEAD")
        && head.header("Content-Length").is_none()
        && head.header("Transfer-Encoding").is_none()
}

/// Connect to `port` and send the request. Nothing has been sent when a connection
/// fails, so every request may try again after that. A replayable request also
/// waits for the first response byte, because a server stopped mid-request
/// resets the connection and the client would otherwise see that reset.
fn open_upstream(port: u16, head: &Head) -> io::Result<Upstream> {
    let mut upstream = match TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(5),
    ) {
        Ok(upstream) => upstream,
        Err(error) => return Ok(Upstream::Lost(error)),
    };
    if !replayable(head) {
        upstream.write_all(&head.raw)?;
        return Ok(Upstream::Ready(upstream));
    }
    if let Err(error) = upstream.write_all(&head.raw) {
        return Ok(Upstream::Lost(error));
    }
    // macOS refuses a socket option once the peer has closed, even with its
    // response waiting to be read: neither call's failure says the upstream is
    // lost. Only the peek decides, and a closed socket's reads cannot block.
    let _ = upstream.set_read_timeout(Some(FIRST_BYTE_WINDOW));
    let first = upstream.peek(&mut [0u8; 1]);
    let _ = upstream.set_read_timeout(None);
    Ok(match first {
        Ok(0) => Upstream::Lost(io::ErrorKind::UnexpectedEof.into()),
        Ok(_) => Upstream::Ready(upstream),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Upstream::Ready(upstream)
        }
        Err(error) => Upstream::Lost(error),
    })
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
        // Only absent client routes may fall back. An existing but
        // ineligible path must keep its refusal.
        (spa_fallback && relative.extension().is_none() && missing_static_path(directory, &path))
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

/// Confirm absence beneath an existing directory inside the served root.
/// Walking up also handles routes whose intermediate directories are absent;
/// a dangling link, unreadable path or an ancestor outside the root refuses.
fn missing_static_path(directory: &Path, path: &Path) -> bool {
    let Ok(root) = directory.canonicalize() else {
        return false;
    };
    let mut missing = false;
    for ancestor in path.ancestors() {
        if !ancestor.starts_with(directory) {
            return false;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                return missing
                    && ancestor.canonicalize().ok().is_some_and(|resolved| {
                        resolved.starts_with(&root)
                            && std::fs::metadata(resolved).is_ok_and(|metadata| metadata.is_dir())
                    });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => missing = true,
            Err(_) => return false,
        }
    }
    false
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
            let routes = match config.routes() {
                Ok(routes) => routes,
                Err(reason) => return route_snapshot_unavailable(client, &reason),
            };
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
            let routes = match config.routes() {
                Ok(routes) => routes,
                Err(reason) => return route_snapshot_unavailable(client, &reason),
            };
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

#[cfg(test)]
mod upstream_tests {
    use super::*;

    fn head(method: &str, headers: &[(&str, &str)]) -> Head {
        Head {
            method: method.into(),
            path: "/".into(),
            headers: headers
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
            raw: format!("{method} / HTTP/1.1\r\n\r\n").into_bytes(),
        }
    }

    /// A server that reads the request, then does what `then` says.
    fn server(then: fn(TcpStream)) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 256];
            stream.read(&mut request).unwrap();
            then(stream);
        });
        port
    }

    #[test]
    fn a_server_that_stops_before_answering_can_be_asked_again() {
        let port = server(drop);
        assert!(matches!(
            open_upstream(port, &head("GET", &[])).unwrap(),
            Upstream::Lost(_)
        ));
    }

    #[test]
    fn a_server_that_answers_is_kept_with_its_response_unread() {
        let port = server(|mut stream| stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap());
        let Upstream::Ready(mut upstream) = open_upstream(port, &head("GET", &[])).unwrap() else {
            panic!("the server answered");
        };
        let mut response = String::new();
        upstream.read_to_string(&mut response).unwrap();
        assert_eq!(response, "HTTP/1.1 200 OK\r\n\r\n");
    }

    /// The server may have acted on these before it stopped, so they are never
    /// sent twice, and the proxy must not wait for an answer before streaming a
    /// body the server may be waiting for.
    #[test]
    fn a_request_that_can_change_state_is_never_held_for_a_replay() {
        for request in [
            head("POST", &[]),
            head("DELETE", &[]),
            head("GET", &[("content-length", "3")]),
            head("GET", &[("Transfer-Encoding", "chunked")]),
        ] {
            let port = server(drop);
            assert!(matches!(
                open_upstream(port, &request).unwrap(),
                Upstream::Ready(_)
            ));
        }
    }

    #[test]
    fn a_refused_connection_sent_nothing_so_any_request_may_try_again() {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        assert!(matches!(
            open_upstream(port, &head("POST", &[])).unwrap(),
            Upstream::Lost(_)
        ));
    }
}
