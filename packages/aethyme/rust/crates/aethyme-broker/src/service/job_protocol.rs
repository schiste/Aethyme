//! Typed job protocol v2. Health v1 remains unchanged. Job results are fetched
//! in bounded chunks; a status response never includes an unbounded plan.

use serde::{Deserialize, Serialize};

use super::jobs::{JobCompletion, JobOperation, JobRecord, JobState};

pub const VERSION: u32 = 2;
pub const RESULT_CHUNK_BYTES: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u32,
    pub request_id: u64,
    pub method: Method,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Method {
    Submit {
        idempotency_key: String,
        operation: JobOperation,
        deadline_ms: i64,
    },
    Lookup {
        idempotency_key: String,
    },
    Status {
        job_id: i64,
    },
    Cancel {
        job_id: i64,
    },
    ResultChunk {
        job_id: i64,
        offset: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub protocol_version: u32,
    pub request_id: u64,
    pub result: Result<Reply, Error>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Submitted {
        job: Status,
        joined: bool,
    },
    Status {
        job: Status,
    },
    Lookup {
        #[serde(deserialize_with = "required_nullable")]
        job: Option<Status>,
    },
    CancelRequested {
        job: Status,
    },
    ResultChunk {
        job_id: i64,
        offset: usize,
        next_offset: usize,
        complete: bool,
        data: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub id: i64,
    pub operation: JobOperation,
    pub state: JobState,
    pub deadline_ms: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub cancellation_requested: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub progress: Option<Progress>,
    #[serde(deserialize_with = "required_nullable")]
    pub completion: Option<Completion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub completed: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub total: Option<u64>,
    pub message: String,
}

// A nullable field is still required on the wire. Without a custom decoder,
// Serde gives Option fields an implicit default when their key is absent.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Completion {
    Succeeded,
    Failed { message: String },
    Cancelled,
}

impl From<&JobRecord> for Status {
    fn from(job: &JobRecord) -> Self {
        Self {
            id: job.id,
            operation: job.operation.clone(),
            state: job.state,
            deadline_ms: job.deadline_ms,
            created_at_ms: job.created_at_ms,
            updated_at_ms: job.updated_at_ms,
            cancellation_requested: job.cancellation_requested,
            progress: job.progress.as_ref().map(|progress| Progress {
                completed: progress.completed,
                total: progress.total,
                message: progress.message.clone(),
            }),
            completion: job.completion.as_ref().map(|completion| match completion {
                JobCompletion::Succeeded { .. } => Completion::Succeeded,
                JobCompletion::Failed { message } => Completion::Failed {
                    message: message.clone(),
                },
                JobCompletion::Cancelled => Completion::Cancelled,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedProtocol,
    JobsDisabled,
    InvalidRequest,
    IdempotencyConflict,
    NotFound,
    NotReady,
    Unavailable,
    Internal,
}

/// A UTF-8 byte offset makes retries deterministic. Chunks are small enough
/// to fit the 64 KiB frame even after worst-case JSON escaping.
pub fn result_chunk(job_id: i64, encoded: &str, offset: usize) -> Result<Reply, Error> {
    if offset > encoded.len() || !encoded.is_char_boundary(offset) {
        return Err(Error {
            code: ErrorCode::InvalidRequest,
            message: "invalid result byte offset".into(),
        });
    }
    let mut end = offset.saturating_add(RESULT_CHUNK_BYTES).min(encoded.len());
    while !encoded.is_char_boundary(end) {
        end -= 1;
    }
    Ok(Reply::ResultChunk {
        job_id,
        offset,
        next_offset: end,
        complete: end == encoded.len(),
        data: encoded[offset..end].into(),
    })
}
