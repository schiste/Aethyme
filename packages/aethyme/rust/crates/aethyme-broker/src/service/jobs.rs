//! Durable job contracts. These records describe work; they never authorize
//! cleanup, open a repository, start a worker, or change CLI routing.

use serde::{Deserialize, Serialize};

pub use crate::store::service_jobs::JobStore;

/// The first supported operation only computes a reviewed GC plan. Applying a
/// plan requires a separately designed authorization contract and is absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobOperation {
    GcPlan {
        include_active_gate_cache: bool,
        full_scan: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRequest {
    /// A stable repository identity resolved by the caller, shared by linked
    /// worktrees. A path supplied by an untrusted request is not that proof.
    pub repository: String,
    /// Scoped to the repository, retained even after the job finishes. A retry
    /// with this key must carry the exact original operation and deadline.
    pub idempotency_key: String,
    pub operation: JobOperation,
    /// Absolute job deadline in Unix milliseconds, part of its input identity.
    /// This is separate from a client's wait timeout. The store bounds queue
    /// admission; a running worker must enforce the deadline and report its
    /// actual outcome.
    pub deadline_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    /// The worker lost ownership before recording a result. Never requeued by
    /// this store; reconciliation must precede another attempt.
    Interrupted,
    Succeeded,
    Failed,
    Cancelled,
    Expired,
}

impl JobState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Interrupted => "interrupted",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobProgress {
    pub completed: u64,
    pub total: Option<u64>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobCompletion {
    Succeeded {
        result: serde_json::Value,
    },
    Failed {
        message: String,
    },
    /// A worker acknowledgement, distinct from a client's cancellation
    /// request. It must only be recorded once the worker has stopped work.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobRecord {
    pub id: i64,
    pub repository: String,
    pub operation: JobOperation,
    pub deadline_ms: i64,
    pub state: JobState,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub cancellation_requested: bool,
    pub progress: Option<JobProgress>,
    pub completion: Option<JobCompletion>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobSubmission {
    pub job: JobRecord,
    /// True for a retry or a request joining identical outstanding work.
    pub joined: bool,
}

/// Claim capability issued once. Its identity cannot be edited by callers.
/// A claim ceases to be valid after completion or worker interruption.
pub struct JobClaim {
    pub(crate) job: JobRecord,
    pub(crate) worker: String,
    pub(crate) token: String,
}

impl std::fmt::Debug for JobClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobClaim")
            .field("job", &self.job)
            .field("worker", &self.worker)
            .finish_non_exhaustive()
    }
}

impl JobClaim {
    pub fn job(&self) -> &JobRecord {
        &self.job
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("job database: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("job encoding: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("invalid job request: {0}")]
    Invalid(String),
    #[error("job database has an incompatible identity or schema")]
    IncompatibleDatabase,
    #[error("idempotency key was already used with different inputs")]
    IdempotencyConflict,
    #[error("job {0} was not found")]
    NotFound(i64),
    #[error("job {0} is no longer owned by this worker")]
    ClaimLost(i64),
}
