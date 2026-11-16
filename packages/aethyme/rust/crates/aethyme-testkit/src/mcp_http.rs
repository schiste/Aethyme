//! A small fake MCP server speaking streamable HTTP, for `aethyme mcp`
//! tests. Each HTTP client gets its own session (`Mcp-Session-Id`), as an
//! isolated server would.
//!
//! - `POST /mcp` `initialize`: a new session, answered as JSON.
//! - any other `POST /mcp` request: answered as a chunked SSE stream (a
//!   progress notification, then `{"session", "text": <pid>}`); a
//!   `tools/call` of `plain`: the same reply as JSON; a request without a
//!   known session gets 404.
//! - notifications and responses: 202.
//! - `GET /mcp`: 405 (no server-initiated stream). `DELETE /mcp`: ends the
//!   session.
//!
//! Test binaries run it by re-executing themselves with `--exact` on a test
//! that calls [`run_from_env`].

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

/// The variable that asks a test process to become the fake server.
pub const FAKE_SERVER_ENV: &str = "AETHYME_TEST_FAKE_MCP_HTTP";

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
    serve(port);
}

struct Sessions {
    known: Mutex<HashSet<String>>,
    next: AtomicU64,
}

/// Serve on `127.0.0.1:port` until the process is stopped.
pub fn serve(port: u16) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).expect("bind the fake server");
    let sessions = Arc::new(Sessions {
        known: Mutex::new(HashSet::new()),
        next: AtomicU64::new(1),
    });
    for stream in listener.incoming().flatten() {
        let sessions = Arc::clone(&sessions);
        thread::spawn(move || {
            let _ignored = handle(stream, &sessions);
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

fn handle(mut stream: TcpStream, sessions: &Sessions) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let mut length = 0usize;
    let mut session: Option<String> = None;
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
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
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
    if tool == "plain" {
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
