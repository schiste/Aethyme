//! A separate, explicitly opened service job database. Legacy broker opens do
//! not create or migrate it. SQL remains in the store layer.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::service::jobs::{
    JobClaim, JobCompletion, JobError, JobOperation, JobProgress, JobRecord, JobRequest, JobState,
    JobSubmission,
};

const APPLICATION_ID: i64 = 0x41534a31;
const SCHEMA_VERSION: i64 = 1;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_RESULT_BYTES: usize = 1024 * 1024;

const SCHEMA: &str = "
CREATE TABLE service_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repository TEXT NOT NULL,
    operation_json TEXT NOT NULL,
    deadline_ms INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN (
        'queued', 'running', 'interrupted', 'succeeded', 'failed', 'cancelled', 'expired'
    )),
    worker TEXT,
    claim_token TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    cancellation_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancellation_requested IN (0,1)),
    progress_json TEXT,
    completion_json TEXT,
    CHECK((state = 'running' AND worker IS NOT NULL AND claim_token IS NOT NULL)
       OR (state <> 'running' AND worker IS NULL AND claim_token IS NULL))
);
CREATE UNIQUE INDEX service_jobs_outstanding
    ON service_jobs(repository, operation_json, deadline_ms)
    WHERE state IN ('queued', 'running', 'interrupted');
CREATE TABLE service_job_requests (
    repository TEXT NOT NULL,
    request_key TEXT NOT NULL,
    job_id INTEGER NOT NULL REFERENCES service_jobs(id),
    PRIMARY KEY(repository, request_key)
);
";

/// One connection per caller. SQLite immediate transactions serialize claims,
/// deduplication and state transitions across processes. The caller must select
/// a trusted private database path and fence worker ownership; opening storage
/// is neither service singleton ownership nor repository authorization.
pub struct JobStore {
    conn: Connection,
}

impl JobStore {
    pub fn open(path: &Path) -> Result<Self, JobError> {
        Self::open_with_flags(path, OpenFlags::default())
    }

    /// The service supplies a validated canonical private directory. Keep
    /// its stricter no-follow policy out of the existing general store API.
    #[cfg(unix)]
    pub(crate) fn open_private(path: &Path) -> Result<Self, JobError> {
        Self::open_with_flags(path, OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW)
    }

    fn open_with_flags(path: &Path, flags: OpenFlags) -> Result<Self, JobError> {
        let mut conn = Connection::open_with_flags(path, flags)?;
        conn.busy_timeout(Duration::from_secs(2))?;
        // Do not change journal mode or schema before checking identity. In
        // particular, accidentally selecting broker.db must leave it intact.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let application: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if application == 0 && version == 0 {
            let objects: i64 = tx.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
                [],
                |r| r.get(0),
            )?;
            if objects != 0 {
                return Err(JobError::IncompatibleDatabase);
            }
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if application != APPLICATION_ID || version != SCHEMA_VERSION {
            return Err(JobError::IncompatibleDatabase);
        }
        tx.commit()?;
        conn.pragma_update(None, "foreign_keys", true)?;
        // FULL makes a committed response durable across service restart; WAL
        // keeps status reads independent of writers on other connections.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { conn })
    }

    #[cfg(unix)]
    pub(crate) fn new_worker_generation(&self) -> Result<String, JobError> {
        Ok(self
            .conn
            .query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?)
    }

    #[cfg(unix)]
    pub(crate) fn outstanding_count(&self, repository: &str) -> Result<usize, JobError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM service_jobs WHERE repository=?1 AND state IN ('queued','running','interrupted')",
            [repository], |r| r.get(0),
        )?)
    }

    /// Retrying a key always returns its original job, including after a
    /// terminal outcome. New keys join identical outstanding work. A fresh key
    /// after completion creates a fresh plan instead of serving stale results.
    pub fn submit(&mut self, request: &JobRequest, now_ms: i64) -> Result<JobSubmission, JobError> {
        valid_text(&request.repository, "repository")?;
        valid_text(&request.idempotency_key, "idempotency key")?;
        valid_time(now_ms)?;
        let operation = serde_json::to_string(&request.operation)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<i64> = tx
            .query_row(
                "SELECT job_id FROM service_job_requests WHERE repository=?1 AND request_key=?2",
                params![request.repository, request.idempotency_key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = previous {
            let job = read_job(&tx, id)?;
            if job.operation != request.operation || job.deadline_ms != request.deadline_ms {
                return Err(JobError::IdempotencyConflict);
            }
            tx.commit()?;
            return Ok(JobSubmission { job, joined: true });
        }
        if request.deadline_ms <= now_ms {
            return Err(JobError::Invalid("deadline must be in the future".into()));
        }
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM service_jobs WHERE repository=?1 AND operation_json=?2
             AND deadline_ms=?3 AND state IN ('queued','running','interrupted')",
                params![request.repository, operation, request.deadline_ms],
                |r| r.get(0),
            )
            .optional()?;
        let id = if let Some(id) = existing {
            id
        } else {
            tx.execute(
                "INSERT INTO service_jobs(repository,operation_json,deadline_ms,state,created_at_ms,updated_at_ms)
                 VALUES (?1,?2,?3,'queued',?4,?4)",
                params![request.repository, operation, request.deadline_ms, now_ms],
            )?;
            tx.last_insert_rowid()
        };
        tx.execute(
            "INSERT INTO service_job_requests(repository,request_key,job_id) VALUES (?1,?2,?3)",
            params![request.repository, request.idempotency_key, id],
        )?;
        let job = read_job(&tx, id)?;
        tx.commit()?;
        Ok(JobSubmission {
            job,
            joined: existing.is_some(),
        })
    }

    pub fn get(&self, id: i64) -> Result<JobRecord, JobError> {
        read_job(&self.conn, id)
    }

    /// A lost reply can be reconciled without knowing the server's job ID.
    pub fn find_request(&self, repository: &str, key: &str) -> Result<Option<JobRecord>, JobError> {
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT job_id FROM service_job_requests WHERE repository=?1 AND request_key=?2",
                params![repository, key],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| self.get(id)).transpose()
    }

    /// Claims at most one unexpired queued job. This never retries a running
    /// or interrupted job. `worker` identifies this fenced service generation.
    pub fn claim_next(&mut self, worker: &str, now_ms: i64) -> Result<Option<JobClaim>, JobError> {
        valid_text(worker, "worker")?;
        valid_time(now_ms)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE service_jobs SET state='expired',updated_at_ms=MAX(updated_at_ms,?1)
             WHERE state='queued' AND deadline_ms<=?1",
            [now_ms],
        )?;
        let id: Option<i64> = tx
            .query_row(
                "SELECT id FROM service_jobs WHERE state='queued' ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let claim = if let Some(id) = id {
            tx.execute(
                "UPDATE service_jobs SET state='running',worker=?1,claim_token=lower(hex(randomblob(16))),
                 updated_at_ms=MAX(updated_at_ms,?2) WHERE id=?3",
                params![worker, now_ms, id],
            )?;
            let token = tx.query_row(
                "SELECT claim_token FROM service_jobs WHERE id=?1",
                [id],
                |r| r.get(0),
            )?;
            Some(JobClaim {
                job: read_job(&tx, id)?,
                worker: worker.into(),
                token,
            })
        } else {
            None
        };
        tx.commit()?;
        Ok(claim)
    }

    /// Cancels queued work immediately. Running work only records a request;
    /// callers must not interpret it as proof that side effects have stopped.
    /// Cancellation applies to the shared job and therefore to every joiner.
    pub fn request_cancel(&mut self, id: i64, now_ms: i64) -> Result<JobRecord, JobError> {
        valid_time(now_ms)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        read_job(&tx, id)?;
        tx.execute(
            "UPDATE service_jobs SET cancellation_requested=1,
             state=CASE WHEN state='queued' THEN 'cancelled' ELSE state END,
             updated_at_ms=MAX(updated_at_ms,?1)
             WHERE id=?2 AND state IN ('queued','running','interrupted')",
            params![now_ms, id],
        )?;
        let job = read_job(&tx, id)?;
        tx.commit()?;
        Ok(job)
    }

    pub fn progress(
        &mut self,
        claim: &JobClaim,
        progress: &JobProgress,
        now_ms: i64,
    ) -> Result<JobRecord, JobError> {
        valid_time(now_ms)?;
        valid_message(&progress.message)?;
        if progress
            .total
            .is_some_and(|total| progress.completed > total)
        {
            return Err(JobError::Invalid("progress exceeds total".into()));
        }
        let encoded = serde_json::to_string(progress)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job = require_claim(&tx, claim)?;
        if job
            .progress
            .is_some_and(|old| progress.completed < old.completed)
        {
            return Err(JobError::Invalid("progress cannot move backwards".into()));
        }
        tx.execute(
            "UPDATE service_jobs SET progress_json=?1,updated_at_ms=MAX(updated_at_ms,?2) WHERE id=?3",
            params![encoded, now_ms, claim.job.id],
        )?;
        let job = read_job(&tx, claim.job.id)?;
        tx.commit()?;
        Ok(job)
    }

    pub fn complete(
        &mut self,
        claim: &JobClaim,
        completion: &JobCompletion,
        now_ms: i64,
    ) -> Result<JobRecord, JobError> {
        valid_time(now_ms)?;
        if let JobCompletion::Failed { message } = completion {
            valid_message(message)?;
        }
        let encoded = serde_json::to_string(completion)?;
        if encoded.len() > MAX_RESULT_BYTES {
            return Err(JobError::Invalid("result exceeds one MiB".into()));
        }
        let state = match completion {
            JobCompletion::Succeeded { .. } => JobState::Succeeded,
            JobCompletion::Failed { .. } => JobState::Failed,
            JobCompletion::Cancelled => JobState::Cancelled,
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_claim(&tx, claim)?;
        tx.execute(
            "UPDATE service_jobs SET state=?1,worker=NULL,claim_token=NULL,completion_json=?2,
             updated_at_ms=MAX(updated_at_ms,?3) WHERE id=?4",
            params![state.as_str(), encoded, now_ms, claim.job.id],
        )?;
        let job = read_job(&tx, claim.job.id)?;
        tx.commit()?;
        Ok(job)
    }

    /// Call only after independently fencing this exact worker generation.
    /// This is not a timeout or liveness detector: an elapsed deadline alone
    /// cannot prove a worker is gone. Interrupted jobs continue to deduplicate
    /// requests and cannot be claimed again or completed by the stale claim.
    pub fn interrupt_worker(&mut self, worker: &str, now_ms: i64) -> Result<usize, JobError> {
        valid_text(worker, "worker")?;
        valid_time(now_ms)?;
        Ok(self.conn.execute(
            "UPDATE service_jobs SET state='interrupted',worker=NULL,claim_token=NULL,
             updated_at_ms=MAX(updated_at_ms,?1) WHERE state='running' AND worker=?2",
            params![now_ms, worker],
        )?)
    }
}

fn require_claim(conn: &Connection, claim: &JobClaim) -> Result<JobRecord, JobError> {
    let owned: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM service_jobs WHERE id=?1 AND state='running' AND worker=?2 AND claim_token=?3)",
        params![claim.job.id, claim.worker, claim.token], |r| r.get(0),
    )?;
    if !owned {
        return Err(JobError::ClaimLost(claim.job.id));
    }
    read_job(conn, claim.job.id)
}

fn valid_time(now_ms: i64) -> Result<(), JobError> {
    if now_ms < 0 {
        return Err(JobError::Invalid("time must be nonnegative".into()));
    }
    Ok(())
}

fn valid_text(text: &str, field: &str) -> Result<(), JobError> {
    if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES || text.contains('\0') {
        return Err(JobError::Invalid(format!(
            "{field} must contain 1..={MAX_TEXT_BYTES} non-NUL bytes"
        )));
    }
    Ok(())
}

fn valid_message(text: &str) -> Result<(), JobError> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(JobError::Invalid("message exceeds 4096 bytes".into()));
    }
    Ok(())
}

fn read_job(conn: &Connection, id: i64) -> Result<JobRecord, JobError> {
    conn.query_row(
        "SELECT id,repository,operation_json,deadline_ms,state,created_at_ms,updated_at_ms,
                cancellation_requested,progress_json,completion_json FROM service_jobs WHERE id=?1",
        [id],
        |r| {
            let operation = decode::<JobOperation>(r.get(2)?, 2)?;
            let state: String = r.get(4)?;
            let state = decode::<JobState>(
                serde_json::to_string(&state).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                4,
            )?;
            let progress: Option<String> = r.get(8)?;
            let completion: Option<String> = r.get(9)?;
            Ok(JobRecord {
                id: r.get(0)?,
                repository: r.get(1)?,
                operation,
                deadline_ms: r.get(3)?,
                state,
                created_at_ms: r.get(5)?,
                updated_at_ms: r.get(6)?,
                cancellation_requested: r.get(7)?,
                progress: progress.map(|s| decode(s, 8)).transpose()?,
                completion: completion.map(|s| decode(s, 9)).transpose()?,
            })
        },
    )
    .optional()?
    .ok_or(JobError::NotFound(id))
}

fn decode<T: serde::de::DeserializeOwned>(encoded: String, column: usize) -> rusqlite::Result<T> {
    serde_json::from_str(&encoded).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}
