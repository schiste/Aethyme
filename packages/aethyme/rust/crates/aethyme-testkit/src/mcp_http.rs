//! A small fake MCP server speaking streamable HTTP, for `aethyme mcp`
//! tests. Each HTTP client gets its own session (`Mcp-Session-Id`), as an
//! isolated server would.
//!
//! - `POST /mcp` `initialize`: a new session, answered as JSON.
//! - any other `POST /mcp` request: answered as a chunked SSE stream (a
//!   progress notification, then `{"session", "text": <pid>}`). A
//!   `tools/call` of `plain` gets the same reply as JSON; `hang` answers
//!   only after 30 seconds; `forget` forgets every session (they expire).
//!   A request without a known session gets 404.
//! - notifications and responses: 202.
//! - `GET /mcp`: 405 (no server-initiated stream). `DELETE /mcp`: ends the
//!   session.
//! - With a token configured, a request without `Authorization: Bearer
//!   <token>` gets 401.
//!
//! Test binaries run it in a child by re-executing themselves with
//! `--exact` on a test that calls [`run_from_env`].

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

/// The variable that asks a test process to become the fake server.
pub const FAKE_SERVER_ENV: &str = "AETHYME_TEST_FAKE_MCP_HTTP";
/// The bearer token the server requires, when set.
pub const TOKEN_ENV: &str = "AETHYME_TEST_FAKE_MCP_TOKEN";
/// The address it binds (default `127.0.0.1`).
pub const BIND_ENV: &str = "AETHYME_TEST_FAKE_MCP_BIND";
/// When set, the token is read from the file named by the argument after
/// this flag (for `bearer_file` entries; `--skip` suits a libtest binary,
/// which takes it with a value).
pub const TOKEN_FILE_FLAG_ENV: &str = "AETHYME_TEST_FAKE_MCP_TOKEN_FILE_FLAG";

/// How the fake server runs.
#[derive(Debug, Clone)]
pub struct FakeOptions {
    pub bind: IpAddr,
    pub token: Option<String>,
}

impl Default for FakeOptions {
    fn default() -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            token: None,
        }
    }
}

/// Serve forever on the port in `AETHYME_MCP_PORT` when [`FAKE_SERVER_ENV`]
/// is set; otherwise return at once (the test then passes trivially).
pub fn run_from_env() {
    if std::env::var_os(FAKE_SERVER_ENV).is_none() {
        return;
    }
    let port: u16 = std::env::var("AETHYME_MCP_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .expect("AETHYME_MCP_PORT");
    let options = FakeOptions {
        bind: std::env::var(BIND_ENV)
            .ok()
            .and_then(|bind| bind.parse().ok())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        token: token_from_file()
            .or_else(|| std::env::var(TOKEN_ENV).ok())
            .filter(|token| !token.is_empty()),
    };
    let listener = TcpListener::bind((options.bind, port)).expect("bind the fake server");
    serve(listener, options);
}

fn token_from_file() -> Option<String> {
    let flag = std::env::var(TOKEN_FILE_FLAG_ENV).ok()?;
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .iter()
        .position(|arg| *arg == flag)
        .and_then(|index| args.get(index + 1))?;
    std::fs::read_to_string(path).ok()
}

/// Start the fake server in this process on a free port; its port.
pub fn spawn_in_process(options: FakeOptions) -> u16 {
    let listener = TcpListener::bind((options.bind, 0)).expect("bind the fake server");
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || serve(listener, options));
    port
}

struct Sessions {
    known: Mutex<HashSet<String>>,
    next: AtomicU64,
}

fn serve(listener: TcpListener, options: FakeOptions) {
    let sessions = Arc::new(Sessions {
        known: Mutex::new(HashSet::new()),
        next: AtomicU64::new(1),
    });
    let options = Arc::new(options);
    for stream in listener.incoming().flatten() {
        let sessions = Arc::clone(&sessions);
        let options = Arc::clone(&options);
        thread::spawn(move || {
            let _ignored = handle(stream, &sessions, &options);
        });
    }
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn handle(
    mut stream: TcpStream,
    sessions: &Sessions,
    options: &FakeOptions,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let mut length = 0usize;
    let mut session: Option<String> = None;
    let mut authorization: Option<String> = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim().to_string();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => length = value.parse().unwrap_or(0),
                "mcp-session-id" => session = Some(value),
                "authorization" => authorization = Some(value),
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    if let Some(token) = &options.token
        && authorization.as_deref() != Some(format!("Bearer {token}").as_str())
    {
        return respond(&mut stream, "401 Unauthorized", &[], b"");
    }
    let known = |id: &Option<String>| {
        id.as_ref()
            .is_some_and(|id| sessions.known.lock().unwrap().contains(id))
    };
    match method.as_str() {
        "GET" => return respond(&mut stream, "405 Method Not Allowed", &[], b""),
        "DELETE" => {
            if let Some(id) = &session {
                sessions.known.lock().unwrap().remove(id);
            }
            return respond(&mut stream, "200 OK", &[], b"");
        }
        "POST" => {}
        _ => return respond(&mut stream, "400 Bad Request", &[], b""),
    }
    let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = message.get("id").cloned();
    let rpc_method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if rpc_method == "initialize" {
        let new_session = format!("s{}", sessions.next.fetch_add(1, Ordering::Relaxed));
        sessions.known.lock().unwrap().insert(new_session.clone());
        let reply = json!({"jsonrpc": "2.0", "id": id, "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake-http", "version": "1"},
        }});
        return respond(
            &mut stream,
            "200 OK",
            &[
                ("Content-Type", "application/json".into()),
                ("Mcp-Session-Id", new_session),
            ],
            reply.to_string().as_bytes(),
        );
    }
    if !known(&session) {
        return respond(&mut stream, "404 Not Found", &[], b"");
    }
    let Some(id) = id else {
        return respond(&mut stream, "202 Accepted", &[], b"");
    };
    if message.get("method").is_none() {
        return respond(&mut stream, "202 Accepted", &[], b"");
    }
    let tool = message
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reply = json!({"jsonrpc": "2.0", "id": id, "result": {
        "session": session,
        "text": std::process::id().to_string(),
    }});
    match tool {
        "hang" => thread::sleep(Duration::from_secs(30)),
        "forget" => sessions.known.lock().unwrap().clear(),
        _ => {}
    }
    if tool == "plain" || tool == "forget" {
        return respond(
            &mut stream,
            "200 OK",
            &[("Content-Type", "application/json".into())],
            reply.to_string().as_bytes(),
        );
    }
    // SSE over chunked encoding: a progress notification, then the reply.
    let event = |value: &Value| format!("event: message\ndata: {value}\n\n");
    let mut events = event(
        &json!({"jsonrpc": "2.0", "method": "notifications/progress",
        "params": {"progressToken": "t", "progress": 1}}),
    );
    events.push_str(&event(&reply));
    let head = "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
    stream.write_all(head.as_bytes())?;
    for chunk in events.as_bytes().chunks(17) {
        stream.write_all(format!("{:x}\r\n", chunk.len()).as_bytes())?;
        stream.write_all(chunk)?;
        stream.write_all(b"\r\n")?;
    }
    stream.write_all(b"0\r\n\r\n")?;
    stream.flush()
}
