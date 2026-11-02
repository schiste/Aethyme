//! Versioned, bounded JSON-line transport for the opt-in broker service.
//!
//! One request and response per connection. Request IDs correlate replies;
//! they are not idempotency keys. Mutations require a separate durable job
//! contract before they can be added here.

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};

// Health uses v3 so it cannot be confused with job protocol v2 by the
// transport's version dispatcher. V3 adds the complete router/engine build
// identity to the health reply.
pub const PROTOCOL_VERSION: u32 = 3;
/// Includes the terminating newline. Both directions use the same bound.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u32,
    pub request_id: u64,
    pub method: Method,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub protocol_version: u32,
    pub request_id: u64,
    pub result: Result<Pong, ServiceError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pong {
    pub pid: u32,
    pub version: String,
    pub build_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedProtocol,
}

impl Request {
    pub fn ping(request_id: u64) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            method: Method::Ping,
        }
    }

    /// Reject incompatible versions before dispatching any method.
    pub fn validate(&self) -> Result<(), ServiceError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ServiceError {
                code: ErrorCode::UnsupportedProtocol,
                message: format!(
                    "unsupported broker service protocol {}; expected {PROTOCOL_VERSION}",
                    self.protocol_version
                ),
            });
        }
        Ok(())
    }
}

impl Response {
    /// A response must belong to this request and use the supported protocol.
    pub fn validate_for(&self, request: &Request) -> io::Result<()> {
        if self.protocol_version != PROTOCOL_VERSION || self.request_id != request.request_id {
            return Err(invalid(
                "broker service response version or request ID mismatch",
            ));
        }
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Read at most one bounded frame. EOF without a newline is an incomplete
/// message, even when the prefix happens to be valid JSON. Callers must set
/// socket deadlines separately: a byte bound is not a time bound.
pub fn read_frame<T: serde::de::DeserializeOwned>(reader: &mut impl BufRead) -> io::Result<T> {
    serde_json::from_slice(&read_frame_bytes(reader)?).map_err(|error| invalid(error.to_string()))
}

pub(crate) fn read_frame_bytes(reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete broker service frame",
            ));
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if count > MAX_FRAME_BYTES - bytes.len() {
            return Err(invalid("broker service frame exceeds byte limit"));
        }
        let complete = available[count - 1] == b'\n';
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if complete {
            return Ok(bytes);
        }
    }
}

/// Serialize and check the complete frame before writing any bytes.
pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|error| invalid(error.to_string()))?;
    if bytes.len() >= MAX_FRAME_BYTES {
        return Err(invalid("broker service frame exceeds byte limit"));
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}
