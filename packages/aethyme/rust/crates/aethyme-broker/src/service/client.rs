//! Explicit repository-verified client. Refusals never trigger local execution.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::job_protocol::{self as wire, Method, Reply, Status};
use super::jobs::{JobOperation, JobState};
use super::{job_runtime, transport};

pub(crate) struct Client {
    directory: PathBuf,
    repository_key: String,
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn accepted(response: wire::Response) -> io::Result<Reply> {
    response.result.map_err(|error| {
        io::Error::other(format!(
            "shared service {:?}: {}",
            error.code, error.message
        ))
    })
}

impl Client {
    pub(crate) fn connect(directory: &Path, repository: &Path) -> io::Result<Self> {
        // Reuse the worker's snapshot-only owner validation. A remote key by
        // itself cannot establish ownership of the independently selected repo.
        let expected = job_runtime::selected_repository_identity(repository)?;
        let reply = accepted(transport::scoped_job_request(
            directory,
            &expected.key,
            Method::Repository {},
        )?)?;
        if reply
            != (Reply::Repository {
                identity: expected.clone(),
            })
        {
            return Err(invalid(
                "shared service returned a different repository owner",
            ));
        }
        Ok(Self {
            directory: directory.into(),
            repository_key: expected.key,
        })
    }

    fn request(&self, method: Method) -> io::Result<Reply> {
        accepted(transport::scoped_job_request(
            &self.directory,
            &self.repository_key,
            method,
        )?)
    }

    pub(crate) fn gc_plan(
        &self,
        request_key: &str,
        deadline_ms: i64,
        include_active_gate_cache: bool,
        full_scan: bool,
    ) -> io::Result<(crate::GcPlan, serde_json::Value)> {
        let remaining = deadline_remaining(deadline_ms)?;
        let deadline = Instant::now() + remaining;
        let operation = JobOperation::GcPlan {
            include_active_gate_cache,
            full_scan,
        };
        let reply = self.request(Method::Submit {
            idempotency_key: request_key.into(),
            operation: operation.clone(),
            deadline_ms,
        }).map_err(|error| io::Error::new(error.kind(), format!(
            "{error}; submission outcome may be unknown; retain --request-key {request_key:?} and --deadline-ms {deadline_ms}, and use service lookup before creating new work"
        )))?;
        let Reply::Submitted { mut job, .. } = reply else {
            return Err(invalid("unexpected shared submission reply"));
        };
        let job_id = job.id;
        loop {
            validate_status(&job, job_id, &operation, deadline_ms)?;
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "shared planning wait expired; the durable job was not cancelled; lookup the same request key",
                ));
            }
            match job.state {
                JobState::Succeeded => {
                    if job.completion != Some(wire::Completion::Succeeded) {
                        return Err(invalid("shared job has inconsistent completion"));
                    }
                    let result = read_result(
                        |method| {
                            if Instant::now() >= deadline {
                                return Err(io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    "shared result deadline expired",
                                ));
                            }
                            self.request(method)
                        },
                        job_id,
                    )?;
                    let plan = serde_json::from_value(result.clone()).map_err(invalid)?;
                    return Ok((plan, result));
                }
                JobState::Queued | JobState::Running => {}
                state => {
                    return Err(io::Error::other(format!(
                        "shared planning job {job_id} stopped as {state:?}: {:?}; no local execution was attempted",
                        job.completion
                    )));
                }
            }
            std::thread::sleep(
                Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
            );
            if Instant::now() >= deadline {
                continue;
            }
            let Reply::Status { job: next } = self.request(Method::Status { job_id })? else {
                return Err(invalid("unexpected shared status reply"));
            };
            job = next;
        }
    }
}

pub(crate) fn deadline_remaining(deadline_ms: i64) -> io::Result<Duration> {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_millis(),
    )
    .map_err(io::Error::other)?;
    deadline_remaining_at(now, deadline_ms)
}

fn deadline_remaining_at(now: i64, deadline_ms: i64) -> io::Result<Duration> {
    let remaining = deadline_ms
        .checked_sub(now)
        .filter(|ms| *ms > 0 && *ms <= 30 * 60 * 1000)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "shared planning requires a future deadline within 30 minutes",
            )
        })?;
    Ok(Duration::from_millis(remaining as u64))
}

fn validate_status(
    job: &Status,
    job_id: i64,
    operation: &JobOperation,
    deadline_ms: i64,
) -> io::Result<()> {
    if job_id <= 0
        || job.id != job_id
        || &job.operation != operation
        || job.deadline_ms != deadline_ms
    {
        return Err(invalid("shared job identity or inputs differ"));
    }
    Ok(())
}

/// Shared by the legacy manual result command and the repository-pinned client.
pub(crate) fn read_result(
    mut request: impl FnMut(Method) -> io::Result<Reply>,
    job_id: i64,
) -> io::Result<serde_json::Value> {
    let mut encoded = String::new();
    loop {
        let Reply::ResultChunk {
            job_id: returned_id,
            offset,
            next_offset,
            complete,
            data,
        } = request(Method::ResultChunk {
            job_id,
            offset: encoded.len(),
        })?
        else {
            return Err(invalid("unexpected result reply"));
        };
        if returned_id != job_id
            || offset != encoded.len()
            || next_offset != offset.saturating_add(data.len())
            || data.len() > wire::RESULT_CHUNK_BYTES
            || next_offset > 1024 * 1024
            || (!complete && data.is_empty())
        {
            return Err(invalid("invalid result chunk"));
        }
        encoded.push_str(&data);
        if complete {
            return serde_json::from_str(&encoded).map_err(invalid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planning_deadline_is_future_bounded_and_overflow_safe() {
        assert_eq!(
            deadline_remaining_at(100, 101).unwrap(),
            Duration::from_millis(1)
        );
        assert_eq!(
            deadline_remaining_at(100, 1_800_100).unwrap(),
            Duration::from_secs(1800)
        );
        for (now, deadline) in [
            (100, 100),
            (100, 99),
            (100, 1_800_101),
            (i64::MIN, i64::MAX),
        ] {
            assert_eq!(
                deadline_remaining_at(now, deadline).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn result_reader_preserves_json_and_checks_every_chunk_boundary() {
        let bytes = ["{\"future\":", "\"é\"}"];
        let mut calls = 0;
        let value = read_result(
            |method| {
                let Method::ResultChunk { job_id, offset } = method else {
                    panic!("unexpected method")
                };
                assert_eq!(job_id, 7);
                assert_eq!(
                    offset,
                    bytes[..calls].iter().map(|part| part.len()).sum::<usize>()
                );
                let data = bytes[calls].to_owned();
                calls += 1;
                Ok(Reply::ResultChunk {
                    job_id,
                    offset,
                    next_offset: offset + data.len(),
                    complete: calls == 2,
                    data,
                })
            },
            7,
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(value, serde_json::json!({"future": "é"}));
        for reply in [
            Reply::ResultChunk {
                job_id: 8,
                offset: 0,
                next_offset: 2,
                complete: true,
                data: "{}".into(),
            },
            Reply::ResultChunk {
                job_id: 7,
                offset: 1,
                next_offset: 3,
                complete: true,
                data: "{}".into(),
            },
            Reply::ResultChunk {
                job_id: 7,
                offset: 0,
                next_offset: 1,
                complete: true,
                data: "{}".into(),
            },
            Reply::ResultChunk {
                job_id: 7,
                offset: 0,
                next_offset: 0,
                complete: false,
                data: String::new(),
            },
            Reply::ResultChunk {
                job_id: 7,
                offset: 0,
                next_offset: wire::RESULT_CHUNK_BYTES + 1,
                complete: true,
                data: "x".repeat(wire::RESULT_CHUNK_BYTES + 1),
            },
        ] {
            assert_eq!(
                read_result(|_| Ok(reply.clone()), 7).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }
}
