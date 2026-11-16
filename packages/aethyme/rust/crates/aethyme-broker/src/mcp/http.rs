//! `transport = "http"`: bridge an agent's stdio to a streamable-HTTP MCP
//! server on loopback.
//!
//! Only what the bridge needs from the MCP streamable-HTTP transport is
//! implemented: each stdin message is POSTed (`Accept: application/json,
//! text/event-stream`); a JSON reply or every `data:` event of an SSE reply
//! goes to stdout; `202 Accepted` carries nothing. The `Mcp-Session-Id` the
//! server returns on `initialize` (and the negotiated `MCP-Protocol-Version`)
//! are sent with every later request, so each bridge is one HTTP session
//! and no ids are rewritten. After `initialize` a `GET` stream is opened for
//! server-initiated messages when the server offers one (405 means none).
//! At the end the session is ended with `DELETE`.
//!
//! The server is reached over plain HTTP/1.1 on `127.0.0.1` only, one
//! connection per request (`Connection: close`), with `Authorization:
//! Bearer` when the entry has a token and never an `Origin` header. Every
//! reply read has an idle deadline, and header lines, bodies and SSE events
//! are bounded by [`MAX_MESSAGE_BYTES`]. When the server ends the session
//! (404 or 401 once initialized) or the caller signals that the server is
//! gone, every request still waiting is answered with a retryable error and
//! the bridge ends at once, so the agent sees its server end.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::config::HttpAuth;
use super::jsonrpc::{self, Kind, MAX_MESSAGE_BYTES, read_message};
use super::key::Plan;
use super::listen::{self, Listeners};

/// The path an HTTP entry is served on unless it sets `http_path`.
pub const DEFAULT_PATH: &str = "/mcp";
/// The variable an HTTP upstream finds its port in (besides `{port}` in
/// its argv).
pub const PORT_ENV: &str = "AETHYME_MCP_PORT";
/// How long a reply may stay silent before the request fails retryably.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// How long a server may take to listen on its port.
pub const LISTEN_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const READY_POLL: Duration = Duration::from_millis(100);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_IN_FLIGHT: usize = 64;
const MAX_LINE: usize = 64 * 1024;
const MAX_HEADERS: usize = 128;

/// `argv` with every `{port}` replaced.
pub fn with_port(argv: &[String], port: u16) -> Vec<String> {
    argv.iter()
        .map(|element| element.replace("{port}", &port.to_string()))
        .collect()
}

/// The loopback URL of a server on `port`.
pub fn url(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}{path}")
}

/// Where a loopback URL points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub address: SocketAddr,
    pub path: String,
}

/// Parse `http://127.0.0.1:<port><path>`; anything else is refused.
pub fn parse_url(url: &str) -> Option<Target> {
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    let (port, path) = match rest.find('/') {
        Some(slash) => (&rest[..slash], &rest[slash..]),
        None => (rest, "/"),
    };
    Some(Target {
        address: SocketAddr::from((Ipv4Addr::LOCALHOST, port.parse::<u16>().ok()?)),
        path: path.to_string(),
    })
}

/// A free loopback port, for a private server (the OS picks it).
pub fn free_port() -> io::Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

/// Whether `port` can be bound on loopback right now (bound and released).
pub fn port_free(port: u16) -> bool {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
}

/// A fresh random bearer token (hex).
pub fn new_token() -> io::Result<String> {
    Ok(super::secret::random_bytes()?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// What a server needs for `auth` and `token`: extra argv elements and an
/// extra environment variable.
pub fn token_delivery(
    auth: &HttpAuth,
    token: Option<&str>,
) -> (Vec<String>, Option<(String, String)>) {
    match (auth, token) {
        (HttpAuth::BearerArg { flag }, Some(token)) => {
            (vec![flag.clone(), token.to_string()], None)
        }
        (HttpAuth::BearerEnv { name }, Some(token)) => {
            (Vec::new(), Some((name.clone(), token.to_string())))
        }
        _ => (Vec::new(), None),
    }
}

/// Wait until every listener on `port` is `group`'s own, on loopback.
/// `Err(reason)` when the server exits, listens elsewhere, someone else
/// holds the port, ownership cannot be determined, or time runs out.
pub fn wait_owned(
    port: u16,
    group: u32,
    child: &mut Child,
    within: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + within;
    loop {
        match listen::check(port, group) {
            Listeners::Ours => return Ok(()),
            Listeners::None => {}
            verdict => return Err(verdict.reason().unwrap_or_default()),
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("exited_before_listening:{status}"));
        }
        if Instant::now() >= deadline {
            return Err("listen_timeout".into());
        }
        thread::sleep(READY_POLL);
    }
}

fn too_large(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{what} too large"))
}

/// One line of at most [`MAX_LINE`] bytes.
fn read_line_capped(reader: &mut impl BufRead, line: &mut String) -> io::Result<usize> {
    let read = reader.by_ref().take(MAX_LINE as u64 + 1).read_line(line)?;
    if read > MAX_LINE {
        return Err(too_large("an HTTP line"));
    }
    Ok(read)
}

/// A chunked transfer-encoding body.
struct Chunked<R> {
    inner: R,
    left: usize,
    done: bool,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.done || buffer.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut line = String::new();
            read_line_capped(&mut self.inner, &mut line)?;
            let size = line.trim().split(';').next().unwrap_or_default();
            self.left = usize::from_str_radix(size, 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if self.left == 0 {
                // Trailers, up to the empty line.
                for _ in 0..MAX_HEADERS {
                    let mut trailer = String::new();
                    if read_line_capped(&mut self.inner, &mut trailer)? == 0
                        || trailer.trim().is_empty()
                    {
                        break;
                    }
                }
                self.done = true;
                return Ok(0);
            }
        }
        let wanted = buffer.len().min(self.left);
        let read = self.inner.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "chunk cut short",
            ));
        }
        self.left -= read;
        if self.left == 0 {
            let mut end = String::new();
            read_line_capped(&mut self.inner, &mut end)?;
        }
        Ok(read)
    }
}

/// A response: status, headers (lower-case names) and a streaming body.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Box<dyn BufRead + Send>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One HTTP/1.1 request on a fresh connection. `idle` bounds every read of
/// the reply (`None`: no bound, for a long-lived event stream).
pub fn request(
    target: &Target,
    method: &str,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
    idle: Option<Duration>,
) -> io::Result<Response> {
    let mut stream = TcpStream::connect_timeout(&target.address, CONNECT_TIMEOUT)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    stream.set_read_timeout(idle)?;
    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        target.path, target.address
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(body) = body {
        stream.write_all(body)?;
    }
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    read_line_capped(&mut reader, &mut status_line)?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad HTTP status line"))?;
    let mut response_headers = Vec::new();
    loop {
        let mut line = String::new();
        if read_line_capped(&mut reader, &mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if response_headers.len() >= MAX_HEADERS {
            return Err(too_large("the HTTP header block"));
        }
        if let Some((name, value)) = line.split_once(':') {
            response_headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let header = |name: &str| {
        response_headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let chunked = header("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    let body: Box<dyn BufRead + Send> = if chunked {
        Box::new(BufReader::new(Chunked {
            inner: reader,
            left: 0,
            done: false,
        }))
    } else if let Some(length) =
        header("content-length").and_then(|value| value.parse::<u64>().ok())
    {
        Box::new(BufReader::new(reader.take(length)))
    } else {
        Box::new(reader)
    };
    Ok(Response {
        status,
        headers: response_headers,
        body,
    })
}

/// A whole (non-streamed) body, at most [`MAX_MESSAGE_BYTES`].
fn read_body(body: &mut dyn BufRead) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    body.take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(too_large("an HTTP body"));
    }
    Ok(bytes)
}

/// The JSON-RPC messages in an SSE body, one per event with data; an event
/// over [`MAX_MESSAGE_BYTES`] is an error.
pub fn sse_messages(body: &mut dyn BufRead, mut each: impl FnMut(Value)) -> io::Result<()> {
    let mut data = String::new();
    loop {
        let mut line = String::new();
        let read = read_line_capped(body, &mut line)?;
        let line_text = line.trim_end_matches(['\r', '\n']);
        if read == 0 || line_text.is_empty() {
            if !data.is_empty() {
                if let Ok(message) = serde_json::from_str::<Value>(&data) {
                    each(message);
                }
                data.clear();
            }
            if read == 0 {
                return Ok(());
            }
            continue;
        }
        if let Some(value) = line_text.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
            if data.len() > MAX_MESSAGE_BYTES {
                return Err(too_large("an SSE event"));
            }
        }
        // `event:`, `id:`, `retry:` and comments carry nothing the bridge uses.
    }
}

/// Where the bridge writes: the agent's stdout, one message per line.
pub type Output = Arc<Mutex<Box<dyn Write + Send>>>;

/// The agent's stdout as a bridge output.
pub fn stdout_output() -> Output {
    Arc::new(Mutex::new(Box::new(io::stdout())))
}

/// How a bridge runs.
pub struct BridgeOptions {
    /// Sent as `Authorization: Bearer`.
    pub token: Option<String>,
    /// See [`IDLE_TIMEOUT`].
    pub idle_timeout: Duration,
    /// Set by the caller when the server is gone (the hub closed).
    pub stop: Arc<AtomicBool>,
}

impl BridgeOptions {
    pub fn new(token: Option<String>) -> Self {
        Self {
            token,
            idle_timeout: IDLE_TIMEOUT,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// How a bridge ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The agent closed its input.
    Done,
    /// The server ended the session or went away; why.
    SessionLost(String),
    /// The URL is not a loopback HTTP URL.
    InvalidUrl,
}

impl Ended {
    pub fn code(&self) -> u8 {
        match self {
            Self::Done => crate::exit_status::SUCCESS,
            Self::SessionLost(_) | Self::InvalidUrl => crate::exit_status::FAILED,
        }
    }
}

struct Session {
    id: Option<String>,
    protocol: Option<String>,
}

struct Shared {
    target: Target,
    token: Option<String>,
    idle: Duration,
    output: Output,
    session: Mutex<Session>,
    /// Requests sent and not yet answered, by their id's JSON text.
    pending: Mutex<HashMap<String, Value>>,
    lost: Mutex<Option<String>>,
    /// Once set nothing more is written (the bridge has answered for itself).
    ended: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn emit(&self, message: &Value) {
        let mut output = lock(&self.output);
        if self.ended.load(Ordering::Acquire) {
            return;
        }
        if let Some(id) = message.get("id")
            && message.get("method").is_none()
        {
            lock(&self.pending).remove(&id.to_string());
        }
        let _written = output
            .write_all(&jsonrpc::encode(message))
            .and_then(|()| output.flush());
    }

    fn headers(&self, accept: &str) -> Vec<(&'static str, String)> {
        let mut headers = vec![("Accept", accept.to_string())];
        if let Some(token) = &self.token {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        let session = lock(&self.session);
        if let Some(id) = &session.id {
            headers.push(("Mcp-Session-Id", id.clone()));
        }
        if let Some(protocol) = &session.protocol {
            headers.push(("MCP-Protocol-Version", protocol.clone()));
        }
        headers
    }

    fn lose(&self, reason: &str) {
        lock(&self.lost).get_or_insert_with(|| reason.to_string());
    }

    fn lost(&self) -> Option<String> {
        lock(&self.lost).clone()
    }

    /// Answer every request still waiting, then write nothing more.
    fn finish(&self, text: &str) {
        let waiting: Vec<Value> = lock(&self.pending).drain().map(|(_, id)| id).collect();
        for id in waiting {
            self.emit(&jsonrpc::retryable_error(id, text));
        }
        self.ended.store(true, Ordering::Release);
    }
}

/// Send one message and route every reply to the agent.
fn post(shared: &Shared, message: &Value) {
    let request_id = (jsonrpc::kind(message) == Kind::Request)
        .then(|| message.get("id").cloned())
        .flatten();
    if let Some(id) = &request_id {
        lock(&shared.pending).insert(id.to_string(), id.clone());
    }
    let mut headers = shared.headers("application/json, text/event-stream");
    headers.push(("Content-Type", "application/json".to_string()));
    let body = serde_json::to_vec(message).unwrap_or_default();
    let fail = |text: &str| {
        if let Some(id) = &request_id {
            shared.emit(&jsonrpc::retryable_error(id.clone(), text));
        }
    };
    let mut response = match request(
        &shared.target,
        "POST",
        &headers,
        Some(&body),
        Some(shared.idle),
    ) {
        Ok(response) => response,
        Err(error) => {
            fail(&format!("the MCP server did not answer: {error}"));
            return;
        }
    };
    let is_initialize = message.get("method").and_then(Value::as_str) == Some("initialize");
    if is_initialize && let Some(id) = response.header("mcp-session-id") {
        lock(&shared.session).id = Some(id.to_string());
    }
    let initialized = !is_initialize && lock(&shared.session).id.is_some();
    match response.status {
        200 | 201 => {
            let event_stream = response
                .header("content-type")
                .is_some_and(|value| value.starts_with("text/event-stream"));
            let deliver = |reply: Value| {
                if is_initialize
                    && let Some(version) = reply
                        .pointer("/result/protocolVersion")
                        .and_then(Value::as_str)
                {
                    lock(&shared.session).protocol = Some(version.to_string());
                }
                shared.emit(&reply);
            };
            if event_stream {
                if let Err(error) = sse_messages(&mut response.body, deliver) {
                    fail(&format!(
                        "the MCP server's event stream broke ({error}); retry"
                    ));
                }
            } else {
                match read_body(&mut response.body) {
                    Err(error) => fail(&format!("the MCP server's reply broke ({error}); retry")),
                    Ok(text) => match serde_json::from_slice::<Value>(&text) {
                        Ok(Value::Array(replies)) => replies.into_iter().for_each(deliver),
                        Ok(reply) if !reply.is_null() => deliver(reply),
                        _ => {}
                    },
                }
            }
            // A reply that never came (an empty stream) still gets an answer.
            if let Some(id) = &request_id
                && lock(&shared.pending).contains_key(&id.to_string())
            {
                fail("the MCP server sent no reply; retry");
            }
        }
        202 => {}
        404 if initialized => {
            shared.lose("session_expired");
            fail("the MCP server ended this session; restart the server");
        }
        401 | 403 if initialized => {
            shared.lose("unauthorized");
            fail("the MCP server no longer accepts this session; restart the server");
        }
        status => fail(&format!("the MCP server answered HTTP {status}")),
    }
}

/// Listen for server-initiated messages on a `GET` stream, if offered.
fn listen_stream(shared: &Shared) {
    let headers = shared.headers("text/event-stream");
    let Ok(mut response) = request(&shared.target, "GET", &headers, None, None) else {
        return;
    };
    if response.status != 200 {
        return;
    }
    let _streamed = sse_messages(&mut response.body, |message| shared.emit(&message));
}

/// Bridge `input` (the agent's stdin) to the server at `url` until input
/// ends, the server ends the session, or `options.stop` is set. Requests
/// run concurrently (at most 64); at the end of input the bridge waits
/// (bounded) for those in flight, then ends the session.
pub fn bridge(
    url: &str,
    mut input: impl BufRead + Send + 'static,
    output: Output,
    options: BridgeOptions,
) -> Ended {
    let Some(target) = parse_url(url) else {
        eprintln!("aethyme mcp: {url} is not a loopback HTTP URL");
        return Ended::InvalidUrl;
    };
    let shared = Arc::new(Shared {
        target,
        token: options.token,
        idle: options.idle_timeout,
        output,
        session: Mutex::new(Session {
            id: None,
            protocol: None,
        }),
        pending: Mutex::new(HashMap::new()),
        lost: Mutex::new(None),
        ended: AtomicBool::new(false),
    });
    // Input is read on its own thread, so a lost session ends the bridge
    // without waiting for the agent's next message.
    let (lines, incoming) = channel::<Vec<u8>>();
    let reader = thread::Builder::new()
        .name("mcp-http-stdin".into())
        .spawn(move || {
            while let Ok(Some(line)) = read_message(&mut input) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
    if let Err(error) = reader {
        eprintln!("aethyme mcp: could not read input: {error}");
        return Ended::SessionLost("input_unreadable".into());
    }
    let mut in_flight: Vec<thread::JoinHandle<()>> = Vec::new();
    let mut listening = false;
    let stopped = |shared: &Shared| {
        options
            .stop
            .load(Ordering::Acquire)
            .then(|| "server_gone".to_string())
            .or_else(|| shared.lost())
    };
    loop {
        if let Some(reason) = stopped(&shared) {
            shared.finish("the MCP server ended this session; restart the server");
            return Ended::SessionLost(reason);
        }
        let line = match incoming.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let Ok(message) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let is_initialize = message.get("method").and_then(Value::as_str) == Some("initialize");
        if is_initialize || jsonrpc::kind(&message) != Kind::Request {
            // initialize first (it creates the session), and notifications
            // and responses in order.
            post(&shared, &message);
            let has_session = lock(&shared.session).id.is_some();
            if is_initialize && has_session && !listening {
                listening = true;
                let listener = Arc::clone(&shared);
                let spawned = thread::Builder::new()
                    .name("mcp-http-listen".into())
                    .spawn(move || listen_stream(&listener));
                crate::warn_unrecorded("listen for MCP server messages", spawned);
            }
            continue;
        }
        // At most MAX_IN_FLIGHT requests at once; the next one waits.
        loop {
            in_flight.retain(|handle| !handle.is_finished());
            if in_flight.len() < MAX_IN_FLIGHT || stopped(&shared).is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let worker = Arc::clone(&shared);
        match thread::Builder::new()
            .name("mcp-http-request".into())
            .spawn(move || post(&worker, &message))
        {
            Ok(handle) => in_flight.push(handle),
            Err(error) => eprintln!("aethyme mcp: could not send a request: {error}"),
        }
    }
    let deadline = Instant::now() + DRAIN_TIMEOUT;
    while in_flight.iter().any(|handle| !handle.is_finished())
        && Instant::now() < deadline
        && stopped(&shared).is_none()
    {
        thread::sleep(Duration::from_millis(20));
    }
    let lost = stopped(&shared);
    shared.finish("the bridge stopped before the MCP server answered; retry");
    if lost.is_none() && lock(&shared.session).id.is_some() {
        let headers = shared.headers("application/json");
        let _deleted = request(
            &shared.target,
            "DELETE",
            &headers,
            None,
            Some(Duration::from_secs(5)),
        );
    }
    match lost {
        Some(reason) => Ended::SessionLost(reason),
        None => Ended::Done,
    }
}

/// Run the entry's HTTP server privately on a free loopback port and bridge
/// this process's stdio to it; the server stops when the bridge ends. Its
/// own stdout goes to stderr (stdout belongs to the protocol). Exits
/// `REFUSED` when the server cannot be trusted with the agent's traffic
/// (it listens off loopback, someone else holds the port, ownership cannot
/// be verified) and `ENVIRONMENT` when it never listens.
pub fn run_private(plan: &Plan, path: &str, auth: &HttpAuth) -> u8 {
    let port = match free_port() {
        Ok(port) => port,
        Err(error) => {
            eprintln!("aethyme mcp: {}: no free loopback port: {error}", plan.name);
            return crate::exit_status::ENVIRONMENT;
        }
    };
    let token = match auth {
        HttpAuth::None => None,
        HttpAuth::BearerEnv { .. } | HttpAuth::BearerArg { .. } => match new_token() {
            Ok(token) => Some(token),
            Err(error) => {
                eprintln!("aethyme mcp: {}: no random token: {error}", plan.name);
                return crate::exit_status::ENVIRONMENT;
            }
        },
    };
    let (extra_argv, token_env) = token_delivery(auth, token.as_deref());
    let mut argv = with_port(&plan.resolution.argv, port);
    argv.extend(extra_argv);
    let stdout_to_stderr = io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let mut command = std::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&plan.cwd)
        .env_clear()
        .envs(plan.environment.0.iter().cloned())
        .env(PORT_ENV, port.to_string())
        .stdin(Stdio::null())
        .stdout(stdout_to_stderr)
        .stderr(Stdio::inherit())
        .process_group(0);
    if let Some((name, value)) = token_env {
        command.env(name, value);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "aethyme mcp: {}: could not start {}: {error}",
                plan.name, argv[0]
            );
            return crate::exit_status::ENVIRONMENT;
        }
    };
    let started = Instant::now();
    let code = match wait_owned(port, child.id(), &mut child, LISTEN_TIMEOUT) {
        Ok(()) => {
            eprintln!(
                "aethyme mcp: {}: private HTTP server listening after {} ms",
                plan.name,
                started.elapsed().as_millis()
            );
            let ended = bridge(
                &url(port, path),
                BufReader::new(io::stdin()),
                stdout_output(),
                BridgeOptions::new(token),
            );
            if let Ended::SessionLost(reason) = &ended {
                eprintln!(
                    "aethyme mcp: {}: the HTTP session ended ({reason})",
                    plan.name
                );
            }
            ended.code()
        }
        Err(reason) => {
            eprintln!(
                "aethyme mcp: {}: the private HTTP server is not used ({reason})",
                plan.name
            );
            if reason == "listen_timeout" || reason.starts_with("exited_before_listening") {
                crate::exit_status::ENVIRONMENT
            } else {
                crate::exit_status::REFUSED
            }
        }
    };
    stop_child(&mut child);
    code
}

/// SIGTERM the child's process group, then SIGKILL if it lingers; reap it.
fn stop_child(child: &mut Child) {
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        // SAFETY: killpg takes no pointers; the group is our own child's
        // (process_group(0) at spawn) and the child is not yet reaped.
        let _signalled = unsafe { libc::killpg(group, libc::SIGTERM) };
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _killed = child.kill();
    let _reaped = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_ports_and_placeholders() {
        assert_eq!(
            with_port(&["server".into(), "--port={port}".into()], 47123),
            vec!["server".to_string(), "--port=47123".to_string()]
        );
        let target = parse_url("http://127.0.0.1:47123/mcp").unwrap();
        assert_eq!(target.address.port(), 47123);
        assert_eq!(target.path, "/mcp");
        assert!(parse_url("http://example.invalid:80/mcp").is_none());
        assert!(parse_url("https://127.0.0.1:1/mcp").is_none());
        assert_eq!(
            token_delivery(
                &HttpAuth::BearerArg {
                    flag: "--token".into()
                },
                Some("t")
            ),
            (vec!["--token".to_string(), "t".to_string()], None)
        );
        assert_eq!(
            token_delivery(
                &HttpAuth::BearerEnv {
                    name: "TOKEN".into()
                },
                Some("t")
            ),
            (Vec::new(), Some(("TOKEN".to_string(), "t".to_string())))
        );
        assert_eq!(new_token().unwrap().len(), 64);
    }

    #[test]
    fn chunked_bodies_and_sse_events_are_decoded() {
        let raw = b"5\r\nhello\r\n7;ext=1\r\n, world\r\n0\r\nTrailer: x\r\n\r\n";
        let mut body = String::new();
        Chunked {
            inner: &raw[..],
            left: 0,
            done: false,
        }
        .read_to_string(&mut body)
        .unwrap();
        assert_eq!(body, "hello, world");

        let sse = b": comment\nevent: message\ndata: {\"a\":1}\n\ndata: {\"b\":\ndata: 2}\n\n";
        let mut seen = Vec::new();
        sse_messages(&mut &sse[..], |message| seen.push(message)).unwrap();
        assert_eq!(seen, vec![json!({"a": 1}), json!({"b": 2})]);
    }

    #[test]
    fn oversized_lines_and_bodies_are_errors_not_allocations() {
        let long = format!("data: {}\n\n", "x".repeat(MAX_LINE + 10));
        let error = sse_messages(&mut long.as_bytes(), |_| {}).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let chunk = format!("{}\r\n", "f".repeat(MAX_LINE + 10));
        let mut chunked = Chunked {
            inner: chunk.as_bytes(),
            left: 0,
            done: false,
        };
        assert!(chunked.read(&mut [0u8; 16]).is_err());
        let body = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        assert!(read_body(&mut &body[..]).is_err());
    }
}
