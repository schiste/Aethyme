//! MCP stdio framing: one JSON-RPC message per line.
//!
//! The hub reads whole messages to rewrite ids; it never logs them.

use std::io::{self, BufRead};

use serde_json::{Value, json};

/// Messages longer than this close the connection that sent them.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

/// One newline-terminated message without its newline; `None` at end of
/// stream. A final unterminated message is returned as is.
pub fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        if let Some(position) = available.iter().position(|byte| *byte == b'\n') {
            line.extend_from_slice(&available[..position]);
            reader.consume(position + 1);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
        let length = available.len();
        line.extend_from_slice(available);
        reader.consume(length);
        if line.len() > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "an MCP message exceeds 32 MiB",
            ));
        }
    }
}

/// The message as one line, newline included.
pub fn encode(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Request,
    Notification,
    Response,
    Invalid,
}

/// What a message is, by its `method` and `id`.
pub fn kind(message: &Value) -> Kind {
    let has_method = message.get("method").and_then(Value::as_str).is_some();
    let has_id = message.get("id").is_some_and(|id| !id.is_null());
    match (has_method, has_id) {
        (true, true) => Kind::Request,
        (true, false) => Kind::Notification,
        (false, true) => Kind::Response,
        (false, false) => Kind::Invalid,
    }
}

/// A JSON-RPC error response.
pub fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

/// The error code a hub uses for a request that may simply be retried.
pub const RETRYABLE: i64 = -32000;

/// A retryable error: the request was not answered and was not replayed.
pub fn retryable_error(id: Value, reason: &str) -> Value {
    error_response(id, RETRYABLE, reason, Some(json!({"retryable": true})))
}
