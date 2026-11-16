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
//! connection per request (`Connection: close`).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::jsonrpc::{self, Kind, read_message};
use super::key::Plan;

/// The path an HTTP entry is served on unless it sets `http_path`.
pub const DEFAULT_PATH: &str = "/mcp";
/// The variable an HTTP upstream finds its port in (besides `{port}` in
/// its argv).
pub const PORT_ENV: &str = "AETHYME_MCP_PORT";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READY_POLL: Duration = Duration::from_millis(100);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_IN_FLIGHT: usize = 64;

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

/// Whether something accepts connections on `port`.
pub fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Duration::from_millis(200),
    )
    .is_ok()
}

/// A free loopback port, for a private server (the OS picks it; a race with
/// another process taking it before the server binds is possible).
pub fn free_port() -> io::Result<u16> {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
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
            self.inner.read_line(&mut line)?;
            let size = line.trim().split(';').next().unwrap_or_default();
            self.left = usize::from_str_radix(size, 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if self.left == 0 {
                // Trailers, up to the empty line.
                loop {
                    let mut trailer = String::new();
                    if self.inner.read_line(&mut trailer)? == 0 || trailer.trim().is_empty() {
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
            self.inner.read_line(&mut end)?;
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

/// One HTTP/1.1 request on a fresh connection.
pub fn request(
    target: &Target,
    method: &str,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
) -> io::Result<Response> {
    let mut stream = TcpStream::connect_timeout(&target.address, CONNECT_TIMEOUT)?;
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
    reader.read_line(&mut status_line)?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad HTTP status line"))?;
    let mut response_headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
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

/// The JSON-RPC messages in an SSE body, one per event with data.
pub fn sse_messages(body: &mut dyn BufRead, mut each: impl FnMut(Value)) -> io::Result<()> {
    let mut data = String::new();
    loop {
        let mut line = String::new();
        let read = body.read_line(&mut line)?;
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
        }
        // `event:`, `id:`, `retry:` and comments carry nothing the bridge uses.
    }
}

/// Where the bridge writes: the agent's stdout, one message per line.
pub type Output = Arc<Mutex<Box<dyn Write + Send>>>;

fn emit(output: &Output, message: &Value) {
    let mut output = output
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _written = output
        .write_all(&jsonrpc::encode(message))
        .and_then(|()| output.flush());
}

struct Session {
    id: Option<String>,
    protocol: Option<String>,
}

/// Send one message; route every reply to `output`. Returns false when the
/// session is gone (404) and the bridge must end.
fn post(target: &Target, session: &Mutex<Session>, message: &Value, output: &Output) -> bool {
    let request_id = (jsonrpc::kind(message) == Kind::Request)
        .then(|| message.get("id").cloned())
        .flatten();
    let mut headers = vec![
        ("Content-Type", "application/json".to_string()),
        ("Accept", "application/json, text/event-stream".to_string()),
    ];
    {
        let session = session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(id) = &session.id {
            headers.push(("Mcp-Session-Id", id.clone()));
        }
        if let Some(protocol) = &session.protocol {
            headers.push(("MCP-Protocol-Version", protocol.clone()));
        }
    }
    let body = serde_json::to_vec(message).unwrap_or_default();
    let fail = |text: &str| {
        if let Some(id) = &request_id {
            emit(output, &jsonrpc::retryable_error(id.clone(), text));
        }
    };
    let mut response = match request(target, "POST", &headers, Some(&body)) {
        Ok(response) => response,
        Err(error) => {
            fail(&format!("the MCP server did not answer: {error}"));
            return true;
        }
    };
    let is_initialize = message.get("method").and_then(Value::as_str) == Some("initialize");
    if is_initialize && let Some(id) = response.header("mcp-session-id") {
        session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .id = Some(id.to_string());
    }
    match response.status {
        200 | 201 => {
            let event_stream = response
                .header("content-type")
                .is_some_and(|value| value.starts_with("text/event-stream"));
            let mut deliver = |reply: Value| {
                if is_initialize
                    && let Some(version) = reply
                        .pointer("/result/protocolVersion")
                        .and_then(Value::as_str)
                {
                    session
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .protocol = Some(version.to_string());
                }
                emit(output, &reply);
            };
            if event_stream {
                if sse_messages(&mut response.body, &mut deliver).is_err() {
                    fail("the MCP server's event stream broke; retry");
                }
            } else {
                let mut text = Vec::new();
                if response.body.read_to_end(&mut text).is_err() {
                    fail("the MCP server's reply broke; retry");
                    return true;
                }
                match serde_json::from_slice::<Value>(&text) {
                    Ok(Value::Array(replies)) => replies.into_iter().for_each(&mut deliver),
                    Ok(reply) if !reply.is_null() => deliver(reply),
                    _ => {}
                }
            }
            true
        }
        202 => true,
        404 if session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .id
            .is_some() =>
        {
            fail("the MCP server ended this session; restart the server");
            false
        }
        status => {
            fail(&format!("the MCP server answered HTTP {status}"));
            true
        }
    }
}

/// Listen for server-initiated messages on a `GET` stream, if offered.
fn listen(target: Target, session: Arc<Mutex<Session>>, output: Output, ended: Arc<AtomicBool>) {
    let mut headers = vec![("Accept", "text/event-stream".to_string())];
    {
        let session = session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(id) = &session.id {
            headers.push(("Mcp-Session-Id", id.clone()));
        }
        if let Some(protocol) = &session.protocol {
            headers.push(("MCP-Protocol-Version", protocol.clone()));
        }
    }
    let Ok(mut response) = request(&target, "GET", &headers, None) else {
        return;
    };
    if response.status != 200 {
        return;
    }
    let _streamed = sse_messages(&mut response.body, |message| {
        if !ended.load(Ordering::Acquire) {
            emit(&output, &message);
        }
    });
}

/// Bridge `input` (the agent's stdin) to the server at `url` until input
/// ends or the server ends the session. Requests run concurrently; the
/// bridge waits (bounded) for those in flight before ending the session.
pub fn bridge(url: &str, mut input: impl BufRead, output: Output) -> u8 {
    let Some(target) = parse_url(url) else {
        eprintln!("aethyme mcp: {url} is not a loopback HTTP URL");
        return crate::exit_status::FAILED;
    };
    let session = Arc::new(Mutex::new(Session {
        id: None,
        protocol: None,
    }));
    let ended = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let mut in_flight: Vec<thread::JoinHandle<()>> = Vec::new();
    let mut listening = false;
    while let Ok(Some(line)) = read_message(&mut input) {
        if lost.load(Ordering::Acquire) {
            break;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let is_initialize = message.get("method").and_then(Value::as_str) == Some("initialize");
        if is_initialize || jsonrpc::kind(&message) != Kind::Request {
            // initialize first (it creates the session), and notifications
            // and responses in order.
            if !post(&target, &session, &message, &output) {
                lost.store(true, Ordering::Release);
                break;
            }
            let has_session = session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .id
                .is_some();
            if is_initialize && has_session && !listening {
                listening = true;
                let (target, session, output, ended) = (
                    target.clone(),
                    Arc::clone(&session),
                    Arc::clone(&output),
                    Arc::clone(&ended),
                );
                let spawned = thread::Builder::new()
                    .name("mcp-http-listen".into())
                    .spawn(move || listen(target, session, output, ended));
                crate::warn_unrecorded("listen for MCP server messages", spawned);
            }
            continue;
        }
        let (target, session, output, lost) = (
            target.clone(),
            Arc::clone(&session),
            Arc::clone(&output),
            Arc::clone(&lost),
        );
        // At most MAX_IN_FLIGHT requests at once; the next one waits.
        loop {
            in_flight.retain(|handle| !handle.is_finished());
            if in_flight.len() < MAX_IN_FLIGHT {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        match thread::Builder::new()
            .name("mcp-http-request".into())
            .spawn(move || {
                if !post(&target, &session, &message, &output) {
                    lost.store(true, Ordering::Release);
                }
            }) {
            Ok(handle) => in_flight.push(handle),
            Err(error) => eprintln!("aethyme mcp: could not send a request: {error}"),
        }
    }
    let deadline = Instant::now() + DRAIN_TIMEOUT;
    for handle in in_flight {
        while !handle.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        if handle.is_finished() {
            let _joined = handle.join();
        }
    }
    ended.store(true, Ordering::Release);
    let id = session
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .id
        .clone();
    if let Some(id) = id {
        let _deleted = request(&target, "DELETE", &[("Mcp-Session-Id", id)], None);
    }
    if lost.load(Ordering::Acquire) {
        crate::exit_status::FAILED
    } else {
        crate::exit_status::SUCCESS
    }
}

/// The agent's stdout as a bridge output.
pub fn stdout_output() -> Output {
    Arc::new(Mutex::new(Box::new(io::stdout())))
}

/// Wait until `port` accepts connections, the child exits, or the deadline.
pub fn wait_ready(port: u16, child: &mut Child, within: Duration) -> Result<(), String> {
    let deadline = Instant::now() + within;
    loop {
        if port_open(port) {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("the server exited before listening ({status})"));
        }
        if Instant::now() >= deadline {
            return Err("the server did not listen in time".into());
        }
        thread::sleep(READY_POLL);
    }
}

/// Run the entry's HTTP server privately on a free loopback port and bridge
/// this process's stdio to it; the server stops when the bridge ends. Its
/// own stdout goes to stderr (stdout belongs to the protocol).
pub fn run_private(plan: &Plan, path: &str) -> u8 {
    let port = match free_port() {
        Ok(port) => port,
        Err(error) => {
            eprintln!("aethyme mcp: {}: no free loopback port: {error}", plan.name);
            return crate::exit_status::ENVIRONMENT;
        }
    };
    let argv = with_port(&plan.resolution.argv, port);
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
    let code = match wait_ready(port, &mut child, Duration::from_secs(30)) {
        Ok(()) => bridge(
            &url(port, path),
            BufReader::new(io::stdin()),
            stdout_output(),
        ),
        Err(reason) => {
            eprintln!("aethyme mcp: {}: {reason}", plan.name);
            crate::exit_status::ENVIRONMENT
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
}
