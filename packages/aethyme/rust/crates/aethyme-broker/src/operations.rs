//! Durable coordination for Git and GitHub CLI operations.
//!
//! The broker does not reimplement either CLI. It fixes the executable,
//! classifies the requested argv, journals a redacted intent, serializes
//! repository writes with `flock`, and records the outcome. A process death
//! after the command starts becomes `outcome_unknown`; later writes fail
//! closed until an operator reconciles that journal row.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::broker::{Broker, BrokerOpError};
use crate::types::{
    CoordinatedOperation, NewCoordinatedOperation, OperationEffect, OperationIdentityProvenance,
    OperationProvider, OperationStatus,
};

/// How long an invocation is willing to queue for the repository write lock.
/// This is a property of the caller's patience, not of the command, so it is
/// passed per invocation rather than carried on the request (issue #138).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueueWait {
    /// Queue until the lock is free.
    #[default]
    Forever,
    /// Refuse immediately if the lock is held, so the caller can report honestly
    /// instead of parking.
    Refuse,
    /// Queue, but give up after this many seconds.
    Seconds(u64),
}

/// How long `--no-wait` admission may spend preparing before it gives up.
///
/// `--no-wait` says "refuse rather than queue", not "do no work": remote
/// resolution and a pre-push dry run still have to run, and against a healthy
/// remote they take seconds. A budget keeps "promptly" meaningful without
/// turning a slow network into a refusal on every call.
const NO_WAIT_ADMISSION_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// How often a bounded child is checked for completion.
const ADMISSION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Environment contract for wrapped commands that can report meaningful
/// progress. Each complete line is either a human-readable message or a JSON
/// object containing `message` and optional `phase` fields.
pub(crate) const BROKER_OPERATION_PROGRESS_ENV: &str = "AETHYME_BROKER_PROGRESS_FILE";
/// Liveness payload shape this build understands. A payload claiming a newer
/// version is reported as unknown rather than interpreted with these keys.
const OPERATION_LIVENESS_SCHEMA_VERSION: i64 = 1;

const OPERATION_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const OPERATION_STALL_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationLivenessView {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_age_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_age_seconds: Option<u64>,
}

/// Make the persisted liveness contract useful to status, operations list,
/// and blocked-call diagnostics without teaching each surface how to parse the
/// journal's free-form details JSON.
pub(crate) fn operation_liveness_view(operation: &CoordinatedOperation) -> OperationLivenessView {
    let liveness = operation
        .details_json
        .as_deref()
        .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
        .and_then(|details| details.get("operation_liveness").cloned());
    let heartbeat_at = liveness
        .as_ref()
        .and_then(|value| value.get("heartbeat_at"))
        .and_then(serde_json::Value::as_i64);
    let progress_at = liveness
        .as_ref()
        .and_then(|value| value.get("progress_at"))
        .and_then(serde_json::Value::as_i64);
    let now = unix_now_ms();
    let heartbeat_age_ms = heartbeat_at.map(|at| now.saturating_sub(at).max(0));
    let progress_age_ms = progress_at.map(|at| now.saturating_sub(at).max(0));
    // A payload this build cannot read says nothing about the holder, so it
    // must not be allowed to say the holder died. `schema_version` is written
    // by every writer; a newer one, or a missing timestamp, means "cannot
    // tell" -- and only a timestamp that is genuinely old means stale.
    // Absent means "written before the field existed", which this build reads
    // correctly; only a version from the future is genuinely unreadable. The
    // guard is against a later shape being misread with these keys, not
    // against the shape that predates the guard.
    let payload_readable = liveness
        .as_ref()
        .and_then(|value| value.get("schema_version"))
        .and_then(serde_json::Value::as_i64)
        .is_none_or(|version| version <= OPERATION_LIVENESS_SCHEMA_VERSION);
    let heartbeat_unreadable = !payload_readable || heartbeat_age_ms.is_none();
    let heartbeat_stale = heartbeat_age_ms
        .is_some_and(|age| age >= (OPERATION_HEARTBEAT_INTERVAL.as_millis() as i64 * 3));
    let progress_stale =
        progress_age_ms.is_none_or(|age| age >= OPERATION_STALL_AFTER.as_millis() as i64);
    let state = if operation.status != OperationStatus::Running {
        "not_running"
    } else if liveness.is_none() || heartbeat_unreadable {
        "unknown"
    } else if heartbeat_stale {
        "heartbeat_stale"
    } else if progress_stale {
        "progress_stale"
    } else {
        "active"
    };
    OperationLivenessView {
        state: state.into(),
        phase: liveness
            .as_ref()
            .and_then(|value| value.get("phase"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        progress: liveness
            .as_ref()
            .and_then(|value| value.get("progress"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        heartbeat_at,
        progress_at,
        heartbeat_age_seconds: heartbeat_age_ms.map(|age| age as u64 / 1_000),
        progress_age_seconds: progress_age_ms.map(|age| age as u64 / 1_000),
    }
}

pub(crate) fn operation_liveness_summary(operation: &CoordinatedOperation) -> String {
    let view = operation_liveness_view(operation);
    operation_liveness_view_summary(&view)
}

pub(crate) fn operation_liveness_view_summary(view: &OperationLivenessView) -> String {
    if view.state == "not_running" {
        return view.state.clone();
    }
    let heartbeat = view
        .heartbeat_age_seconds
        .map(humanize_duration)
        .unwrap_or_else(|| "unknown".into());
    let progress = view
        .progress_age_seconds
        .map(humanize_duration)
        .unwrap_or_else(|| "unknown".into());
    let phase = view.phase.as_deref().unwrap_or("unknown phase");
    let message = view.progress.as_deref().unwrap_or("no progress message");
    format!(
        "{}; phase {phase}; heartbeat {heartbeat} ago; progress {progress} ago ({message})",
        view.state
    )
}

/// Best-effort line protocol for wrapped commands. A downstream hook can
/// report progress without linking to the broker or opening its database.
pub(crate) fn emit_operation_progress(message: &str) {
    let Some(path) = std::env::var_os(BROKER_OPERATION_PROGRESS_ENV) else {
        return;
    };
    append_progress_event(Path::new(&path), message, None);
}

#[derive(Debug, Clone)]
struct OperationHeartbeatState {
    phase: String,
    progress: String,
    last_progress_at: i64,
    output_bytes: u64,
}

struct OperationHeartbeat {
    progress_file: tempfile::NamedTempFile,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    state: Arc<Mutex<OperationHeartbeatState>>,
    consumed: Arc<Mutex<usize>>,
}

impl OperationHeartbeat {
    fn start(db_path: &Path, operation_id: i64, phase: &str) -> Option<Self> {
        let run_dir = db_path.parent()?.join("run/operations");
        std::fs::create_dir_all(&run_dir).ok()?;
        let progress_file = tempfile::Builder::new()
            .prefix(&format!("operation-{operation_id}-"))
            .suffix(".progress")
            .tempfile_in(run_dir)
            .ok()?;
        let progress_path = progress_file.path().to_path_buf();
        let state = Arc::new(Mutex::new(OperationHeartbeatState {
            phase: phase.into(),
            progress: "provider command started".into(),
            last_progress_at: unix_now_ms(),
            output_bytes: 0,
        }));
        let consumed = Arc::new(Mutex::new(0usize));
        let (stop_tx, stop_rx) = mpsc::channel();
        let thread_state = Arc::clone(&state);
        let thread_consumed = Arc::clone(&consumed);
        let thread_db_path = db_path.to_path_buf();
        let thread = thread::Builder::new()
            .name(format!("aethyme-operation-heartbeat-{operation_id}"))
            .spawn(move || {
                let mut write_failures: u32 = 0;
                loop {
                    match stop_rx.recv_timeout(OPERATION_HEARTBEAT_INTERVAL) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                            if let Ok(mut consumed) = thread_consumed.lock() {
                                consume_progress_file(&progress_path, &mut consumed, &thread_state);
                            }
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if let Ok(mut consumed) = thread_consumed.lock() {
                        consume_progress_file(&progress_path, &mut consumed, &thread_state);
                    }
                    let now = unix_now_ms();
                    let snapshot = thread_state.lock().ok().map(|state| state.clone());
                    let Some(snapshot) = snapshot else {
                        continue;
                    };
                    let liveness = json!({
                        "schema_version": OPERATION_LIVENESS_SCHEMA_VERSION,
                        "write_failures": write_failures,
                        "phase": snapshot.phase,
                        "progress": snapshot.progress,
                        "heartbeat_at": now,
                        "progress_at": snapshot.last_progress_at,
                        "output_bytes": snapshot.output_bytes,
                        "heartbeat_interval_ms": OPERATION_HEARTBEAT_INTERVAL.as_millis(),
                        "stall_after_ms": OPERATION_STALL_AFTER.as_millis(),
                    });
                    // Persisting can fail for reasons that say nothing about
                    // this operation -- a sibling worktree's binary moved the
                    // schema, or SQLite stayed busy past its timeout. Those
                    // must not accumulate into "the holder died", so the count
                    // of consecutive failures rides along and a reader treats a
                    // gap it can explain as unknown rather than stale.
                    let persisted = crate::BrokerStore::open(&thread_db_path)
                        .ok()
                        .and_then(|mut store| {
                            store
                                .update_coordinated_operation_liveness(operation_id, &liveness)
                                .ok()
                        })
                        .is_some();
                    if persisted {
                        write_failures = 0;
                    } else {
                        write_failures = write_failures.saturating_add(1);
                    }
                }
            })
            .ok()?;
        let heartbeat = Self {
            progress_file,
            stop: Some(stop_tx),
            thread: Some(thread),
            state,
            consumed,
        };
        append_progress_event(heartbeat.progress_file.path(), phase, Some(phase));
        Some(heartbeat)
    }

    fn path(&self) -> &Path {
        self.progress_file.path()
    }

    fn liveness(&self, heartbeat_at: i64) -> serde_json::Value {
        let state = self
            .state
            .lock()
            .ok()
            .map(|state| state.clone())
            .unwrap_or_else(|| OperationHeartbeatState {
                phase: "unknown".into(),
                progress: "heartbeat state unavailable".into(),
                last_progress_at: heartbeat_at,
                output_bytes: 0,
            });
        json!({
            "schema_version": 1,
            "phase": state.phase,
            "progress": state.progress,
            "heartbeat_at": heartbeat_at,
            "progress_at": state.last_progress_at,
            "output_bytes": state.output_bytes,
            "heartbeat_interval_ms": OPERATION_HEARTBEAT_INTERVAL.as_millis(),
            "stall_after_ms": OPERATION_STALL_AFTER.as_millis(),
        })
    }

    fn finish(&mut self) -> serde_json::Value {
        if let Ok(mut consumed) = self.consumed.lock() {
            consume_progress_file(self.progress_file.path(), &mut consumed, &self.state);
        }
        self.stop();
        self.liveness(unix_now_ms())
    }

    fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for OperationHeartbeat {
    fn drop(&mut self) {
        self.stop();
    }
}

fn consume_progress_file(
    path: &Path,
    consumed: &mut usize,
    state: &Arc<Mutex<OperationHeartbeatState>>,
) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    if bytes.len() < *consumed {
        *consumed = 0;
    }
    let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return;
    };
    let complete_end = last_newline + 1;
    if complete_end <= *consumed {
        return;
    }
    // Returning here without advancing `consumed` would re-read the same
    // bytes on every tick and never get past them, freezing progress for the
    // rest of the run while valid lines keep arriving behind the bad one. A
    // gate echoing a non-UTF-8 path is enough to trigger it, so the undecodable
    // bytes are replaced rather than allowed to stop the stream.
    let text = String::from_utf8_lossy(&bytes[*consumed..complete_end]);
    let text = text.as_ref();
    for line in text.lines() {
        let value = serde_json::from_str::<serde_json::Value>(line).ok();
        let message = value
            .as_ref()
            .and_then(|value| value.get("message"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| (!line.trim().is_empty()).then_some(line.trim()));
        let Some(message) = message else {
            continue;
        };
        if let Ok(mut state) = state.lock() {
            state.progress = message.to_owned();
            state.last_progress_at = unix_now_ms();
            if let Some(phase) = value
                .as_ref()
                .and_then(|value| value.get("phase"))
                .and_then(serde_json::Value::as_str)
            {
                state.phase = phase.to_owned();
            }
        }
    }
    *consumed = complete_end;
}

fn append_progress_event(path: &Path, message: &str, phase: Option<&str>) {
    let event = json!({
        "schema_version": 1,
        "message": message,
        "phase": phase,
        "ts": unix_now_ms(),
    });
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{event}");
}

/// The wall-clock bound on everything an admission does before it holds the
/// repository lane.
///
/// `--queue-timeout` reads as a bound on the command returning, and that is how
/// callers use it. It bounded only the `flock`, while the preparation in front
/// of that lock -- remote resolution, a pre-push dry run that contacts the
/// network -- carried no deadline at all. A wedged remote therefore hung both
/// `--queue-timeout 60` and `--no-wait` (#219).
///
/// The budget is shared, not per-stage: whatever preparation spends is taken
/// out of what the lock may wait. Otherwise a 60 second request could still
/// park for 120.
#[derive(Debug, Clone, Copy)]
struct AdmissionDeadline {
    at: Option<std::time::Instant>,
    budget: Option<std::time::Duration>,
}

impl AdmissionDeadline {
    fn start(queue_wait: QueueWait) -> Self {
        let budget = match queue_wait {
            // Waiting forever is a deliberate choice; honour it rather than
            // inventing a bound the caller did not ask for.
            QueueWait::Forever => None,
            QueueWait::Refuse => Some(NO_WAIT_ADMISSION_BUDGET),
            QueueWait::Seconds(seconds) => Some(std::time::Duration::from_secs(seconds)),
        };
        Self {
            at: budget.map(|budget| std::time::Instant::now() + budget),
            budget,
        }
    }

    fn budget_label(&self) -> String {
        self.budget
            .map(|budget| humanize_duration(budget.as_secs()))
            .unwrap_or_else(|| "unbounded".into())
    }

    fn remaining(&self) -> Option<std::time::Duration> {
        self.at
            .map(|at| at.saturating_duration_since(std::time::Instant::now()))
    }

    fn expired(&self) -> bool {
        self.remaining().is_some_and(|left| left.is_zero())
    }

    /// Refuse before starting a stage whose budget is already gone. `stage`
    /// completes "while ...", so it reads as the thing that was being done.
    fn check(&self, repository: &str, stage: &str) -> Result<(), BrokerOpError> {
        if self.expired() {
            return Err(BrokerOpError::AdmissionTimedOut {
                repository: repository.into(),
                stage: stage.into(),
                budget: self.budget_label(),
            });
        }
        Ok(())
    }

    /// What the lock may still wait for, after preparation took its share.
    ///
    /// A caller who asked to refuse still refuses; a caller who asked to wait
    /// forever still waits. Only a bounded request is narrowed.
    fn remaining_queue_wait(&self, queue_wait: QueueWait) -> QueueWait {
        match queue_wait {
            QueueWait::Forever | QueueWait::Refuse => queue_wait,
            QueueWait::Seconds(_) => match self.remaining() {
                // Zero would read as "wait none" and silently become a refusal
                // with the wrong error; the caller checks expiry before this.
                Some(left) => QueueWait::Seconds(left.as_secs().max(1)),
                None => queue_wait,
            },
        }
    }
}

/// Run a child to completion, or kill it when the admission budget is gone.
///
/// The output readers run concurrently with the child. Apart from avoiding a
/// pipe-capacity deadlock, that gives the operation heartbeat a meaningful
/// progress signal while a provider is still working.
fn output_within(
    mut command: Command,
    deadline: AdmissionDeadline,
    repository: &str,
    stage: &str,
    heartbeat: Option<&OperationHeartbeat>,
) -> Result<std::process::Output, BrokerOpError> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| BrokerOpError::OperationIo {
            path: PathBuf::from("git"),
            source,
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| BrokerOpError::OperationIo {
            path: PathBuf::from("git"),
            source: std::io::Error::other("child stdout was not piped"),
        })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| BrokerOpError::OperationIo {
            path: PathBuf::from("git"),
            source: std::io::Error::other("child stderr was not piped"),
        })?;
    let stdout_state = heartbeat.map(|heartbeat| Arc::clone(&heartbeat.state));
    let stderr_state = heartbeat.map(|heartbeat| Arc::clone(&heartbeat.state));
    let stdout_reader = spawn_output_reader(stdout, stdout_state, "provider stdout");
    let stderr_reader = spawn_output_reader(stderr, stderr_state, "provider stderr");

    let status = loop {
        if deadline.expired() {
            // Killing is the point: leaving it behind would keep contacting the
            // remote after the caller was told nothing happened.
            let _ = child.kill();
            let _ = child.wait();
            // A provider may have grandchildren that inherited the pipes.
            // Joining here would make a bounded operation wait for those
            // grandchildren even after the provider itself was killed. Drop
            // the handles and let those readers end when their inherited
            // descriptors close.
            drop(stdout_reader);
            drop(stderr_reader);
            return Err(BrokerOpError::AdmissionTimedOut {
                repository: repository.into(),
                stage: stage.into(),
                budget: deadline.budget_label(),
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(stdout_reader);
                drop(stderr_reader);
                return Err(BrokerOpError::OperationIo {
                    path: PathBuf::from("git"),
                    source,
                });
            }
        }
        std::thread::sleep(ADMISSION_POLL_INTERVAL);
    };
    let stdout = join_output_reader(stdout_reader)?;
    let stderr = join_output_reader(stderr_reader)?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn spawn_output_reader<R>(
    mut reader: R,
    state: Option<Arc<Mutex<OperationHeartbeatState>>>,
    stream: &'static str,
) -> JoinHandle<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0u8; 8 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..read]);
            if let Some(state) = &state
                && let Ok(mut state) = state.lock()
            {
                state.progress = format!("{stream} output received");
                state.last_progress_at = unix_now_ms();
                state.output_bytes = state.output_bytes.saturating_add(read as u64);
            }
        }
        Ok(output)
    })
}

fn join_output_reader(
    reader: JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, BrokerOpError> {
    match reader.join() {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(source)) => Err(BrokerOpError::OperationIo {
            path: PathBuf::from("git"),
            source,
        }),
        Err(_) => Err(BrokerOpError::OperationIo {
            path: PathBuf::from("git"),
            source: std::io::Error::other("child output reader panicked"),
        }),
    }
}

#[derive(Debug, Clone)]
pub struct CoordinatedCommand {
    pub session_id: i64,
    pub provider: OperationProvider,
    /// Required for GitHub operations and remote Git operations. `owner/repo`.
    pub repository: Option<String>,
    /// Broker-resolved identity for an internal remote Git workflow. This is
    /// distinct from the caller's `--repo owner/name` assertion.
    pub resolved_target: Option<crate::ResolvedRemoteTarget>,
    /// Audit scope. V1 deliberately locks the whole repository regardless.
    pub scope: Option<String>,
    pub declared_effect: Option<OperationEffect>,
    pub destructive_confirmed: bool,
    /// Required for writes; identifies the user request or documented workflow.
    pub authorization_reason: Option<String>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CoordinatedOperationReport {
    pub operation: CoordinatedOperation,
    pub classification: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_target: Option<crate::ResolvedRemoteTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_target: Option<crate::ResolvedGithubTarget>,
    pub command_success: bool,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_merge_cleanup: Option<PostMergeCleanupReport>,
    /// Exactly what a successful push sent. Empty for anything that was not a
    /// plannable push.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pushed_refs: Vec<PushedRef>,
    /// Set when a successful merge was observed to carry this session's work
    /// onto the default branch, so the session can finish without resubmitting.
    pub representing_commit: Option<String>,
    /// Pull request this operation created, when it created one. Recorded so
    /// the session that opened it can be told about review activity without a
    /// human first noticing the number (#150). The watch is *not* started
    /// here: doing so would poll the provider while this operation still holds
    /// the repository write lock, which is the head-of-line stall of #138.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_pull_request: Option<i64>,
}

impl CoordinatedOperationReport {
    pub fn ok(&self) -> bool {
        self.operation.status == OperationStatus::Succeeded
    }

    /// What external inspection concluded about a create that exited non-zero.
    ///
    /// The ambiguity #184 reports is "did it create the issue or not", and by
    /// the time this is rendered the journal already holds the answer. Saying
    /// it out loud is the difference between having an answer and having a
    /// record of one. `None` where there is nothing to say: a command that
    /// creates nothing, or an outcome still unknown -- which
    /// [`Self::unknown_outcome_recovery`] reports far more loudly.
    pub fn create_outcome(&self) -> Option<String> {
        let details: serde_json::Value =
            serde_json::from_str(self.operation.details_json.as_deref()?).ok()?;
        let evidence = details.get("create_reconciliation")?.get("evidence")?;
        match evidence.get("classification")?.as_str()? {
            "succeeded" => Some(
                match evidence.get("created").and_then(serde_json::Value::as_str) {
                    Some(created) => {
                        format!("the command failed, but it had already created {created}")
                    }
                    None => "the command failed, but it had already created the resource".into(),
                },
            ),
            "failed" => {
                Some("the command failed and the repository shows nothing was created".into())
            }
            _ => None,
        }
    }

    pub fn unknown_outcome_recovery(&self) -> Option<UnknownOutcomeRecovery> {
        (self.operation.status == OperationStatus::OutcomeUnknown)
            .then(|| UnknownOutcomeRecovery::from_operation(&self.operation))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PostMergeCleanupState {
    Cleaned,
    NotNeeded,
    Deferred,
}

impl PostMergeCleanupState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cleaned => "cleaned",
            Self::NotNeeded => "not_needed",
            Self::Deferred => "deferred",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PostMergeCleanupReport {
    pub state: PostMergeCleanupState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch_operation_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<crate::AutomaticIntegrationCleanupReport>,
    pub explanation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ExactPushDestination {
    destination_ref: String,
    pre_push_sha: Option<String>,
    proposed_sha: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ExactPushPlan {
    remote: String,
    destinations: Vec<ExactPushDestination>,
}

/// What a successful push actually sent, per destination.
///
/// The planner already resolves this to answer "did the push send what the
/// dry run inspected"; reporting it closes the gap that let a `HEAD:` refspec
/// publish an unintended commit under a success line that named neither (#269).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PushedRef {
    pub destination_ref: String,
    pub proposed_sha: String,
}

#[derive(Debug, Clone)]
enum PushPlanning {
    NotApplicable,
    Unsupported { reason: &'static str },
    Unavailable { reason: &'static str },
    Planned(ExactPushPlan),
}

impl PushPlanning {
    /// Whether this plan sends exactly what an earlier plan described.
    ///
    /// Compared by destination and proposed commit, which is precisely what the
    /// pre-push hook inspected during the dry run. Anything the planner cannot
    /// describe exactly is treated as not covered, so an unplannable push never
    /// reaches the remote with its hook skipped.
    fn matches_prechecked(&self, earlier: &PushPlanning) -> bool {
        match (self, earlier) {
            (Self::Planned(now), Self::Planned(before)) => {
                now.remote == before.remote
                    && now.destinations.len() == before.destinations.len()
                    && now.destinations.iter().zip(&before.destinations).all(
                        |(current, earlier)| {
                            current.destination_ref == earlier.destination_ref
                                && current.proposed_sha == earlier.proposed_sha
                        },
                    )
            }
            _ => false,
        }
    }

    fn journal_value(&self) -> Option<serde_json::Value> {
        match self {
            Self::NotApplicable => None,
            Self::Unsupported { reason } => Some(json!({
                "planning": "unsupported",
                "reason": reason,
            })),
            Self::Unavailable { reason } => Some(json!({
                "planning": "unavailable",
                "reason": reason,
            })),
            Self::Planned(plan) => Some(json!({
                "planning": "planned",
                "plan": plan,
            })),
        }
    }
}

/// Complete operator handoff for a write whose external outcome is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownOutcomeRecovery {
    pub canonical_repository: String,
    pub operation_id: i64,
    pub provider: OperationProvider,
    pub scope: String,
    pub remote_write: bool,
}

impl UnknownOutcomeRecovery {
    pub fn from_operation(operation: &CoordinatedOperation) -> Self {
        Self {
            canonical_repository: operation.repository.clone(),
            operation_id: operation.id,
            provider: operation.provider,
            scope: operation.scope.clone(),
            remote_write: operation.host_operation_id.is_some(),
        }
    }

    fn inspection_instruction(&self) -> String {
        match self.provider {
            OperationProvider::Git if !self.remote_write => format!(
                "Inspect local Git refs and worktree state for {} at scope {} to determine whether the write took effect.",
                self.canonical_repository, self.scope
            ),
            OperationProvider::Git => format!(
                "Inspect remote Git refs for canonical repository {} at scope {} to determine whether the write took effect.",
                self.canonical_repository, self.scope
            ),
            OperationProvider::Github => format!(
                "Inspect GitHub state for canonical repository {} at scope {} to determine whether the write took effect.",
                self.canonical_repository, self.scope
            ),
        }
    }

    pub fn succeeded_command(&self) -> String {
        format!(
            "aethyme broker operations reconcile --operation {} --outcome succeeded --reason \"external inspection confirmed operation {} took effect\"",
            self.operation_id, self.operation_id
        )
    }

    pub fn failed_command(&self) -> String {
        format!(
            "aethyme broker operations reconcile --operation {} --outcome failed --reason \"external inspection confirmed operation {} did not take effect\"",
            self.operation_id, self.operation_id
        )
    }
}

impl fmt::Display for UnknownOutcomeRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "Canonical repository {} is now write-blocked because a coordinated write has an unknown outcome.",
            self.canonical_repository
        )?;
        writeln!(formatter, "Operation ID: {}", self.operation_id)?;
        writeln!(formatter, "{}", self.inspection_instruction())?;
        writeln!(
            formatter,
            "If external inspection proves the write succeeded, run:"
        )?;
        writeln!(formatter, "  {}", self.succeeded_command())?;
        writeln!(
            formatter,
            "If external inspection proves the write failed, run:"
        )?;
        writeln!(formatter, "  {}", self.failed_command())?;
        write!(
            formatter,
            "Blind retry is forbidden until operation {} is reconciled.",
            self.operation_id
        )
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationReconcileReport {
    pub operation: CoordinatedOperation,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationReconciliationState {
    NotRequired,
    Required,
    ReconciledSucceeded,
    ReconciledFailed,
}

impl OperationReconciliationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Required => "required",
            Self::ReconciledSucceeded => "reconciled_succeeded",
            Self::ReconciledFailed => "reconciled_failed",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationReconciliationRecovery {
    pub inspection: String,
    pub succeeded_command: String,
    pub failed_command: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationReconciliation {
    pub state: OperationReconciliationState,
    pub required: bool,
    pub write_blocked: bool,
    /// The broker never turns an inspection result into an automatic retry.
    pub automatic_retry_allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<OperationReconciliationRecovery>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationShowReport {
    pub operation: CoordinatedOperation,
    pub reconciliation: OperationReconciliation,
}

impl OperationShowReport {
    fn from_operation(operation: CoordinatedOperation) -> Self {
        let details = operation
            .details_json
            .as_deref()
            .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok());
        let state = match operation.status {
            OperationStatus::OutcomeUnknown => OperationReconciliationState::Required,
            OperationStatus::ReconciledSucceeded => {
                OperationReconciliationState::ReconciledSucceeded
            }
            OperationStatus::ReconciledFailed => OperationReconciliationState::ReconciledFailed,
            _ => OperationReconciliationState::NotRequired,
        };
        let recovery = (state == OperationReconciliationState::Required).then(|| {
            let recovery = UnknownOutcomeRecovery::from_operation(&operation);
            OperationReconciliationRecovery {
                inspection: recovery.inspection_instruction(),
                succeeded_command: recovery.succeeded_command(),
                failed_command: recovery.failed_command(),
            }
        });
        let evidence = details
            .as_ref()
            .and_then(|details| {
                details
                    .get("push_reconciliation")
                    .or_else(|| details.get("create_reconciliation"))
            })
            .cloned();
        let operator_reason = details.as_ref().and_then(|details| {
            details
                .get("reconciliation")
                .and_then(|reconciliation| reconciliation.get("operator_reason"))
                .or_else(|| details.get("operator_reason"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
        Self {
            operation,
            reconciliation: OperationReconciliation {
                state,
                required: state == OperationReconciliationState::Required,
                write_blocked: state == OperationReconciliationState::Required,
                automatic_retry_allowed: false,
                evidence,
                operator_reason,
                recovery,
            },
        }
    }
}

struct RepositoryWriteLock {
    file: File,
    acquired_at: Instant,
    acquired_at_ms: i64,
    queue_wait_ms: i64,
}

impl RepositoryWriteLock {
    fn acquire(
        main_root: &Path,
        repository: &str,
        operation_id: i64,
        mut describe_holder: impl FnMut() -> Result<String, BrokerOpError>,
        queue_wait: QueueWait,
    ) -> Result<Self, BrokerOpError> {
        let dir = main_root.join(".aethyme/locks/operations");
        std::fs::create_dir_all(&dir).map_err(|source| BrokerOpError::OperationIo {
            path: dir.clone(),
            source,
        })?;
        let path = dir.join(format!("{:016x}.lock", stable_hash(repository.as_bytes())));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| BrokerOpError::OperationIo {
                path: path.clone(),
                source,
            })?;
        // Try without blocking first, so the uncontended path stays a single
        // syscall and the holder lookup only runs when it can actually help.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Self {
                file,
                acquired_at: Instant::now(),
                acquired_at_ms: unix_now_ms(),
                queue_wait_ms: 0,
            });
        }
        let would_block = std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK);
        if !would_block {
            return Err(BrokerOpError::OperationIo {
                path,
                source: std::io::Error::last_os_error(),
            });
        }
        if queue_wait == QueueWait::Refuse {
            return Err(BrokerOpError::CoordinatedLockBusy {
                repository: repository.into(),
                holder: describe_holder()?,
                waited: "not waited for".into(),
                operation_id,
            });
        }
        // A coordinated operation that simply pauses is indistinguishable from one
        // that died. Saying what holds the lock, and for how long, is what makes
        // the difference visible to the caller (issue #138).
        let holder = describe_holder()?;
        eprintln!(
            "[coordination] waiting for the {repository} write lock: {}",
            holder
        );
        let waited = std::time::Instant::now();
        match queue_wait {
            QueueWait::Refuse => unreachable!("refused above"),
            QueueWait::Forever => {
                let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
                if rc != 0 {
                    return Err(BrokerOpError::OperationIo {
                        path,
                        source: std::io::Error::last_os_error(),
                    });
                }
            }
            // No portable timed flock, so poll: the deadline is the caller's, and
            // giving up honestly beats parking past it.
            QueueWait::Seconds(seconds) => {
                let deadline = waited + std::time::Duration::from_secs(seconds);
                loop {
                    let rc =
                        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                    if rc == 0 {
                        break;
                    }
                    if std::io::Error::last_os_error().raw_os_error() != Some(libc::EWOULDBLOCK) {
                        return Err(BrokerOpError::OperationIo {
                            path,
                            source: std::io::Error::last_os_error(),
                        });
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(BrokerOpError::CoordinatedLockBusy {
                            repository: repository.into(),
                            holder: describe_holder()?,
                            waited: humanize_duration(waited.elapsed().as_secs()),
                            operation_id,
                        });
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        eprintln!(
            "[coordination] acquired the {repository} write lock after {}",
            humanize_duration(waited.elapsed().as_secs())
        );
        Ok(Self {
            file,
            acquired_at: Instant::now(),
            acquired_at_ms: unix_now_ms(),
            queue_wait_ms: waited.elapsed().as_millis() as i64,
        })
    }

    fn hold_duration_ms(&self) -> i64 {
        self.acquired_at.elapsed().as_millis() as i64
    }
}

/// Probe with signal 0: reports whether a process can be signalled without
/// disturbing it. Conservative about PID reuse -- a recycled PID reads as alive,
/// which forgoes a cleanup rather than resolving a live operation out from under
/// the process still running it.
pub(crate) fn process_is_gone(pid: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return false;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// Opt-in: run a push's local hooks before taking the repository lock.
///
/// The lock exists to order remote mutations, but `git push` runs the
/// repository's `pre-push` hook inside its own process, so holding the lock
/// across the command holds it across that hook too. On a repository whose
/// pre-push gate is legitimately long, the fleet then serialises on whoever is
/// pushing the largest change (issues #138, #146).
///
/// Off by default because it changes what the push verifies: the hook runs
/// against the same commits in a dry run, and the real push is then made with
/// `--no-verify`. That is sound only because the broker re-plans under the lock
/// and refuses if any local ref moved in between -- but it does skip any *other*
/// pre-push protection the repository relies on, which is the repository
/// owner's decision to make, not ours.
fn hooks_outside_lock_enabled(main_root: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(main_root.join(".aethyme/config.toml")) else {
        return false;
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return false;
    };
    value
        .get("coordination")
        .and_then(|section| section.get("hooks_outside_lock"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}

/// Opt-in measurement of the provider read a future ref-scoped merge would
/// need. The result is deliberately not used to choose today's lock: this
/// probe measures the cost without changing the conservative repository-wide
/// policy or making a merge depend on a second provider call.
fn measure_pr_merge_ref_enabled(main_root: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(main_root.join(".aethyme/config.toml")) else {
        return false;
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return false;
    };
    value
        .get("coordination")
        .and_then(|section| section.get("measure_pr_merge_ref"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy)]
struct RefDeterminationMeasurement {
    duration_ms: i64,
    succeeded: bool,
}

fn measure_pr_merge_ref_determination(
    main_root: &Path,
    cwd: &Path,
    args: &[String],
    github_target: Option<&crate::ResolvedGithubTarget>,
) -> Option<RefDeterminationMeasurement> {
    if !measure_pr_merge_ref_enabled(main_root) || !is_github_pull_request_merge(args) {
        return None;
    }
    let selector = first_positional(args.get(2..)?)?;
    let target = github_target?;
    let started = Instant::now();
    let mut command = provider_command(OperationProvider::Github);
    command
        .args(["pr", "view", selector, "--json", "baseRefName"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("GH_REPO", &target.display_slug);
    let succeeded = command
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    Some(RefDeterminationMeasurement {
        duration_ms: started.elapsed().as_millis() as i64,
        succeeded,
    })
}

fn is_push(args: &[String]) -> bool {
    git_subcommand_args(args)
        .and_then(|args| args.first())
        .is_some_and(|command| command == "push")
}

pub(crate) fn humanize_duration(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        _ => format!("{}m {}s", seconds / 60, seconds % 60),
    }
}

fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn add_coordination_timing(
    details: &mut serde_json::Value,
    lock_key: &str,
    lock_wait_started_at: Option<i64>,
    lock: Option<&RepositoryWriteLock>,
    hooks_outside_lock: bool,
    ref_determination: Option<RefDeterminationMeasurement>,
) {
    let Some(details) = details.as_object_mut() else {
        return;
    };
    let (lock_acquired_at, queue_wait_ms, lock_hold_ms) = match lock {
        Some(lock) => (
            Some(lock.acquired_at_ms),
            Some(lock.queue_wait_ms),
            Some(lock.hold_duration_ms()),
        ),
        None => (None, None, None),
    };
    details.insert(
        "coordination_timing".into(),
        json!({
            "schema_version": 1,
            "lock_key": lock_key,
            "lock_wait_started_at": lock_wait_started_at,
            "lock_acquired_at": lock_acquired_at,
            "lock_released_at": lock.map(|_| unix_now_ms()),
            "queue_wait_ms": queue_wait_ms,
            "lock_hold_ms": lock_hold_ms,
            "hooks_outside_lock": hooks_outside_lock,
            "ref_determination_ms": ref_determination.map(|measurement| measurement.duration_ms),
            "ref_determination_succeeded": ref_determination.map(|measurement| measurement.succeeded),
        }),
    );
}

#[derive(Debug, Clone)]
struct LockHolderInfo {
    description: String,
    operation_id: Option<i64>,
    session_id: Option<i64>,
    pid: Option<i64>,
    scope: Option<String>,
    started_at: Option<i64>,
}

/// The holder is whichever operation on this repository is recorded as
/// running. An operation that has not registered yet is reported as such
/// rather than as "no holder", because the lock is demonstrably held by
/// someone.
fn lock_holder_info(store: &mut crate::BrokerStore, repository: &str) -> LockHolderInfo {
    let now = unix_now_ms();
    let running = store
        .unresolved_coordinated_operations(repository)
        .ok()
        .and_then(|operations| {
            operations
                .into_iter()
                .find(|operation| operation.status == OperationStatus::Running)
        });
    match running {
        Some(operation) => LockHolderInfo {
            description: format!(
                "operation {} (session {}, {} {}) has held it for {}; {}",
                operation.id,
                operation.session_id,
                operation.provider.as_str(),
                operation.scope,
                humanize_duration(now.saturating_sub(operation.created_at).max(0) as u64 / 1_000),
                operation_liveness_summary(&operation),
            ),
            operation_id: Some(operation.id),
            session_id: Some(operation.session_id),
            pid: Some(operation.pid),
            scope: Some(operation.scope),
            started_at: Some(operation.created_at),
        },
        None => LockHolderInfo {
            description: "held by an operation that has not recorded itself yet".into(),
            operation_id: None,
            session_id: None,
            pid: None,
            scope: None,
            started_at: None,
        },
    }
}

fn coordination_wait_details(
    holder: &LockHolderInfo,
    enqueued_at: i64,
    waiting_started_at: i64,
) -> String {
    json!({
        "coordination_wait": {
            "schema_version": 1,
            "reason": "repository_write_lock",
            "holder_description": holder.description.as_str(),
            "holder": {
                "operation_id": holder.operation_id,
                "session_id": holder.session_id,
                "pid": holder.pid,
                "scope": holder.scope.as_deref(),
                "started_at": holder.started_at,
            },
            "enqueued_at": enqueued_at,
            "waiting_started_at": waiting_started_at,
            "waited_ms": unix_now_ms().saturating_sub(waiting_started_at).max(0),
        }
    })
    .to_string()
}

impl Drop for RepositoryWriteLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn validate_repository(value: &str) -> Result<(), BrokerOpError> {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    let valid_component = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    };
    if !valid_component(owner) || !valid_component(name) || parts.next().is_some() {
        return Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: format!("repository must be an exact owner/name slug, got {value:?}"),
        });
    }
    Ok(())
}

fn validate_scope(value: &str) -> Result<(), BrokerOpError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(|ch| matches!(ch, '\n' | '\r')) {
        return Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: "scope must be 1-256 characters without newlines".into(),
        });
    }
    Ok(())
}

fn validate_authorization_reason(value: Option<&str>) -> Result<Option<String>, BrokerOpError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() || value.len() > 500 || value.chars().any(|ch| matches!(ch, '\n' | '\r')) {
        return Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: "--reason must be 1-500 characters without newlines".into(),
        });
    }
    Ok(Some(value.into()))
}

fn effect_rank(effect: OperationEffect) -> u8 {
    match effect {
        OperationEffect::Read => 0,
        OperationEffect::Write => 1,
        OperationEffect::Destructive => 2,
    }
}

fn resolve_effect(
    inferred: Option<OperationEffect>,
    declared: Option<OperationEffect>,
) -> Result<(OperationEffect, &'static str), BrokerOpError> {
    match (inferred, declared) {
        (Some(inferred), Some(declared)) if effect_rank(declared) < effect_rank(inferred) => {
            Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "--effect {} cannot downgrade inferred {} operation",
                    declared.as_str(),
                    inferred.as_str()
                ),
            })
        }
        (Some(_), Some(declared)) => Ok((declared, "declared")),
        (Some(inferred), None) => Ok((inferred, "inferred")),
        // An unrecognized command may be an alias for anything, including a
        // push, so the caller cannot vouch that it only reads.
        (None, Some(OperationEffect::Read)) => Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: "unrecognized command: check the command name first (a typo, or a \
                     repeated `git`/`gh`); only if it is intentional, declare --effect write \
                     or --effect destructive, because it cannot be declared --effect read"
                .into(),
        }),
        (None, Some(declared)) => Ok((declared, "declared")),
        (None, None) => Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: "operation is ambiguous; declare --effect write|destructive and --scope".into(),
        }),
    }
}

fn has_any(args: &[String], needles: &[&str]) -> bool {
    args.iter().any(|arg| needles.contains(&arg.as_str()))
}

/// True when a bundled short-option argument such as `-fdx` or `-uf`
/// includes `flag`. Matching `has_any` against `-f` alone misses bundles.
fn has_short_flag(args: &[String], flag: char) -> bool {
    args.iter().any(|arg| {
        arg.strip_prefix('-').is_some_and(|flags| {
            !flags.is_empty()
                && !flags.starts_with('-')
                && flags.chars().all(|ch| ch.is_ascii_alphabetic())
                && flags.contains(flag)
        })
    })
}

/// Positional refspecs led by `+` force-update their destination.
fn has_forced_refspec(args: &[String]) -> bool {
    args.iter()
        .skip(1)
        .any(|arg| arg.starts_with('+') && arg.len() > 1)
}

/// Config keys that make Git run a program or reinterpret a command name.
/// Set inline for a coordinated operation, they would run arbitrary code or
/// disguise a push as an unrecognized command.
const CODE_EXECUTING_CONFIG_PREFIXES: &[&str] = &["alias.", "includeif.", "filter.", "credential."];
const CODE_EXECUTING_CONFIG_KEYS: &[&str] = &[
    "core.hookspath",
    "core.sshcommand",
    "core.fsmonitor",
    "core.pager",
    "core.editor",
    "core.askpass",
    "sequence.editor",
    "include.path",
    "gpg.program",
    "diff.external",
    "uploadpack.packobjectshook",
];

fn config_key_executes_code(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    CODE_EXECUTING_CONFIG_PREFIXES
        .iter()
        .any(|prefix| key.starts_with(prefix))
        || CODE_EXECUTING_CONFIG_KEYS.contains(&key.as_str())
}

/// `broker git -- git push` runs `git git push`. Refuse it plainly: otherwise
/// the command reads as an unrecognized subcommand, the caller raises its
/// declared effect to get past that, and the failed write then blocks the
/// repository as an unknown outcome (seen twice on 2026-09-23).
pub(crate) fn refuse_repeated_program_name(
    provider: OperationProvider,
    args: &[String],
) -> Result<(), BrokerOpError> {
    let (program, first) = match provider {
        OperationProvider::Git => (
            "git",
            git_subcommand_index(args).and_then(|index| args.get(index)),
        ),
        OperationProvider::Github => ("gh", args.first()),
    };
    if first.is_some_and(|arg| arg == program) {
        return Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: format!(
                "the broker already runs `{program}`; drop the leading `{program}` after `--` \
                 (write `broker {program} ... -- <args>`, not `-- {program} <args>`)"
            ),
        });
    }
    Ok(())
}

/// Refuse global options that change what a coordinated Git command executes.
pub(crate) fn refuse_code_executing_git_options(args: &[String]) -> Result<(), BrokerOpError> {
    let refuse = |what: &str| {
        Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: format!(
                "{what} is refused for coordinated git: it can make git run another program \
                 or treat a push as an unrecognized command"
            ),
        })
    };
    let end = git_subcommand_index(args).unwrap_or(args.len());
    let mut index = 0;
    while index < end {
        let arg = args[index].as_str();
        let (key, consumed) = if arg == "-c" || arg == "--config-env" {
            (args.get(index + 1).map(String::as_str).unwrap_or(""), 2)
        } else if let Some(inline) = arg.strip_prefix("--config-env=") {
            (inline, 1)
        } else if let Some(inline) = arg.strip_prefix("-c") {
            (inline, 1)
        } else if arg == "--exec-path" || arg.starts_with("--exec-path=") {
            return refuse("--exec-path");
        } else {
            index += 1;
            continue;
        };
        let key = key.split('=').next().unwrap_or("");
        if config_key_executes_code(key) {
            return refuse(&format!("config key `{key}`"));
        }
        index += consumed;
    }
    Ok(())
}

/// The subset of Git's global options that can appear before its subcommand.
/// Unknown leading options stay unclassified so a mutating command cannot be
/// treated as a definitely local failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitGlobalOption {
    Flag,
    Value,
    Directory,
}

fn git_global_option(arg: &str) -> Option<GitGlobalOption> {
    match arg {
        "-C" => Some(GitGlobalOption::Directory),
        "-c" | "--exec-path" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix"
        | "--config-env" => Some(GitGlobalOption::Value),
        "--paginate"
        | "--no-pager"
        | "--no-replace-objects"
        | "--no-lazy-fetch"
        | "--no-optional-locks"
        | "--no-advice"
        | "--literal-pathspecs"
        | "--glob-pathspecs"
        | "--noglob-pathspecs"
        | "--icase-pathspecs"
        | "--html-path"
        | "--man-path"
        | "--info-path"
        | "-p"
        | "-P" => Some(GitGlobalOption::Flag),
        _ if arg.starts_with("-C") && arg.len() > 2 => Some(GitGlobalOption::Directory),
        _ if arg.starts_with("-c") && arg.len() > 2 => Some(GitGlobalOption::Value),
        _ if arg.starts_with("--exec-path=")
            || arg.starts_with("--git-dir=")
            || arg.starts_with("--work-tree=")
            || arg.starts_with("--namespace=")
            || arg.starts_with("--super-prefix=")
            || arg.starts_with("--config-env=") =>
        {
            Some(GitGlobalOption::Value)
        }
        _ => None,
    }
}

fn git_subcommand_index(args: &[String]) -> Option<usize> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if arg == "--" {
            return args.get(index + 1).map(|_| index + 1);
        }
        if arg == "--version" {
            return Some(index);
        }
        match git_global_option(arg) {
            Some(GitGlobalOption::Flag) => index += 1,
            Some(kind @ (GitGlobalOption::Value | GitGlobalOption::Directory)) => {
                let has_inline_value = match kind {
                    GitGlobalOption::Value => {
                        arg.starts_with("-c") && arg.len() > 2 || arg.contains('=')
                    }
                    GitGlobalOption::Directory => arg.starts_with("-C") && arg.len() > 2,
                    GitGlobalOption::Flag => false,
                };
                if has_inline_value {
                    index += 1;
                } else if args.get(index + 1).is_some_and(|value| !value.is_empty()) {
                    index += 2;
                } else {
                    return None;
                }
            }
            None if arg.starts_with('-') => return None,
            None => return Some(index),
        }
    }
    None
}

fn git_subcommand_args(args: &[String]) -> Option<&[String]> {
    git_subcommand_index(args).and_then(|index| args.get(index..))
}

fn git_explicit_directory(args: &[String], cwd: &Path) -> Result<Option<PathBuf>, BrokerOpError> {
    let mut index = 0;
    let mut directory = cwd.to_path_buf();
    let mut explicit = false;
    while let Some(arg) = args.get(index) {
        if arg == "--" {
            break;
        }
        match git_global_option(arg) {
            Some(GitGlobalOption::Directory) => {
                let path = if arg.len() > 2 {
                    &arg[2..]
                } else {
                    args.get(index + 1).ok_or_else(|| {
                        BrokerOpError::InvalidCoordinatedOperation {
                            reason: "git -C requires a non-empty checkout path".into(),
                        }
                    })?
                };
                if path.is_empty() {
                    return Err(BrokerOpError::InvalidCoordinatedOperation {
                        reason: "git -C requires a non-empty checkout path".into(),
                    });
                }
                let path = Path::new(path);
                directory = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    directory.join(path)
                };
                explicit = true;
                index += if arg.len() > 2 { 1 } else { 2 };
            }
            Some(GitGlobalOption::Flag) => index += 1,
            Some(GitGlobalOption::Value) => {
                let has_inline_value = arg.starts_with("-c") && arg.len() > 2 || arg.contains('=');
                index += if has_inline_value { 1 } else { 2 };
            }
            None => break,
        }
    }
    Ok(explicit.then_some(directory))
}

pub fn classify_git(args: &[String]) -> Option<OperationEffect> {
    let args = git_subcommand_args(args)?;
    let command = args.first()?.as_str();
    match command {
        "status" | "diff" | "log" | "show" | "rev-parse" | "ls-files" | "ls-tree" | "cat-file"
        | "grep" | "blame" | "describe" | "shortlog" | "whatchanged" | "merge-base"
        | "name-rev" | "for-each-ref" | "check-ignore" | "count-objects" | "fsck" | "help"
        | "ls-remote" | "version" | "--version" | "rev-list" | "show-ref" | "show-branch"
        | "cherry" | "range-diff" | "var" | "check-attr" | "check-ref-format" | "verify-commit"
        | "verify-tag" | "diff-tree" | "diff-index" | "diff-files" => Some(OperationEffect::Read),
        "config" => {
            let reads = has_any(
                args,
                &[
                    "--get",
                    "--get-all",
                    "--get-regexp",
                    "--get-urlmatch",
                    "--list",
                    "-l",
                ],
            ) || matches!(args.get(1).map(String::as_str), Some("get" | "list"));
            Some(if reads {
                OperationEffect::Read
            } else {
                OperationEffect::Write
            })
        }
        "update-ref" => Some(
            if has_short_flag(args, 'd') || has_any(args, &["--stdin"]) {
                OperationEffect::Destructive
            } else {
                OperationEffect::Write
            },
        ),
        "send-pack" => Some(
            if has_any(args, &["--force", "--mirror"]) || has_forced_refspec(args) {
                OperationEffect::Destructive
            } else {
                OperationEffect::Write
            },
        ),
        "branch" => {
            if has_any(args, &["--delete", "--force"])
                || ['d', 'D', 'f', 'M', 'C']
                    .into_iter()
                    .any(|flag| has_short_flag(args, flag))
            {
                Some(OperationEffect::Destructive)
            } else if args.len() == 1
                || has_any(
                    args,
                    &[
                        "-l",
                        "--list",
                        "-a",
                        "--all",
                        "-r",
                        "--remotes",
                        "-v",
                        "-vv",
                        "--verbose",
                        "--show-current",
                        "--contains",
                        "--no-contains",
                        "--merged",
                        "--no-merged",
                    ],
                )
            {
                Some(OperationEffect::Read)
            } else {
                Some(OperationEffect::Write)
            }
        }
        "tag" => {
            if has_any(args, &["--delete", "--force"])
                || has_short_flag(args, 'd')
                || has_short_flag(args, 'f')
            {
                Some(OperationEffect::Destructive)
            } else if args.len() == 1 || has_any(args, &["-l", "--list", "--contains"]) {
                Some(OperationEffect::Read)
            } else {
                Some(OperationEffect::Write)
            }
        }
        "remote" => {
            if args.len() == 1
                || has_any(args, &["-v", "--verbose", "get-url", "show"])
                    && !has_any(
                        args,
                        &["add", "remove", "rename", "set-url", "prune", "update"],
                    )
            {
                Some(OperationEffect::Read)
            } else if has_any(args, &["remove", "rm", "set-url"]) {
                Some(OperationEffect::Destructive)
            } else {
                Some(OperationEffect::Write)
            }
        }
        "push" => {
            let destructive = args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "--force" | "--force-with-lease" | "--delete" | "--mirror" | "--prune"
                ) || arg.starts_with("--force-with-lease=")
                    || (arg.starts_with(':') && arg.len() > 1)
            }) || has_short_flag(args, 'f')
                || has_short_flag(args, 'd')
                || has_forced_refspec(args);
            Some(if destructive {
                OperationEffect::Destructive
            } else {
                OperationEffect::Write
            })
        }
        "reset" if has_any(args, &["--hard", "--merge", "--keep"]) => {
            Some(OperationEffect::Destructive)
        }
        "clean"
            if has_any(args, &["--force"])
                || ['f', 'd', 'x', 'X']
                    .into_iter()
                    .any(|flag| has_short_flag(args, flag)) =>
        {
            Some(OperationEffect::Destructive)
        }
        "reflog" if has_any(args, &["delete", "expire"]) => Some(OperationEffect::Destructive),
        "add" | "am" | "apply" | "checkout" | "cherry-pick" | "clone" | "commit" | "fetch"
        | "gc" | "init" | "merge" | "mv" | "notes" | "pull" | "rebase" | "replace" | "restore"
        | "revert" | "rm" | "stash" | "submodule" | "switch" | "worktree" | "reset" | "clean"
        | "reflog" => Some(OperationEffect::Write),
        _ => None,
    }
}

fn gh_method(args: &[String]) -> Option<&str> {
    args.windows(2)
        .find(|pair| matches!(pair[0].as_str(), "-X" | "--method"))
        .map(|pair| pair[1].as_str())
        .or_else(|| args.iter().find_map(|arg| arg.strip_prefix("--method=")))
        // `-XDELETE`: the method bundled into the flag, as curl also accepts.
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix("-X").filter(|method| !method.is_empty()))
        })
}

pub fn classify_gh(args: &[String]) -> Option<OperationEffect> {
    let command = args.first()?.as_str();
    let action = args.get(1).map(String::as_str);
    if command == "api" {
        let method = gh_method(args).unwrap_or_else(|| {
            if has_any(args, &["-f", "--raw-field", "-F", "--field", "--input"]) {
                "POST"
            } else {
                "GET"
            }
        });
        return Some(match method.to_ascii_uppercase().as_str() {
            "GET" | "HEAD" => OperationEffect::Read,
            "DELETE" => OperationEffect::Destructive,
            _ => OperationEffect::Write,
        });
    }
    let read_actions = [
        "list", "view", "status", "diff", "checks", "watch", "download", "get", "token",
    ];
    let destructive_actions = ["delete", "remove", "archive"];
    match command {
        "search" | "status" | "browse" | "--version" | "version" => Some(OperationEffect::Read),
        "auth" => match action {
            Some("status" | "token") => Some(OperationEffect::Read),
            Some("login" | "logout" | "refresh" | "setup-git") => Some(OperationEffect::Write),
            _ => None,
        },
        "pr" | "issue" | "run" | "workflow" | "release" | "repo" | "secret" | "variable"
        | "label" | "cache" | "codespace" | "ssh-key" | "gpg-key" => {
            if action.is_some_and(|value| destructive_actions.contains(&value)) {
                Some(OperationEffect::Destructive)
            } else if action.is_some_and(|value| read_actions.contains(&value)) {
                Some(OperationEffect::Read)
            } else if action.is_some() {
                Some(OperationEffect::Write)
            } else {
                None
            }
        }
        "project" => match action {
            Some("list" | "view" | "item-list" | "field-list") => Some(OperationEffect::Read),
            Some(_) => Some(OperationEffect::Write),
            None => None,
        },
        "extension" | "alias" | "config" => None,
        _ => None,
    }
}

/// `gh` flags that consume the following argument, so a positional scan does
/// not mistake a flag's value for the command's own operand.
const GH_VALUE_FLAGS: &[&str] = &[
    "-X",
    "--method",
    "-f",
    "--raw-field",
    "-F",
    "--field",
    "-H",
    "--header",
    "-q",
    "--jq",
    "-t",
    "--template",
    "--input",
    "--hostname",
    "--cache",
    "-b",
    "--body",
    "--body-file",
    "-R",
    "--repo",
    "--title",
];

/// First operand that is not a flag or a flag's value.
///
/// `--flag=value` is skipped by the leading-dash test; a bare `--flag value`
/// pair is skipped by `GH_VALUE_FLAGS`. Anything unrecognized falls through to
/// the caller, which treats a parse it cannot explain as repository-wide.
fn first_positional(args: &[String]) -> Option<&str> {
    let mut skip_value = false;
    for arg in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        if GH_VALUE_FLAGS.contains(&arg.as_str()) {
            skip_value = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.as_str());
    }
    None
}

/// Per-resource coordination scope for a provider write that provably mutates
/// no Git ref and no repository setting.
///
/// `None` means repository-wide, and is the conservative default: every
/// command not on this closed list keeps the historical lock, as does any
/// spelling this cannot parse with certainty. Widening the list is a decision
/// about what "touches no ref" means; narrowing it is always safe.
///
/// Derived from the command line alone. That is the property that makes it
/// usable: the scope must be known *before* the operation queues, so resolving
/// it cannot involve a provider round-trip (#181). A comment or review write
/// has no affected ref, and the resource it does touch is already an operand.
fn resource_lock_scope(provider: OperationProvider, args: &[String]) -> Option<String> {
    if provider != OperationProvider::Github {
        return None;
    }
    match args.first()?.as_str() {
        command @ ("pr" | "issue") => {
            let resource = match (command, args.get(1)?.as_str()) {
                (_, "comment") => "comments",
                ("pr", "review") => "reviews",
                _ => return None,
            };
            let collection = if command == "pr" { "pull" } else { "issue" };
            // A selector that is not a plain number may be a URL or a branch,
            // which this cannot resolve without asking the provider.
            let number: u64 = first_positional(args.get(2..)?)?.parse().ok()?;
            Some(format!("{collection}/{number}/{resource}"))
        }
        "api" => api_resource_scope(args),
        _ => None,
    }
}

/// Resource scope for the `gh api` spelling of a comment or review write.
///
/// Only the exact six-segment collection endpoints qualify. A longer path
/// reaches something else (a reaction, a single comment's replies), and a
/// shorter one is a collection this has no opinion about.
fn api_resource_scope(args: &[String]) -> Option<String> {
    let path = first_positional(args.get(1..)?)?;
    // A query string addresses the same resource; anything after `?` only
    // filters or paginates it.
    let path = path.split('?').next()?.trim_matches('/');
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() != 6 || segments[0] != "repos" {
        return None;
    }
    let number: u64 = segments[4].parse().ok()?;
    match (segments[3], segments[5]) {
        ("pulls", "reviews") => Some(format!("pull/{number}/reviews")),
        ("pulls", "comments") => Some(format!("pull/{number}/comments")),
        ("issues", "comments") => Some(format!("issue/{number}/comments")),
        _ => None,
    }
}

/// The key two operations must share before they serialise against each other.
///
/// Repository-wide operations keep the bare canonical repository, so #166's
/// "one repository, one lock" still holds for everything that can touch a ref.
/// A resource-scoped operation gets a sub-key derived from that same
/// canonical string, so no spelling difference can fragment it either.
///
/// `::` is deliberate: `validate_remote_key` rejects `#`, `@`, `?` and `://`
/// as credential or URL syntax, and `owner/repo` cannot contain a colon.
fn coordination_lock_key(repository: &str, resource: Option<&str>) -> String {
    match resource {
        Some(resource) => format!("{repository}::{resource}"),
        None => repository.to_string(),
    }
}

/// Recompute a recorded operation's resource scope from its stored command.
///
/// `redacted_command` hides flag *values* only, so every operand this reads
/// survives redaction. Recomputing beats trusting the stored `scope` column:
/// that column accepts a caller-declared string, and a caller must never be
/// able to name its way out of the repository lock.
fn stored_resource_scope(operation: &CoordinatedOperation) -> Option<String> {
    let command: Vec<String> = serde_json::from_str(&operation.command_json).ok()?;
    resource_lock_scope(operation.provider, command.get(1..)?)
}

/// Whether a parsed Git command can have changed a remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitOperationKind {
    Local,
    Remote,
    Unknown,
}

fn git_operation_kind(args: &[String]) -> GitOperationKind {
    let Some(args) = git_subcommand_args(args) else {
        return GitOperationKind::Unknown;
    };
    if matches!(
        args.first().map(String::as_str),
        Some("clone" | "fetch" | "pull" | "push" | "ls-remote" | "submodule")
    ) || (args.first().map(String::as_str) == Some("remote")
        && args
            .get(1)
            .is_some_and(|arg| matches!(arg.as_str(), "show" | "prune" | "update")))
    {
        GitOperationKind::Remote
    } else if classify_git(args).is_some() {
        GitOperationKind::Local
    } else {
        GitOperationKind::Unknown
    }
}

/// Coordination key for a Git command that resolves no remote of its own.
///
/// `None` means the checkout has no single usable `origin`. That is a fallback,
/// not a failure: a repository whose remote cannot be resolved cannot be the
/// target of a remote operation either, so no other spelling can collide with
/// whatever the caller falls back to.
///
/// A caller assertion that contradicts `origin` is refused rather than accepted
/// under its own private key, which is the rule remote commands already apply.
fn canonical_local_repository(
    cwd: &Path,
    assertion: Option<&str>,
) -> Result<Option<String>, BrokerOpError> {
    let Ok(repo) = crate::GitRepo::discover(cwd) else {
        return Ok(None);
    };
    match repo.resolve_remote_target("origin", assertion) {
        Ok(target) => Ok(Some(target.coordination_key)),
        Err(crate::RemoteTargetError::AssertionMismatch {
            assertion,
            resolved,
        }) => Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: format!(
                "--repo {assertion:?} does not match the repository this checkout's origin identifies ({resolved:?})"
            ),
        }),
        Err(_) => Ok(None),
    }
}

fn journal_details(
    classification: &'static str,
    resolved_target: Option<&crate::ResolvedRemoteTarget>,
    github_target: Option<&crate::ResolvedGithubTarget>,
    extra: serde_json::Value,
) -> serde_json::Value {
    let mut details = json!({ "classification": classification });
    if let Some(target) = resolved_target {
        details["resolved_target"] = json!(target);
    }
    if let Some(target) = github_target {
        details["github_target"] = json!(target);
    }
    if let (Some(details), Some(extra)) = (details.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            details.insert(key.clone(), value.clone());
        }
    }
    details
}

fn add_operation_liveness(details: &mut serde_json::Value, liveness: serde_json::Value) {
    if let Some(details) = details.as_object_mut() {
        details.insert("operation_liveness".into(), liveness);
    }
}

fn with_push_planning(mut extra: serde_json::Value, planning: &PushPlanning) -> serde_json::Value {
    if let (Some(extra), Some(push)) = (extra.as_object_mut(), planning.journal_value()) {
        extra.insert("push_reconciliation".into(), push);
    }
    extra
}

/// Push sources that mean something different in each worktree.
///
/// Refs under `refs/` are shared by every worktree of a repository, so
/// `main:refs/heads/x` resolves identically wherever the command runs. `HEAD`
/// does not -- it is per-worktree state. `broker git` executes inside the
/// *session* worktree rather than the caller's, so a `HEAD:` refspec sent from
/// somewhere else silently publishes the session's commit under the caller's
/// chosen branch name, and the push reports success (#269).
pub(crate) fn worktree_relative_push_sources(args: &[String]) -> Vec<String> {
    let Some(args) = git_subcommand_args(args) else {
        return Vec::new();
    };
    if args.first().map(String::as_str) != Some("push") {
        return Vec::new();
    }
    args.iter()
        .skip(1)
        .filter(|argument| !argument.starts_with('-'))
        .filter(|argument| {
            let refspec = argument.strip_prefix('+').unwrap_or(argument);
            let source = refspec.split(':').next().unwrap_or(refspec);
            let base = source.split(['~', '^']).next().unwrap_or(source);
            base == "HEAD" || base == "@" || base.starts_with("@{")
        })
        .cloned()
        .collect()
}

/// Whether `candidate` is the session worktree or lives inside it.
///
/// Compared after canonicalization so a symlinked temporary directory -- the
/// normal shape of a scratch checkout on macOS -- is not mistaken for a
/// different tree.
pub(crate) fn is_within(candidate: &Path, root: &Path) -> bool {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical(candidate).starts_with(canonical(root))
}

fn plan_exact_push(
    cwd: &Path,
    args: &[String],
    target: Option<&crate::ResolvedRemoteTarget>,
) -> PushPlanning {
    let Some(args) = git_subcommand_args(args) else {
        return PushPlanning::NotApplicable;
    };
    if args.first().map(String::as_str) != Some("push") {
        return PushPlanning::NotApplicable;
    }
    let Some(target) = target else {
        return PushPlanning::Unsupported {
            reason: "push_target_is_not_a_resolved_remote",
        };
    };
    if args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--all"
                | "--delete"
                | "--dry-run"
                | "--follow-tags"
                | "--mirror"
                | "--prune"
                | "--tags"
        )
    }) {
        return PushPlanning::Unsupported {
            reason: "push_uses_implicit_or_set_expanding_options",
        };
    }
    let remote_positions = args
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(index, argument)| (argument == &target.remote_name).then_some(index))
        .collect::<Vec<_>>();
    let [remote_index] = remote_positions.as_slice() else {
        return PushPlanning::Unsupported {
            reason: "push_remote_position_is_not_unique",
        };
    };
    let refspecs = &args[*remote_index + 1..];
    if refspecs.is_empty()
        || refspecs
            .iter()
            .any(|refspec| refspec.starts_with('-') || refspec == "--")
    {
        return PushPlanning::Unsupported {
            reason: "push_does_not_have_only_explicit_refspecs",
        };
    }

    let Ok(repo) = crate::GitRepo::discover(cwd) else {
        return PushPlanning::Unavailable {
            reason: "local_repository_evidence_unavailable",
        };
    };
    let mut seen_destinations = BTreeSet::new();
    let mut destinations = Vec::with_capacity(refspecs.len());
    for refspec in refspecs {
        let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
        // A refspec without a colon pushes the named ref to the ref of the
        // same name on the remote, so its destination is derivable locally.
        // Resolving it here is what lets a failed `push origin <branch>`,
        // `push -u origin HEAD`, or `push origin refs/tags/<tag>` be
        // classified from remote evidence instead of being reported as an
        // unknown outcome that write-blocks the repository.
        let resolved_destination;
        let (source, destination) = match refspec.split_once(':') {
            Some(pair) => pair,
            None => {
                let Some(full) = repo.full_ref_name(refspec) else {
                    return PushPlanning::Unsupported {
                        reason: "push_refspec_does_not_resolve_to_one_local_ref",
                    };
                };
                resolved_destination = full;
                (refspec, resolved_destination.as_str())
            }
        };
        if source.is_empty()
            || destination.is_empty()
            || source.starts_with('-')
            || source.contains(':')
            || destination.contains(':')
            || !destination.starts_with("refs/")
            || !seen_destinations.insert(destination.to_string())
            || repo.validate_push_destination(destination).is_err()
        {
            return PushPlanning::Unsupported {
                reason: "push_refspec_is_not_one_unique_full_destination",
            };
        }
        let Ok(proposed_sha) = repo.resolve_push_source(source) else {
            return PushPlanning::Unavailable {
                reason: "push_source_object_is_unavailable",
            };
        };
        if proposed_sha.len() != 40 || !proposed_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return PushPlanning::Unavailable {
                reason: "push_source_object_is_not_a_full_sha",
            };
        }
        destinations.push(ExactPushDestination {
            destination_ref: destination.into(),
            pre_push_sha: None,
            proposed_sha: proposed_sha.to_ascii_lowercase(),
        });
    }

    let destination_refs = destinations
        .iter()
        .map(|destination| destination.destination_ref.clone())
        .collect::<Vec<_>>();
    let Ok(pre_push) = repo.remote_ref_oids(&target.remote_name, &destination_refs) else {
        return PushPlanning::Unavailable {
            reason: "pre_push_remote_evidence_unavailable",
        };
    };
    for destination in &mut destinations {
        destination.pre_push_sha = pre_push
            .get(&destination.destination_ref)
            .cloned()
            .flatten();
    }
    PushPlanning::Planned(ExactPushPlan {
        remote: target.remote_name.clone(),
        destinations,
    })
}

fn reconcile_failed_push(
    cwd: &Path,
    planning: &PushPlanning,
    remote_contact: Option<RemoteContactEvidence>,
) -> Option<(OperationStatus, serde_json::Value)> {
    let PushPlanning::Planned(plan) = planning else {
        return planning.journal_value().map(|mut value| {
            value["evidence"] = json!({
                "classification": "unknown",
                "reason": "exact_push_plan_unavailable",
            });
            (OperationStatus::OutcomeUnknown, value)
        });
    };
    let Ok(repo) = crate::GitRepo::discover(cwd) else {
        let mut value = planning.journal_value().expect("planned push");
        value["evidence"] = json!({
            "classification": "unknown",
            "reason": "local_repository_evidence_unavailable",
        });
        return Some((OperationStatus::OutcomeUnknown, value));
    };
    let destination_refs = plan
        .destinations
        .iter()
        .map(|destination| destination.destination_ref.clone())
        .collect::<Vec<_>>();
    let Ok(observed) = repo.remote_ref_oids(&plan.remote, &destination_refs) else {
        let mut value = planning.journal_value().expect("planned push");
        value["evidence"] = json!({
            "classification": "unknown",
            "reason": "post_push_remote_evidence_unavailable",
        });
        return Some((OperationStatus::OutcomeUnknown, value));
    };

    let mut all_pre_push = true;
    let mut all_proposed = true;
    let mut every_observation_is_expected = true;
    let observations = plan
        .destinations
        .iter()
        .map(|destination| {
            let observed_sha = observed
                .get(&destination.destination_ref)
                .cloned()
                .flatten();
            all_pre_push &= observed_sha == destination.pre_push_sha;
            all_proposed &= observed_sha.as_deref() == Some(destination.proposed_sha.as_str());
            every_observation_is_expected &= observed_sha == destination.pre_push_sha
                || observed_sha.as_deref() == Some(destination.proposed_sha.as_str());
            json!({
                "destination_ref": destination.destination_ref,
                "observed_sha": observed_sha,
            })
        })
        .collect::<Vec<_>>();
    let (status, classification) = if all_proposed {
        (OperationStatus::Succeeded, "succeeded")
    } else if all_pre_push {
        (OperationStatus::Failed, "failed")
    } else if every_observation_is_expected {
        (OperationStatus::OutcomeUnknown, "partial")
    } else {
        (OperationStatus::OutcomeUnknown, "unknown")
    };
    let mut value = planning.journal_value().expect("planned push");
    value["evidence"] = json!({
        "classification": classification,
        "destinations": observations,
    });
    if let Some(remote_contact) = remote_contact {
        value["evidence"]["remote_contact"] = json!(remote_contact.remote_contact);
        value["evidence"]["remote_write_contact"] = json!(remote_contact.remote_write_contact);
        value["evidence"]["remote_not_contacted"] = json!(remote_contact.remote_not_contacted);
    }
    Some((status, value))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RemoteContactEvidence {
    remote_contact: &'static str,
    remote_write_contact: &'static str,
    remote_not_contacted: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct GitTransferTrace {
    pre_push_hook: bool,
    pre_push_hook_failed: bool,
    remote_transport: bool,
}

impl GitTransferTrace {
    fn remote_contact(self) -> Option<RemoteContactEvidence> {
        if self.pre_push_hook_failed {
            Some(RemoteContactEvidence {
                remote_contact: if self.remote_transport {
                    "contacted"
                } else {
                    "not_contacted"
                },
                remote_write_contact: "not_contacted",
                remote_not_contacted: !self.remote_transport,
            })
        } else if self.remote_transport {
            Some(RemoteContactEvidence {
                remote_contact: "contacted",
                remote_write_contact: "unknown",
                remote_not_contacted: false,
            })
        } else {
            None
        }
    }
}

fn inspect_git_transfer_trace(path: &Path) -> GitTransferTrace {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return GitTransferTrace::default();
    };
    let mut pre_push_child_ids = BTreeSet::new();
    let mut trace = GitTransferTrace::default();
    for line in contents.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match event.get("event").and_then(serde_json::Value::as_str) {
            Some("child_start") => {
                let Some(arguments) = event
                    .get("argv")
                    .or_else(|| event.get("child").and_then(|child| child.get("argv")))
                else {
                    continue;
                };
                let Some(arguments) = arguments.as_array() else {
                    continue;
                };
                let command = arguments
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_ascii_lowercase();
                if command.contains("pre-push") {
                    trace.pre_push_hook = true;
                    if let Some(child_id) =
                        event.get("child_id").and_then(serde_json::Value::as_u64)
                    {
                        pre_push_child_ids.insert(child_id);
                    }
                }
                trace.remote_transport |= ["receive-pack", "upload-pack", "git-remote-", "ssh"]
                    .iter()
                    .any(|marker| command.contains(marker));
            }
            Some("child_exit")
                if event
                    .get("child_id")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|child_id| pre_push_child_ids.contains(&child_id))
                    && event
                        .get("code")
                        .and_then(serde_json::Value::as_i64)
                        .is_some_and(|code| code != 0) =>
            {
                trace.pre_push_hook_failed = true;
            }
            _ => {}
        }
    }
    trace
}

/// The `(command, action)` pair a `gh` invocation names.
///
/// Flags may precede the subcommand (`--repo o/n issue create`), and dropping
/// every `-` token would leave a flag's *value* looking positional. So match an
/// adjacent pair neither of whose halves is a flag, and require that the first
/// half is not itself the value of a preceding flag -- the same rule
/// [`crate::creates_pull_request`] applies to one hard-coded pair, generalised.
fn gh_subcommand(args: &[String]) -> Option<(&str, &str)> {
    args.windows(2).enumerate().find_map(|(index, pair)| {
        (!pair[0].starts_with('-')
            && !pair[1].starts_with('-')
            && (index == 0 || !args[index - 1].starts_with('-')))
        .then(|| (pair[0].as_str(), pair[1].as_str()))
    })
}

/// Every value a repeatable `gh` flag carries, in either spelling.
///
/// A spelling this does not recognise -- `pflag` also accepts the attached
/// shorthand `-lbug` -- yields no value, and every caller is written so that no
/// value means "behave as if this check did not exist". Missing a label is
/// then the status quo `gh` already reports; inventing one would refuse a
/// command that was going to work.
fn gh_flag_values<'a>(args: &'a [String], long: &str, short: Option<&str>) -> Vec<&'a str> {
    let long_assigned = format!("{long}=");
    let short_assigned = short.map(|short| format!("{short}="));
    let mut values = Vec::new();
    let mut pending = false;
    for arg in args {
        if pending {
            values.push(arg.as_str());
            pending = false;
        } else if arg == long || short.is_some_and(|short| arg == short) {
            pending = true;
        } else if let Some(value) = arg.strip_prefix(&long_assigned) {
            values.push(value);
        } else if let Some(value) = short_assigned
            .as_deref()
            .and_then(|prefix| arg.strip_prefix(prefix))
        {
            values.push(value);
        }
    }
    values
}

/// `gh` flags whose value must already name a label in the repository.
const GH_LABEL_FLAGS: &[(&str, Option<&str>)] = &[
    ("--label", Some("-l")),
    ("--add-label", None),
    ("--remove-label", None),
];

/// How many label names a refusal spells out before summarising the rest.
const LABEL_VOCABULARY_PREVIEW: usize = 40;

/// The labels a `gh` write asks GitHub to resolve by name.
///
/// `gh` resolves these against the repository *before* it creates anything and
/// refuses the whole command when one is unknown, so the vocabulary is a
/// precondition of the write rather than a part of it that could half-apply
/// (#184). Reads are excluded by the caller for a different reason: `--label`
/// on `issue list` is a filter, where an unknown name returns nothing instead
/// of failing.
///
/// One flag may carry several names -- `gh` parses these as comma-separated
/// lists -- and may be repeated.
fn gh_requested_labels(args: &[String]) -> Vec<String> {
    if !matches!(
        gh_subcommand(args),
        Some(("issue" | "pr", "create" | "edit"))
    ) {
        return Vec::new();
    }
    let mut labels: Vec<String> = Vec::new();
    for (long, short) in GH_LABEL_FLAGS {
        for value in gh_flag_values(args, long, *short) {
            for name in value.split(',') {
                let name = name.trim();
                if !name.is_empty() && !labels.iter().any(|seen| seen == name) {
                    labels.push(name.to_string());
                }
            }
        }
    }
    labels
}

/// The repository's label vocabulary, or `None` when it cannot be read.
///
/// Unreadable is not empty, and neither is a refusal. A machine that is
/// offline, unauthenticated or rate-limited may still be one where the write
/// itself would work, and turning that into "unknown label" would refuse valid
/// commands for a reason that has nothing to do with labels. The caller
/// degrades to the behaviour that existed before this check -- `gh` reports the
/// unknown name itself -- and the reconciliation below then says whether
/// anything was created.
fn github_label_vocabulary(repository: &str, cwd: &Path) -> Option<Vec<String>> {
    let output = provider_command(OperationProvider::Github)
        .args([
            "label", "list", "--repo", repository, "--limit", "500", "--json", "name",
        ])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    Some(
        parsed
            .as_array()?
            .iter()
            .filter_map(|label| label["name"].as_str().map(str::to_string))
            .collect(),
    )
}

/// Refuse a labelled write naming a label the repository does not define.
///
/// Refusing is the whole point: the caller runs this before anything is
/// journaled, queued or sent, so "was the issue created?" has exactly one
/// answer -- which the message states outright rather than leaving to be
/// inferred (#184).
fn refuse_undefined_labels(
    args: &[String],
    repository: &str,
    cwd: &Path,
) -> Result<(), BrokerOpError> {
    let requested = gh_requested_labels(args);
    if requested.is_empty() {
        return Ok(());
    }
    let Some(vocabulary) = github_label_vocabulary(repository, cwd) else {
        return Ok(());
    };
    // GitHub matches label names without regard to case, so refusing on case
    // alone would reject a name the command was going to apply.
    let unknown = requested
        .iter()
        .filter(|name| {
            !vocabulary
                .iter()
                .any(|known| known.eq_ignore_ascii_case(name))
        })
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        return Ok(());
    }
    let noun = if unknown.len() == 1 {
        "label"
    } else {
        "labels"
    };
    let unknown = unknown.join(", ");
    let mut defined = vocabulary;
    defined.sort();
    let remainder = defined.len().saturating_sub(LABEL_VOCABULARY_PREVIEW);
    let defined = if defined.is_empty() {
        "it defines none".to_string()
    } else {
        let listed = defined
            .iter()
            .take(LABEL_VOCABULARY_PREVIEW)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        match remainder {
            0 => listed,
            more => format!("{listed}, and {more} more"),
        }
    };
    Err(BrokerOpError::InvalidCoordinatedOperation {
        reason: format!(
            "{repository} does not define the {noun} {unknown}, and `gh` resolves labels before it \
             creates anything -- so nothing was sent and nothing was created. Labels defined \
             there: {defined}"
        ),
    })
}

/// How many entries a post-failure listing reads back.
const CREATE_OBSERVATION_LIMIT: usize = 50;

/// A `gh` command that would have GitHub assign a new number.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CreatePlanning {
    NotApplicable,
    Unplannable { reason: &'static str },
    Planned(CreatePlan),
}

/// What makes a created resource findable after a run that exited non-zero.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct CreatePlan {
    /// The `gh` subcommand, which is also the collection to list.
    collection: String,
    /// The listing field whose value names this particular create.
    identity_field: String,
    identity: String,
    /// The highest number the repository had already assigned.
    ///
    /// A create that lands takes a number strictly greater than every number
    /// the repository had, so this one integer separates the resource this run
    /// created from one that merely looks like it. `None` means the
    /// observation failed, which is what keeps such an outcome unknown.
    watermark: Option<i64>,
}

impl CreatePlanning {
    fn journal_value(&self) -> Option<serde_json::Value> {
        match self {
            Self::NotApplicable => None,
            Self::Unplannable { reason } => Some(json!({
                "planning": "unplannable",
                "reason": reason,
            })),
            Self::Planned(plan) => Some(json!({
                "planning": "planned",
                "plan": plan,
            })),
        }
    }
}

/// Plan how a failed `gh issue create` / `gh pr create` would be recognised.
///
/// Identity is the title for an issue and the head branch for a pull request.
/// Neither is unique on its own, which is why the number watermark taken in
/// [`observe_create_watermark`] is what makes the pair conclusive.
///
/// Reading the checkout's branch is not a guess: it is the same thing `gh pr
/// create` does with an omitted `--head`. A detached HEAD has no branch to
/// read, and says so rather than proposing the literal `HEAD`.
fn plan_github_create(args: &[String], cwd: &Path) -> CreatePlanning {
    match gh_subcommand(args) {
        Some(("issue", "create")) => match gh_flag_values(args, "--title", Some("-t")).first() {
            Some(title) => CreatePlanning::Planned(CreatePlan {
                collection: "issue".into(),
                identity_field: "title".into(),
                identity: (*title).to_string(),
                watermark: None,
            }),
            None => CreatePlanning::Unplannable {
                reason: "issue_create_without_an_explicit_title",
            },
        },
        Some(("pr", "create")) => {
            let head = gh_flag_values(args, "--head", Some("-H"))
                .first()
                .map(|head| (*head).to_string())
                .or_else(|| {
                    crate::GitRepo::discover(cwd)
                        .ok()?
                        .current_branch()
                        .ok()
                        .filter(|branch| branch != "HEAD")
                });
            match head {
                Some(head) => CreatePlanning::Planned(CreatePlan {
                    collection: "pr".into(),
                    identity_field: "headRefName".into(),
                    identity: head,
                    watermark: None,
                }),
                None => CreatePlanning::Unplannable {
                    reason: "pull_request_create_without_a_resolvable_head",
                },
            }
        }
        _ => CreatePlanning::NotApplicable,
    }
}

/// One page of `gh <collection> list`, newest first.
fn github_list_within(
    collection: &str,
    repository: &str,
    fields: &[&str],
    limit: usize,
    cwd: &Path,
    deadline: AdmissionDeadline,
    stage: &str,
) -> Result<Option<Vec<serde_json::Value>>, BrokerOpError> {
    let limit = limit.to_string();
    let fields = fields.join(",");
    let mut command = provider_command(OperationProvider::Github);
    command
        .args([
            collection, "list", "--repo", repository, "--state", "all", "--limit", &limit,
            "--json", &fields,
        ])
        .current_dir(cwd);
    let output = output_within(command, deadline, repository, stage, None)?;
    if !output.status.success() {
        return Ok(None);
    }
    let parsed: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(None),
    };
    Ok(parsed.as_array().cloned())
}

/// Record the number the repository stands at before a create runs.
///
/// Costs one listing, and only for a create whose result could be recognised
/// at all. Taking it afterwards would be worthless: the whole question is
/// which numbers are new.
fn observe_create_watermark(
    planning: &mut CreatePlanning,
    repository: &str,
    cwd: &Path,
    deadline: AdmissionDeadline,
) -> Result<(), BrokerOpError> {
    let CreatePlanning::Planned(plan) = planning else {
        return Ok(());
    };
    plan.watermark = github_list_within(
        &plan.collection,
        repository,
        &["number"],
        1,
        cwd,
        deadline,
        "observing GitHub create watermark",
    )?
    .map(|listed| {
        // A repository with nothing in the collection yet has no number, and 0
        // is below every number GitHub assigns.
        listed
            .first()
            .and_then(|entry| entry["number"].as_i64())
            .unwrap_or(0)
    });
    Ok(())
}

/// The URL `gh` prints for a resource it just created in this repository.
///
/// Scoped to the asserted repository, so an unrelated link in the output -- one
/// quoted by an error message, one in a template -- cannot be read as proof
/// that something was created.
fn created_resource_url(stdout: &str, repository: &str) -> Option<String> {
    let mut segments = repository.rsplit('/');
    let name = segments.next()?;
    let owner = segments.next()?;
    let needle = format!("/{owner}/{name}/").to_ascii_lowercase();
    stdout
        .split_whitespace()
        .find(|token| {
            let token = token.to_ascii_lowercase();
            token.starts_with("https://")
                && token.contains(&needle)
                && ["/issues/", "/pull/"].iter().any(|collection| {
                    token.rsplit_once(collection).is_some_and(|(_, number)| {
                        !number.is_empty() && number.chars().all(|digit| digit.is_ascii_digit())
                    })
                })
        })
        .map(str::to_string)
}

/// Decide whether a failed `gh` create nevertheless created something.
///
/// The same shape as [`reconcile_failed_push`]: observe external state, then
/// classify, and record what was observed. Two independent kinds of evidence
/// answer it and the stronger one wins. `gh` prints the new resource's URL once
/// the API call has returned, so a URL on stdout is proof even when the process
/// then exits non-zero. When stdout is silent the repository itself is asked,
/// and the watermark taken before the run is what separates what this run
/// created from what was already there.
///
/// Every path that cannot see far enough records a named reason and stays
/// unknown rather than guessing, because it is a wrong "failed" that makes a
/// blind retry look safe (#184).
#[cfg(test)]
fn reconcile_failed_github_create(
    cwd: &Path,
    repository: &str,
    planning: &CreatePlanning,
    stdout: &[u8],
) -> Option<(OperationStatus, serde_json::Value)> {
    reconcile_failed_github_create_with_deadline(
        cwd,
        repository,
        planning,
        stdout,
        AdmissionDeadline::start(QueueWait::Forever),
    )
    .ok()
    .flatten()
}

fn reconcile_failed_github_create_with_deadline(
    cwd: &Path,
    repository: &str,
    planning: &CreatePlanning,
    stdout: &[u8],
    deadline: AdmissionDeadline,
) -> Result<Option<(OperationStatus, serde_json::Value)>, BrokerOpError> {
    let CreatePlanning::Planned(plan) = planning else {
        return Ok(planning.journal_value().map(|mut value| {
            value["evidence"] = json!({
                "classification": "unknown",
                "reason": "create_plan_unavailable",
            });
            (OperationStatus::OutcomeUnknown, value)
        }));
    };
    let mut value = planning.journal_value().expect("planned create");
    if let Some(created) = created_resource_url(&String::from_utf8_lossy(stdout), repository) {
        value["evidence"] = json!({
            "classification": "succeeded",
            "source": "command_output",
            "created": created,
        });
        return Ok(Some((OperationStatus::Succeeded, value)));
    }
    let Some(watermark) = plan.watermark else {
        value["evidence"] = json!({
            "classification": "unknown",
            "reason": "pre_create_number_watermark_unavailable",
        });
        return Ok(Some((OperationStatus::OutcomeUnknown, value)));
    };
    let Some(listed) = github_list_within(
        &plan.collection,
        repository,
        &["number", "url", plan.identity_field.as_str()],
        CREATE_OBSERVATION_LIMIT,
        cwd,
        deadline,
        "observing GitHub create after failure",
    )?
    else {
        value["evidence"] = json!({
            "classification": "unknown",
            "reason": "post_create_repository_evidence_unavailable",
        });
        return Ok(Some((OperationStatus::OutcomeUnknown, value)));
    };
    let (status, evidence) = classify_create_observation(plan, watermark, &listed);
    value["evidence"] = evidence;
    Ok(Some((status, value)))
}

/// Read a listing of the collection, newest first, for the planned create.
///
/// Kept apart from the `gh` call so the question it answers -- what does this
/// listing prove? -- can be asked of any listing, including the ones a test
/// writes by hand.
fn classify_create_observation(
    plan: &CreatePlan,
    watermark: i64,
    listed: &[serde_json::Value],
) -> (OperationStatus, serde_json::Value) {
    let identity_field = plan.identity_field.as_str();
    if let Some(created) = listed.iter().find(|entry| {
        entry["number"]
            .as_i64()
            .is_some_and(|number| number > watermark)
            && entry[identity_field].as_str() == Some(plan.identity.as_str())
    }) {
        return (
            OperationStatus::Succeeded,
            json!({
                "classification": "succeeded",
                "source": "post_create_observation",
                "created": created["url"],
                "number": created["number"],
            }),
        );
    }
    // The listing is newest first, so its last entry is the oldest it reached.
    // A full page that never got back to the watermark leaves a gap the create
    // could be hiding in, and "absent from the page" is then not "not created".
    let reached_watermark = listed.len() < CREATE_OBSERVATION_LIMIT
        || listed
            .last()
            .and_then(|entry| entry["number"].as_i64())
            .is_some_and(|oldest| oldest <= watermark);
    if !reached_watermark {
        return (
            OperationStatus::OutcomeUnknown,
            json!({
                "classification": "unknown",
                "reason": "post_create_listing_did_not_reach_the_watermark",
                "watermark": watermark,
            }),
        );
    }
    (
        OperationStatus::Failed,
        json!({
            "classification": "failed",
            "source": "post_create_observation",
            "watermark": watermark,
        }),
    )
}

/// How a provider's CLI is spelled on disk. Not `OperationProvider::as_str`,
/// which is the wire spelling stored in the journal: that says `github` where
/// the binary is `gh`.
fn provider_executable(provider: OperationProvider) -> &'static str {
    match provider {
        OperationProvider::Git => "git",
        OperationProvider::Github => "gh",
    }
}

/// The binary that performs a coordinated operation.
///
/// The git arm is [`crate::git::git_command`] -- the probed binary -- and
/// never `Command::new("git")`. Until #179's review this was the one spawn in
/// the crate that still resolved its own executable through PATH, and it was
/// invisible to any audit grepping for `Command::new("git")` because the
/// literal had been factored into a `match` on the provider. Extracting it
/// here is half the fix: the choice now has a name, a doc comment, and a test.
///
/// Sharing the *name* with the pre-push dry run is not enough. On a machine
/// whose wrapper the probe rejects, the check would run the trusted binary and
/// the mutation a different one -- verifying one command and performing
/// another, which is the shape of every defect #176 and #178 were about -- and
/// the `--no-verify` this function's caller appends then removes the pre-push
/// hook that was the last thing able to notice.
fn provider_command(provider: OperationProvider) -> Command {
    match provider {
        OperationProvider::Git => crate::git::git_command(),
        OperationProvider::Github => Command::new(provider_executable(provider)),
    }
}

fn redacted_command(provider: OperationProvider, args: &[String]) -> Result<String, BrokerOpError> {
    let sensitive_flags = [
        "-m",
        "--message",
        "--body",
        "--body-file",
        "--title",
        "--notes",
        "--notes-file",
        "--description",
        "--token",
        "--password",
        "--client-secret",
        "--value",
        "-f",
        "-F",
        "--field",
        "--raw-field",
        "--input",
    ];
    let mut redacted = vec![provider_executable(provider).to_string()];
    let mut hide_next = false;
    for arg in args {
        if hide_next {
            redacted.push("[REDACTED]".into());
            hide_next = false;
            continue;
        }
        if sensitive_flags.contains(&arg.as_str()) {
            redacted.push(arg.clone());
            hide_next = true;
        } else if sensitive_flags
            .iter()
            .any(|flag| arg.starts_with(&format!("{flag}=")))
        {
            let flag = arg.split('=').next().unwrap_or(arg);
            redacted.push(format!("{flag}=[REDACTED]"));
        } else if arg.contains("://") && arg.contains('@') {
            redacted.push("[REDACTED_URL]".into());
        } else {
            redacted.push(arg.clone());
        }
    }
    Ok(serde_json::to_string(&redacted)?)
}

fn is_github_pull_request_merge(args: &[String]) -> bool {
    args.first().map(String::as_str) == Some("pr")
        && args.get(1).map(String::as_str) == Some("merge")
}

fn tracking_upstream_parts(upstream: &str) -> Option<(&str, &str)> {
    upstream
        .strip_prefix("refs/remotes/")
        .unwrap_or(upstream)
        .split_once('/')
        .filter(|(remote, branch)| !remote.is_empty() && !branch.is_empty())
}

fn deferred_post_merge_cleanup(
    upstream_ref: Option<String>,
    fetch_operation_id: Option<i64>,
    explanation: &str,
    next_action: Option<String>,
) -> PostMergeCleanupReport {
    PostMergeCleanupReport {
        state: PostMergeCleanupState::Deferred,
        upstream_ref,
        fetch_operation_id,
        cleanup: None,
        explanation: explanation.into(),
        next_action,
    }
}

impl Broker {
    /// Return a bounded, read-only coordination measurement snapshot. The
    /// lock policy intentionally stays unchanged until this evidence shows
    /// that repository-wide contention remains material after local hooks are
    /// moved outside the lock.
    pub fn coordinated_operation_stats(
        &mut self,
        repository: Option<&str>,
        limit: u32,
    ) -> Result<crate::OperationStats, BrokerOpError> {
        Ok(crate::operation_stats::from_store(
            self.store(),
            repository,
            limit,
        )?)
    }

    pub fn show_coordinated_operation(
        &mut self,
        operation_id: i64,
    ) -> Result<OperationShowReport, BrokerOpError> {
        let operation = self.store().coordinated_operation(operation_id)?.ok_or(
            crate::BrokerError::CoordinatedOperationNotFound(operation_id),
        )?;
        Ok(OperationShowReport::from_operation(operation))
    }

    pub fn run_coordinated_operation(
        &mut self,
        request: CoordinatedCommand,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        self.run_coordinated_operation_with_wait(request, QueueWait::Forever)
    }

    /// As [`Self::run_coordinated_operation`], but bounding how long the caller
    /// is willing to queue for the repository write lock.
    pub fn run_coordinated_operation_with_wait(
        &mut self,
        request: CoordinatedCommand,
        queue_wait: QueueWait,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        let session = self.store().session(request.session_id)?;
        if session.status.is_closed() {
            return Err(BrokerOpError::ClosedSessionOperation {
                session_id: session.id,
            });
        }
        let should_cleanup_after_merge = request.provider == OperationProvider::Github
            && is_github_pull_request_merge(&request.args);
        let session_id = request.session_id;
        let repository = request.repository.clone();
        let mut report = self.run_coordinated_operation_at_with_hooks(
            request,
            Path::new(&session.worktree_path),
            queue_wait,
            || Ok(()),
            |_, _| Ok(None),
        )?;
        if report.ok() && should_cleanup_after_merge {
            report.post_merge_cleanup = Some(
                self.cleanup_after_github_pull_request_merge(session_id, repository.as_deref()),
            );
            // The cleanup above fetches the upstream first, so the default
            // branch now carries the merge and the landing is findable. A
            // squash rewrites the SHA, so without this the session's own work
            // would look unsubmitted forever (#152).
            report.representing_commit = self.note_merge_time_representation(session_id, None);
        }
        Ok(report)
    }

    fn cleanup_after_github_pull_request_merge(
        &mut self,
        session_id: i64,
        repository: Option<&str>,
    ) -> PostMergeCleanupReport {
        let Some(repository) = repository else {
            return deferred_post_merge_cleanup(
                None,
                None,
                "the successful GitHub merge had no canonical repository assertion",
                None,
            );
        };
        let Some((upstream_ref, _)) = self.repo_handle().tracking_upstream() else {
            return deferred_post_merge_cleanup(
                None,
                None,
                "the primary branch has no configured fetched upstream",
                None,
            );
        };
        let Some((remote, branch)) = tracking_upstream_parts(&upstream_ref) else {
            return deferred_post_merge_cleanup(
                Some(upstream_ref.clone()),
                None,
                "the configured upstream is not a remote-tracking branch",
                Some(format!(
                    "aethyme broker integration reconcile --upstream {upstream_ref} --dry-run"
                )),
            );
        };
        let destination = format!("refs/remotes/{remote}/{branch}");
        let source = format!("refs/heads/{branch}");
        let fetch = self.run_coordinated_operation(CoordinatedCommand {
            session_id,
            provider: OperationProvider::Git,
            repository: Some(repository.to_string()),
            resolved_target: None,
            scope: Some(format!("ref:{destination}")),
            declared_effect: None,
            destructive_confirmed: false,
            authorization_reason: Some(
                "refresh the tracked target after an authorized pull-request merge".into(),
            ),
            args: vec![
                "fetch".into(),
                remote.to_string(),
                format!("{source}:{destination}"),
            ],
        });
        let fetch = match fetch {
            Ok(fetch) if fetch.ok() => fetch,
            Ok(fetch) => {
                return deferred_post_merge_cleanup(
                    Some(upstream_ref),
                    Some(fetch.operation.id),
                    "the pull request merged, but refreshing its target branch did not complete successfully",
                    Some(format!(
                        "aethyme broker operations show {}",
                        fetch.operation.id
                    )),
                );
            }
            Err(error) => {
                return deferred_post_merge_cleanup(
                    Some(upstream_ref.clone()),
                    None,
                    &format!(
                        "the pull request merged, but the tracked target could not be refreshed: {error}"
                    ),
                    Some(format!(
                        "aethyme broker integration reconcile --upstream {upstream_ref} --dry-run"
                    )),
                );
            }
        };

        match self.auto_cleanup_landed_integration(&upstream_ref) {
            Ok(cleanup) => {
                let state = match cleanup.state {
                    crate::AutomaticIntegrationCleanupState::Cleaned => {
                        PostMergeCleanupState::Cleaned
                    }
                    crate::AutomaticIntegrationCleanupState::NotNeeded => {
                        PostMergeCleanupState::NotNeeded
                    }
                    crate::AutomaticIntegrationCleanupState::Deferred => {
                        PostMergeCleanupState::Deferred
                    }
                };
                PostMergeCleanupReport {
                    state,
                    upstream_ref: Some(upstream_ref),
                    fetch_operation_id: Some(fetch.operation.id),
                    explanation: cleanup.explanation.clone(),
                    next_action: cleanup.next_action.clone(),
                    cleanup: Some(cleanup),
                }
            }
            Err(error) => deferred_post_merge_cleanup(
                Some(upstream_ref.clone()),
                Some(fetch.operation.id),
                &format!(
                    "the pull request merged and upstream refreshed, but automatic cleanup was refused: {error}"
                ),
                Some(format!(
                    "aethyme broker integration reconcile --upstream {upstream_ref} --dry-run"
                )),
            ),
        }
    }

    /// A record created before queueing must not linger as prepared when the
    /// operation never started. Best-effort: the caller is already returning the
    /// real failure, and the liveness-aware sweep is the backstop.
    fn resolve_unstarted_operation(&mut self, id: i64, reason: &str) {
        let details = json!({ "reason": reason }).to_string();
        let _ = self.store().transition_coordinated_operation(
            id,
            OperationStatus::Failed,
            None,
            Some(&details),
        );
    }

    /// Reap prepared write rows whose client process is gone. This runs from
    /// broker open so a dead client is resolved by an independent invocation,
    /// not only when another write happens to reach the same lock.
    pub(crate) fn reap_abandoned_prepared_operations(&mut self) -> Result<usize, BrokerOpError> {
        let pending = self.store().pending_coordinated_operations()?;
        let mut reaped = 0;
        for operation in pending {
            if operation.status != OperationStatus::Prepared || !process_is_gone(operation.pid) {
                continue;
            }
            let mut details = operation
                .details_json
                .as_deref()
                .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
                .filter(serde_json::Value::is_object)
                .unwrap_or_else(|| json!({}));
            details["reason"] = json!("abandoned_before_start");
            details["reaped_by"] = json!("broker_open");
            self.store().transition_coordinated_operation(
                operation.id,
                OperationStatus::Failed,
                None,
                Some(&details.to_string()),
            )?;
            reaped += 1;
        }
        Ok(reaped)
    }

    pub(crate) fn run_coordinated_operation_at(
        &mut self,
        request: CoordinatedCommand,
        cwd: &Path,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        self.run_coordinated_operation_at_with_wait(request, cwd, QueueWait::Forever)
    }

    /// As [`Self::run_coordinated_operation_at`], but bound the complete
    /// operation admission and child process to the caller's wait budget.
    ///
    /// The original helper intentionally keeps the compatibility API's
    /// unbounded behavior. Delivery callers use this form because a remote
    /// fetch, push, or provider query must not leave the broker holding a
    /// repository lane forever when the network stops responding.
    pub(crate) fn run_coordinated_operation_at_with_wait(
        &mut self,
        request: CoordinatedCommand,
        cwd: &Path,
        queue_wait: QueueWait,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        self.run_coordinated_operation_at_with_hooks(
            request,
            cwd,
            queue_wait,
            || Ok(()),
            |_, _| Ok(None),
        )
    }

    /// Execute through the normal coordinated-operation state machine while
    /// allowing a caller to revalidate local state under the repository lock,
    /// then durably journal structured successful stdout before success.
    pub(crate) fn run_coordinated_operation_at_with_hooks<P, F>(
        &mut self,
        request: CoordinatedCommand,
        cwd: &Path,
        queue_wait: QueueWait,
        pre_execute: P,
        on_success: F,
    ) -> Result<CoordinatedOperationReport, BrokerOpError>
    where
        P: FnOnce() -> Result<(), String>,
        F: FnOnce(&[u8], i64) -> Result<Option<serde_json::Value>, String>,
    {
        // Starts before any preparation, so what preparation spends is taken
        // out of what the lock may wait (#219).
        let admission = AdmissionDeadline::start(queue_wait);
        if request.args.is_empty() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "broker {} requires arguments after --",
                    request.provider.as_str()
                ),
            });
        }
        refuse_repeated_program_name(request.provider, &request.args)?;
        if request.provider == OperationProvider::Git {
            refuse_code_executing_git_options(&request.args)?;
        }
        if request.provider == OperationProvider::Git
            && let Some(directory) = git_explicit_directory(&request.args, cwd)?
        {
            let selected = crate::GitRepo::discover(&directory)?;
            if selected.git_common_dir()? != self.repo_handle().git_common_dir()? {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: format!(
                        "git -C target {:?} is outside this broker repository; run the operation through that repository's broker",
                        directory
                    ),
                });
            }
        }
        let github_target = if request.provider == OperationProvider::Github {
            request
                .repository
                .as_deref()
                .map(|repository| crate::resolve_github_target(repository, &request.args))
                .transpose()?
        } else {
            None
        };
        let inferred = match request.provider {
            OperationProvider::Git => classify_git(&request.args),
            OperationProvider::Github => classify_gh(&request.args),
        };
        let (effect, classification) = resolve_effect(inferred, request.declared_effect)?;
        if effect == OperationEffect::Destructive && !request.destructive_confirmed {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason:
                    "destructive operation requires --destructive after resolving exact targets"
                        .into(),
            });
        }
        let authorization_reason =
            validate_authorization_reason(request.authorization_reason.as_deref())?;
        if effect != OperationEffect::Read && authorization_reason.is_none() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "coordinated writes require --reason identifying their authorization"
                    .into(),
            });
        }

        let git_operation =
            (request.provider == OperationProvider::Git).then(|| git_operation_kind(&request.args));
        let is_remote_git = git_operation == Some(GitOperationKind::Remote);
        let resolved_target = match (
            &request.resolved_target,
            &request.repository,
            request.provider,
        ) {
            (Some(expected), None, OperationProvider::Git) if is_remote_git => {
                let repo = crate::GitRepo::discover(cwd)?;
                let actual = repo.resolve_remote_command_target(
                    git_subcommand_args(&request.args).expect("remote Git command was parsed"),
                    None,
                )?;
                if actual.remote_name != expected.remote_name
                    || actual.coordination_key != expected.coordination_key
                {
                    return Err(BrokerOpError::InvalidCoordinatedOperation {
                        reason: format!(
                            "Git command resolved to {} via remote {:?}, but the internal workflow authorized {} via remote {:?}",
                            actual.coordination_key,
                            actual.remote_name,
                            expected.coordination_key,
                            expected.remote_name
                        ),
                    });
                }
                Some(actual)
            }
            (Some(_), _, _) => {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: "a resolved remote target is only valid for internal Git operations without a second repository assertion".into(),
                });
            }
            (None, Some(repository), OperationProvider::Git) if is_remote_git => {
                validate_repository(repository)?;
                let repo = crate::GitRepo::discover(cwd)?;
                Some(repo.resolve_remote_command_target(
                    git_subcommand_args(&request.args).expect("remote Git command was parsed"),
                    Some(repository),
                )?)
            }
            (None, Some(_), OperationProvider::Github) => {
                debug_assert!(github_target.is_some());
                None
            }
            (None, Some(repository), OperationProvider::Git) => {
                validate_repository(repository)?;
                None
            }
            (None, None, OperationProvider::Github) => {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: "broker gh requires --repo owner/name".into(),
                });
            }
            (None, None, OperationProvider::Git) if is_remote_git => {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: "remote Git operation requires --repo owner/name".into(),
                });
            }
            (None, None, OperationProvider::Git) => None,
        };
        // Before anything is journaled, queued or sent, because that is the
        // whole value of the check: a refusal leaves no operation to reconcile
        // and no question about what reached GitHub (#184).
        if let Some(target) = &github_target
            && effect != OperationEffect::Read
        {
            refuse_undefined_labels(&request.args, &target.display_slug, cwd)?;
        }
        // One repository has exactly one coordination key. The head-of-line
        // check below matches `repository` exactly, so a second spelling is not
        // cosmetic: a wedged operation journaled as `owner/Name` would not block
        // a later push journaled as `github.com/owner/name`, and the check that
        // exists to fail closed would fail open instead (issue #166).
        let (repository, canonical_identity) =
            match (&resolved_target, &request.repository, request.provider) {
                (Some(target), _, OperationProvider::Git) => {
                    (target.coordination_key.clone(), true)
                }
                (None, Some(_), OperationProvider::Github) => (
                    github_target
                        .as_ref()
                        .expect("resolved GitHub target")
                        .coordination_key
                        .clone(),
                    true,
                ),
                // A local Git command selects no remote, so it cannot resolve its
                // own identity the way `push` does. Anchoring it to `origin` --
                // the same anchor gates already use -- gives `tag -a` and the
                // `push` that publishes that tag one key instead of two.
                (None, assertion, OperationProvider::Git) => {
                    match canonical_local_repository(cwd, assertion.as_deref())? {
                        Some(key) => (key, true),
                        None => (
                            assertion
                                .clone()
                                .unwrap_or_else(|| format!("local:{}", self.main_root().display())),
                            false,
                        ),
                    }
                }
                (Some(_), _, OperationProvider::Github) => unreachable!("validated above"),
                (None, None, OperationProvider::Github) => unreachable!("validated above"),
            };
        // Derived before anything queues, from the command line alone. A
        // recognized comment or review write coordinates per resource; every
        // other command stays repository-wide (#181).
        let resource_scope = if effect == OperationEffect::Read {
            None
        } else {
            resource_lock_scope(request.provider, &request.args)
        };
        let lock_key = coordination_lock_key(&repository, resource_scope.as_deref());

        let scope_was_declared = request.scope.is_some();
        // The derived scope is also the audit scope, so the journal names the
        // resource the lock actually protects. A caller-declared scope still
        // wins for the record, but never changes which lock is taken.
        let scope = request
            .scope
            .or_else(|| resource_scope.clone())
            .unwrap_or_else(|| "repository".into());
        validate_scope(&scope)?;
        if inferred.is_none() && !scope_was_declared {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "ambiguous operation requires an explicit --scope as well as --effect"
                    .into(),
            });
        }

        let is_remote_write = effect != OperationEffect::Read
            && (resolved_target.is_some() || github_target.is_some());

        // Register before queueing, not after acquiring. An operation that only
        // existed once it held the lock was invisible for the whole wait, so a
        // caller could not tell a queued command from one that never started and
        // re-issued it (issue #138).
        let command_json = redacted_command(request.provider, &request.args)?;

        let hooks_ran_outside_lock = effect != OperationEffect::Read
            && request.provider == OperationProvider::Git
            && is_push(&request.args)
            && hooks_outside_lock_enabled(self.main_root());

        // Two identical commands from one session cannot both be intended: the
        // second would fire against state the first already changed. Now that a
        // queued operation is recorded, refusing the duplicate is possible before
        // it is queued rather than after both have run (issue #138).
        if effect != OperationEffect::Read
            && let Some(pending) = self
                .store()
                .unresolved_coordinated_operations(&repository)?
                .into_iter()
                .find(|pending| {
                    pending.session_id == request.session_id
                        && pending.command_json == command_json
                        && matches!(
                            pending.status,
                            OperationStatus::Prepared | OperationStatus::Running
                        )
                        && !process_is_gone(pending.pid)
                })
        {
            return Err(BrokerOpError::DuplicatePendingOperation {
                operation_id: pending.id,
                status: pending.status.as_str(),
                liveness: operation_liveness_summary(&pending),
            });
        }

        let operation = self
            .store()
            .create_coordinated_operation(&NewCoordinatedOperation {
                session_id: request.session_id,
                provider: request.provider,
                repository: repository.clone(),
                scope,
                effect,
                authorization_reason,
                command_json,
                pid: i64::from(std::process::id()),
                // Not known until the lock is held and the host guard begins.
                host_operation_id: None,
                identity_provenance: if canonical_identity {
                    OperationIdentityProvenance::VerifiedCanonical
                } else {
                    OperationIdentityProvenance::LocalRepository
                },
            })?;

        let queued_operation_id = operation.id;

        let ref_determination = measure_pr_merge_ref_determination(
            self.main_root(),
            cwd,
            &request.args,
            github_target.as_ref(),
        );

        // Run the push's local hooks before queueing for the lock, when the
        // repository opts in. A dry run executes `pre-push` against exactly the
        // commits the real push will send, so the expensive part happens outside
        // the lock and the fleet no longer serialises on the slowest gate
        // (issues #138, #146).
        let prechecked_plan = if hooks_ran_outside_lock {
            let mut dry_run = crate::git::git_command();
            let command_index =
                git_subcommand_index(&request.args).expect("push command was parsed");
            dry_run.args(&request.args[..command_index]);
            dry_run.arg("push").arg("--dry-run");
            dry_run.args(&request.args[command_index + 1..]);
            dry_run.current_dir(cwd);
            match output_within(
                dry_run,
                admission,
                &repository,
                "running the pre-push dry run",
                None,
            ) {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    // Failing here is the point: nothing is queued, and no other
                    // session waited on a gate that was going to refuse anyway.
                    self.resolve_unstarted_operation(queued_operation_id, "pre_push_refused");
                    return Err(BrokerOpError::InvalidCoordinatedOperation {
                        reason: format!(
                            "the repository's pre-push hook refused this push before the lock was taken: {}",
                            String::from_utf8_lossy(&output.stderr).trim()
                        ),
                    });
                }
                Err(error) => {
                    let reason = if matches!(&error, BrokerOpError::AdmissionTimedOut { .. }) {
                        "admission_timed_out"
                    } else {
                        "pre_push_unavailable"
                    };
                    self.resolve_unstarted_operation(queued_operation_id, reason);
                    return Err(error);
                }
            }
            Some(plan_exact_push(
                cwd,
                &request.args,
                resolved_target.as_ref(),
            ))
        } else {
            None
        };

        let lock_wait_started_at = (effect != OperationEffect::Read).then(unix_now_ms);
        let waiting_started_at = lock_wait_started_at.unwrap_or_else(unix_now_ms);
        let enqueued_at = operation.created_at;
        let lock = if effect == OperationEffect::Read {
            None
        } else {
            let main_root = self.main_root().to_path_buf();
            if let Err(error) = admission.check(&repository, "preparing the operation") {
                self.resolve_unstarted_operation(queued_operation_id, "admission_timed_out");
                return Err(error);
            }
            match RepositoryWriteLock::acquire(
                &main_root,
                &lock_key,
                queued_operation_id,
                || {
                    let holder = lock_holder_info(self.store(), &repository);
                    let details =
                        coordination_wait_details(&holder, enqueued_at, waiting_started_at);
                    self.store()
                        .annotate_prepared_operation(queued_operation_id, &details)?;
                    Ok(holder.description)
                },
                admission.remaining_queue_wait(queue_wait),
            ) {
                Ok(lock) => Some(lock),
                Err(error) => {
                    // Nothing ran, so the record must not linger as queued.
                    self.resolve_unstarted_operation(queued_operation_id, "lock_unavailable");
                    return Err(error);
                }
            }
        };
        if effect != OperationEffect::Read {
            let unresolved = self
                .store()
                .unresolved_coordinated_operations(&repository)?;
            for operation in unresolved {
                // Two operations block each other only where they would have
                // serialised. An unknown-outcome comment write leaves no ref
                // ambiguous, so it must not write-block a push -- and a stalled
                // push says nothing about a comment thread (#181).
                if stored_resource_scope(&operation) != resource_scope {
                    continue;
                }
                match operation.status {
                    // A prepared record now also covers an operation queued for
                    // this lock, so only a record whose owner is gone is abandoned.
                    // Resolving a live one would fail an operation that is merely
                    // waiting its turn (issue #138).
                    OperationStatus::Prepared if process_is_gone(operation.pid) => {
                        self.store().transition_coordinated_operation(
                            operation.id,
                            OperationStatus::Failed,
                            None,
                            Some(r#"{"reason":"abandoned_before_start"}"#),
                        )?;
                    }
                    OperationStatus::Prepared => {}
                    OperationStatus::Running => {
                        let blocking = self.store().transition_coordinated_operation(
                            operation.id,
                            OperationStatus::OutcomeUnknown,
                            operation.exit_code,
                            Some(r#"{"reason":"process_ended_without_outcome"}"#),
                        )?;
                        self.resolve_unstarted_operation(queued_operation_id, "repository_blocked");
                        return Err(BrokerOpError::CoordinatedOperationBlocked {
                            repository,
                            operation_id: blocking.id,
                            recovery: UnknownOutcomeRecovery::from_operation(&blocking),
                        });
                    }
                    OperationStatus::OutcomeUnknown => {
                        let recovery = UnknownOutcomeRecovery::from_operation(&operation);
                        self.resolve_unstarted_operation(queued_operation_id, "repository_blocked");
                        return Err(BrokerOpError::CoordinatedOperationBlocked {
                            repository,
                            operation_id: operation.id,
                            recovery,
                        });
                    }
                    _ => {}
                }
            }
        }
        let mut host_guard = if is_remote_write {
            Some(crate::HostOperationGuard::begin(
                &self.host_operation_database_path()?,
                &lock_key,
                request.provider,
                effect,
            )?)
        } else {
            None
        };
        if let Err(reason) = pre_execute() {
            self.resolve_unstarted_operation(queued_operation_id, "revalidation_failed");
            return Err(BrokerOpError::InvalidCoordinatedOperation { reason });
        }
        let push_planning = if request.provider == OperationProvider::Git {
            plan_exact_push(cwd, &request.args, resolved_target.as_ref())
        } else {
            PushPlanning::NotApplicable
        };
        // The counterpart for GitHub: what a create would produce, and the
        // number the repository already stands at. Taken here and not earlier
        // so the watermark is as close to the spawn as the lock allows --
        // every number assigned after it is a candidate for "this run did
        // that" (#184).
        let mut create_planning =
            if request.provider == OperationProvider::Github && effect != OperationEffect::Read {
                plan_github_create(&request.args, cwd)
            } else {
                CreatePlanning::NotApplicable
            };
        if let Some(target) = &github_target
            && let Err(error) =
                observe_create_watermark(&mut create_planning, &target.display_slug, cwd, admission)
        {
            self.resolve_unstarted_operation(queued_operation_id, "create_observation_failed");
            return Err(error);
        }

        // The hook verified specific commits. If any local ref moved while this
        // operation waited for the lock, that verification no longer describes
        // what would be sent, and the push must not proceed with hooks skipped.
        if let Some(prechecked) = &prechecked_plan
            && !push_planning.matches_prechecked(prechecked)
        {
            self.resolve_unstarted_operation(queued_operation_id, "refs_moved_after_pre_push");
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "a local ref moved between the pre-push hook and acquiring the lock, so \
                         the hook no longer describes what would be pushed; re-run the command"
                    .into(),
            });
        }

        // A bounded request may spend its entire budget in revalidation or
        // push planning. It has not started the provider command in this
        // case, so leave the prepared journal row failed rather than claiming
        // that a remote write has an unknown outcome.
        if let Err(error) = admission.check(&repository, "preparing the operation") {
            self.resolve_unstarted_operation(queued_operation_id, "admission_timed_out");
            return Err(error);
        }

        if let Some(host_operation_id) = host_guard
            .as_ref()
            .map(|guard| guard.operation().operation_id.clone())
        {
            self.store()
                .attach_host_operation(operation.id, &host_operation_id)?;
        }
        if let Some(guard) = &mut host_guard {
            guard.mark_running()?;
        }
        let operation_db_path = crate::broker_db_path(self.main_root());
        let mut operation_heartbeat = OperationHeartbeat::start(
            &operation_db_path,
            operation.id,
            "executing coordinated operation",
        );
        let mut running_details = journal_details(
            classification,
            resolved_target.as_ref(),
            github_target.as_ref(),
            with_push_planning(json!({}), &push_planning),
        );
        if let Some(heartbeat) = operation_heartbeat.as_ref() {
            add_operation_liveness(&mut running_details, heartbeat.liveness(unix_now_ms()));
        }
        self.store().transition_coordinated_operation(
            operation.id,
            OperationStatus::Running,
            None,
            Some(&running_details.to_string()),
        )?;

        // This is the spawn that performs the remote mutation, so the binary
        // it names is the whole point -- see `provider_command`.
        let executable = provider_executable(request.provider);
        let mut command = provider_command(request.provider);
        command.args(&request.args);
        // Appended after the subcommand, where `git push` accepts it. The
        // repository opted in, the same hook already ran against these exact
        // commits in the dry run above, and the plan was re-proven unchanged
        // under the lock. Re-running it here would double the cost the opt-in
        // exists to avoid (issues #138, #146).
        if hooks_ran_outside_lock {
            command.arg("--no-verify");
        }
        // Trace2 gives us process-level evidence for the one useful
        // pre-transfer distinction Git itself does not expose in its exit
        // status: a local pre-push hook can reject the command before any
        // transport child starts. Keep the trace private and use it only for
        // the journal; it must not alter the command's user-facing output.
        let git_trace = (request.provider == OperationProvider::Git
            && is_remote_git
            && effect != OperationEffect::Read)
            .then(|| tempfile::NamedTempFile::new().ok())
            .flatten();
        if let Some(trace) = &git_trace {
            command.env("GIT_TRACE2_EVENT", trace.path());
        }
        command
            .current_dir(cwd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Inline config from the caller's environment would bypass the
            // `-c` refusal above.
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT")
            .env("AETHYME_BROKER_SESSION_ID", request.session_id.to_string());
        // The pre-push hook treats the operation id as coordination for
        // protected branches, so a read never carries one.
        if effect != OperationEffect::Read {
            command.env("AETHYME_BROKER_OPERATION_ID", operation.id.to_string());
        }
        if let Some(heartbeat) = operation_heartbeat.as_ref() {
            command.env(BROKER_OPERATION_PROGRESS_ENV, heartbeat.path());
        }
        if request.provider == OperationProvider::Github {
            command.env(
                "GH_REPO",
                &github_target
                    .as_ref()
                    .expect("resolved GitHub target")
                    .display_slug,
            );
        }
        let output = match output_within(
            command,
            admission,
            &repository,
            "executing coordinated operation",
            operation_heartbeat.as_ref(),
        ) {
            Ok(output) => output,
            Err(BrokerOpError::AdmissionTimedOut {
                repository,
                stage,
                budget,
            }) => {
                let operation_liveness =
                    operation_heartbeat.as_mut().map(OperationHeartbeat::finish);
                let status = if is_remote_write {
                    OperationStatus::OutcomeUnknown
                } else {
                    OperationStatus::Failed
                };
                let mut details = journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    with_push_planning(
                        json!({
                            "failure_class": "coordinated_operation_timeout",
                            "stage": stage,
                            "budget": budget,
                            "remote_outcome": if is_remote_write {
                                "unknown"
                            } else {
                                "not_applicable"
                            },
                        }),
                        &push_planning,
                    ),
                );
                if let Some(liveness) = operation_liveness {
                    add_operation_liveness(&mut details, liveness);
                }
                add_coordination_timing(
                    &mut details,
                    &lock_key,
                    lock_wait_started_at,
                    lock.as_ref(),
                    hooks_ran_outside_lock,
                    ref_determination,
                );
                let operation = self.store().transition_coordinated_operation(
                    operation.id,
                    status,
                    None,
                    Some(&details.to_string()),
                )?;
                if let Some(guard) = &mut host_guard {
                    guard.finish(operation.status)?;
                }
                if status == OperationStatus::OutcomeUnknown {
                    return Err(BrokerOpError::CoordinatedOperationBlocked {
                        repository,
                        operation_id: operation.id,
                        recovery: UnknownOutcomeRecovery::from_operation(&operation),
                    });
                }
                return Err(BrokerOpError::CoordinatedOperationTimedOut {
                    provider: request.provider.as_str(),
                    operation_id: operation.id,
                    repository,
                    stage,
                    budget,
                });
            }
            Err(BrokerOpError::OperationIo { source, .. }) => {
                let operation_liveness =
                    operation_heartbeat.as_mut().map(OperationHeartbeat::finish);
                let mut details = journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    with_push_planning(
                        json!({
                            "reason": "spawn_failed",
                            "remote_contact": "not_contacted",
                            "remote_not_contacted": true,
                        }),
                        &push_planning,
                    ),
                );
                if let Some(liveness) = operation_liveness {
                    add_operation_liveness(&mut details, liveness);
                }
                add_coordination_timing(
                    &mut details,
                    &lock_key,
                    lock_wait_started_at,
                    lock.as_ref(),
                    hooks_ran_outside_lock,
                    ref_determination,
                );
                let operation = self.store().transition_coordinated_operation(
                    operation.id,
                    OperationStatus::Failed,
                    None,
                    Some(&details.to_string()),
                )?;
                if let Some(guard) = &mut host_guard {
                    guard.finish(operation.status)?;
                }
                return Err(BrokerOpError::OperationSpawn {
                    executable: executable.into(),
                    source,
                });
            }
            Err(error) => {
                let _ = operation_heartbeat.as_mut().map(OperationHeartbeat::finish);
                return Err(error);
            }
        };
        // The heartbeat deliberately outlives the child. Everything below --
        // `on_success`, and the push and GitHub reconciles -- runs while the
        // row is still `running`, and one of those reconciles is an unbounded
        // `git ls-remote`. Stopping the heartbeat here left a healthy
        // operation looking `heartbeat_stale` after 30s, so every blocked
        // caller was told the holder may have died: the #138 failure this
        // machinery exists to prevent, restated more confidently.
        let remote_contact = git_trace
            .as_ref()
            .map(|trace| inspect_git_transfer_trace(trace.path()))
            .and_then(GitTransferTrace::remote_contact);
        let exit_code = output.status.code().map(i64::from);
        let (status, mut details) = if output.status.success() {
            match on_success(&output.stdout, operation.id) {
                Ok(Some(result)) => (
                    OperationStatus::Succeeded,
                    journal_details(
                        classification,
                        resolved_target.as_ref(),
                        github_target.as_ref(),
                        with_push_planning(json!({ "result": result }), &push_planning),
                    ),
                ),
                Ok(None) => (
                    OperationStatus::Succeeded,
                    journal_details(
                        classification,
                        resolved_target.as_ref(),
                        github_target.as_ref(),
                        with_push_planning(json!({}), &push_planning),
                    ),
                ),
                Err(reason) => (
                    OperationStatus::OutcomeUnknown,
                    journal_details(
                        classification,
                        resolved_target.as_ref(),
                        github_target.as_ref(),
                        with_push_planning(
                            json!({
                                "reason": "success_result_not_recorded",
                                "diagnosis": reason,
                            }),
                            &push_planning,
                        ),
                    ),
                ),
            }
        } else if effect == OperationEffect::Read {
            (
                OperationStatus::Failed,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    json!({}),
                ),
            )
        } else if request.provider == OperationProvider::Git
            && git_operation == Some(GitOperationKind::Local)
        {
            // A local Git command can leave the worktree or index in a
            // conflict state, but it cannot have an uncertain remote effect.
            // Keeping it as `outcome_unknown` write-blocked the canonical
            // repository and told the operator to inspect remote state that
            // the command could never have touched (#185).
            (
                OperationStatus::Failed,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    json!({
                        "failure_class": "local_git_command_failed",
                        "remote_contact": "not_applicable",
                        "recovery": "inspect_or_abort_local_worktree_state",
                    }),
                ),
            )
        } else if let Some((status, push_reconciliation)) =
            reconcile_failed_push(cwd, &push_planning, remote_contact)
        {
            (
                status,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    json!({ "push_reconciliation": push_reconciliation }),
                ),
            )
        } else if let Some((status, create_reconciliation)) = match github_target.as_ref() {
            Some(target) => reconcile_failed_github_create_with_deadline(
                cwd,
                &target.display_slug,
                &create_planning,
                &output.stdout,
                admission,
            )
            .ok()
            .flatten(),
            None => None,
        } {
            (
                status,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    json!({ "create_reconciliation": create_reconciliation }),
                ),
            )
        } else {
            // A mutating command may have applied a subset of its effects
            // before returning non-zero. Treating that as safely failed would
            // make a blind retry possible, so require external inspection.
            (
                OperationStatus::OutcomeUnknown,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    remote_contact.map_or_else(
                        || json!({}),
                        |remote_contact| {
                            json!({
                                "remote_contact": remote_contact.remote_contact,
                                "remote_write_contact": remote_contact.remote_write_contact,
                                "remote_not_contacted": remote_contact.remote_not_contacted,
                            })
                        },
                    ),
                ),
            )
        };
        // The row leaves `running` on the next statement, after which liveness
        // is no longer consulted, so this is the first moment the heartbeat is
        // redundant rather than load-bearing.
        if let Some(liveness) = operation_heartbeat.as_mut().map(OperationHeartbeat::finish) {
            add_operation_liveness(&mut details, liveness);
        }
        add_coordination_timing(
            &mut details,
            &lock_key,
            lock_wait_started_at,
            lock.as_ref(),
            hooks_ran_outside_lock,
            ref_determination,
        );
        let operation = self.store().transition_coordinated_operation(
            operation.id,
            status,
            exit_code,
            Some(&details.to_string()),
        )?;
        if let Some(guard) = &mut host_guard {
            guard.finish(operation.status)?;
        }
        let created_pull_request_number = (output.status.success()
            && request.provider == OperationProvider::Github
            && crate::creates_pull_request(&request.args))
        .then(|| crate::pull_request_number_from_output(&String::from_utf8_lossy(&output.stdout)))
        .flatten();
        Ok(CoordinatedOperationReport {
            operation,
            classification,
            resolved_target,
            github_target,
            command_success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            post_merge_cleanup: None,
            representing_commit: None,
            created_pull_request: created_pull_request_number,
            pushed_refs: if output.status.success() {
                match &push_planning {
                    PushPlanning::Planned(plan) => plan
                        .destinations
                        .iter()
                        .map(|destination| PushedRef {
                            destination_ref: destination.destination_ref.clone(),
                            proposed_sha: destination.proposed_sha.clone(),
                        })
                        .collect(),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            },
        })
    }

    pub fn reconcile_coordinated_operation(
        &mut self,
        operation_id: i64,
        succeeded: bool,
        reason: &str,
    ) -> Result<OperationReconcileReport, BrokerOpError> {
        if reason.trim().is_empty() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "operation reconciliation requires a non-empty --reason".into(),
            });
        }
        let operation = self.store().coordinated_operation(operation_id)?.ok_or(
            crate::BrokerError::CoordinatedOperationNotFound(operation_id),
        )?;
        if operation.host_operation_id.is_none()
            && operation.status != OperationStatus::OutcomeUnknown
        {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "operation {} is {}, not outcome_unknown",
                    operation_id,
                    operation.status.as_str()
                ),
            });
        }
        let status = if succeeded {
            OperationStatus::ReconciledSucceeded
        } else {
            OperationStatus::ReconciledFailed
        };
        if let Some(host_operation_id) = &operation.host_operation_id {
            crate::reconcile_host_operation(
                &self.host_operation_database_path()?,
                host_operation_id,
                succeeded,
            )?;
        }
        let mut details = operation
            .details_json
            .as_deref()
            .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| json!({}));
        details["reconciliation"] = json!({
            "operator_reason": reason,
            "outcome": status.as_str(),
        });
        let operation = self.store().transition_coordinated_operation(
            operation_id,
            status,
            operation.exit_code,
            Some(&details.to_string()),
        )?;
        Ok(OperationReconcileReport {
            operation,
            reason: reason.into(),
        })
    }
}

#[cfg(test)]
mod tests {

    fn gh(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    fn scope(args: &[&str]) -> Option<String> {
        resource_lock_scope(OperationProvider::Github, &gh(args))
    }

    fn issue_plan(identity: &str, watermark: Option<i64>) -> CreatePlan {
        CreatePlan {
            collection: "issue".into(),
            identity_field: "title".into(),
            identity: identity.into(),
            watermark,
        }
    }

    fn listed(entries: &[(i64, &str)]) -> Vec<serde_json::Value> {
        entries
            .iter()
            .map(|(number, title)| {
                json!({
                    "number": number,
                    "title": title,
                    "url": format!("https://github.com/o/r/issues/{number}"),
                })
            })
            .collect()
    }

    /// The subcommand is what decides whether a command is labelled at all, and
    /// `gh` accepts flags in front of it.
    #[test]
    fn the_subcommand_is_found_behind_leading_flags_but_never_inside_one() {
        assert_eq!(
            gh_subcommand(&gh(&["issue", "create", "--title", "x"])),
            Some(("issue", "create"))
        );
        assert_eq!(
            gh_subcommand(&gh(&["--repo", "o/r", "issue", "create"])),
            Some(("issue", "create"))
        );
        // `issue` here is the value of `--title`, not the command.
        assert_eq!(
            gh_subcommand(&gh(&["--title", "issue", "create", "later"])),
            Some(("create", "later"))
        );
    }

    /// Every spelling `gh` accepts for a label has to be seen, because a label
    /// this misses is one the pre-flight check cannot refuse (#184).
    #[test]
    fn labels_are_collected_across_spellings_repetitions_and_comma_lists() {
        assert_eq!(
            gh_requested_labels(&gh(&[
                "issue",
                "create",
                "--title",
                "t",
                "--label",
                "bug,dette",
                "-l",
                "area:broker",
                "--label=chore",
            ])),
            vec!["bug", "dette", "area:broker", "chore"]
        );
        assert_eq!(
            gh_requested_labels(&gh(&[
                "issue",
                "edit",
                "7",
                "--add-label",
                "bug",
                "--remove-label",
                "stale",
            ])),
            vec!["bug", "stale"]
        );
    }

    /// `--label` on a listing is a filter: an unknown name returns nothing
    /// rather than failing, so refusing it would break a working read.
    #[test]
    fn a_label_filter_on_a_listing_is_not_a_label_to_resolve() {
        assert!(gh_requested_labels(&gh(&["issue", "list", "--label", "dette"])).is_empty());
        assert!(gh_requested_labels(&gh(&["label", "create", "dette"])).is_empty());
    }

    /// The identity has to come from the command line, and a create whose
    /// result could not be named afterwards must say so rather than be planned
    /// against something that does not identify it.
    #[test]
    fn a_create_is_planned_from_its_identity_or_declared_unplannable() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            plan_github_create(&gh(&["issue", "create", "--title", "a bug"]), tmp.path()),
            CreatePlanning::Planned(issue_plan("a bug", None))
        );
        assert_eq!(
            plan_github_create(&gh(&["issue", "create", "--body", "b"]), tmp.path()),
            CreatePlanning::Unplannable {
                reason: "issue_create_without_an_explicit_title"
            }
        );
        assert_eq!(
            plan_github_create(
                &gh(&["pr", "create", "--title", "t", "--head", "topic"]),
                tmp.path()
            ),
            CreatePlanning::Planned(CreatePlan {
                collection: "pr".into(),
                identity_field: "headRefName".into(),
                identity: "topic".into(),
                watermark: None,
            })
        );
        assert_eq!(
            plan_github_create(&gh(&["issue", "comment", "7", "--body", "b"]), tmp.path()),
            CreatePlanning::NotApplicable
        );
    }

    /// `gh` prints the new resource's URL once the API call has returned, so a
    /// URL on stdout survives a later non-zero exit as proof. A URL for some
    /// other repository proves nothing about this one.
    #[test]
    fn only_a_url_under_the_asserted_repository_counts_as_a_created_resource() {
        assert_eq!(
            created_resource_url(
                "https://github.com/Schiste/Aethyme/issues/184\n",
                "github.com/schiste/aethyme"
            )
            .as_deref(),
            Some("https://github.com/Schiste/Aethyme/issues/184")
        );
        assert_eq!(
            created_resource_url(
                "https://github.com/other/repo/issues/9\n",
                "schiste/aethyme"
            ),
            None
        );
        assert_eq!(
            created_resource_url("could not add label: dette not found\n", "schiste/aethyme"),
            None
        );
    }

    /// The watermark is what separates the resource this run created from one
    /// that merely carries the same title.
    #[test]
    fn only_a_number_above_the_watermark_proves_this_run_created_it() {
        let plan = issue_plan("a bug", Some(10));
        let (status, evidence) =
            classify_create_observation(&plan, 10, &listed(&[(11, "a bug"), (9, "older")]));
        assert_eq!(status, OperationStatus::Succeeded);
        assert_eq!(evidence["classification"], "succeeded");
        assert_eq!(evidence["number"], 11);

        // Same title, but it predates the command: this run created nothing.
        let (status, evidence) =
            classify_create_observation(&plan, 10, &listed(&[(9, "a bug"), (8, "older")]));
        assert_eq!(status, OperationStatus::Failed);
        assert_eq!(evidence["classification"], "failed");
    }

    /// A page that never reached back to the watermark leaves a gap the create
    /// could be hiding in, and a wrong "failed" is what makes a blind retry
    /// look safe (#184).
    #[test]
    fn a_listing_that_stops_above_the_watermark_stays_unknown() {
        let plan = issue_plan("a bug", Some(10));
        let full: Vec<(i64, &str)> = (0..CREATE_OBSERVATION_LIMIT)
            .map(|index| (200 - index as i64, "unrelated"))
            .collect();
        let (status, evidence) = classify_create_observation(&plan, 10, &listed(&full));
        assert_eq!(status, OperationStatus::OutcomeUnknown);
        assert_eq!(
            evidence["reason"],
            "post_create_listing_did_not_reach_the_watermark"
        );

        // One entry short of a full page is a listing that ran out, not one
        // that was truncated, so it does prove absence.
        let (status, _) = classify_create_observation(&plan, 10, &listed(&full[1..]));
        assert_eq!(status, OperationStatus::Failed);
    }

    /// A create nobody could recognise afterwards is unknown, not failed.
    #[test]
    fn an_unplannable_create_and_a_missing_watermark_both_stay_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let (status, value) = reconcile_failed_github_create(
            tmp.path(),
            "o/r",
            &CreatePlanning::Unplannable {
                reason: "issue_create_without_an_explicit_title",
            },
            b"",
        )
        .unwrap();
        assert_eq!(status, OperationStatus::OutcomeUnknown);
        assert_eq!(value["evidence"]["reason"], "create_plan_unavailable");

        let (status, value) = reconcile_failed_github_create(
            tmp.path(),
            "o/r",
            &CreatePlanning::Planned(issue_plan("a bug", None)),
            b"",
        )
        .unwrap();
        assert_eq!(status, OperationStatus::OutcomeUnknown);
        assert_eq!(
            value["evidence"]["reason"],
            "pre_create_number_watermark_unavailable"
        );
    }

    /// Nothing to reconcile for a command that creates nothing: the caller's
    /// existing conservative fallback still applies.
    #[test]
    fn a_command_that_creates_nothing_is_left_to_the_conservative_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            reconcile_failed_github_create(
                tmp.path(),
                "o/r",
                &CreatePlanning::NotApplicable,
                b"whatever",
            )
            .is_none()
        );
    }

    /// The scope must come from the command line alone: it decides which lock
    /// to queue for, so it has to be known before the operation queues (#181).
    #[test]
    fn comment_and_review_writes_resolve_to_a_per_resource_scope() {
        assert_eq!(
            scope(&["pr", "comment", "7", "--body", "hi"]).as_deref(),
            Some("pull/7/comments")
        );
        assert_eq!(
            scope(&["issue", "comment", "12", "--body", "hi"]).as_deref(),
            Some("issue/12/comments")
        );
        assert_eq!(
            scope(&["pr", "review", "7", "--approve"]).as_deref(),
            Some("pull/7/reviews")
        );
        assert_eq!(
            scope(&["api", "repos/o/r/pulls/7/reviews", "--method", "POST"]).as_deref(),
            Some("pull/7/reviews")
        );
        assert_eq!(
            scope(&["api", "repos/o/r/issues/12/comments", "-f", "body=hi"]).as_deref(),
            Some("issue/12/comments")
        );
    }

    /// A flag's value is not an operand. `--method POST` in front of the path
    /// must not make `POST` the resource.
    #[test]
    fn a_flag_value_is_never_read_as_the_resource() {
        assert_eq!(
            scope(&["api", "--method", "POST", "repos/o/r/pulls/7/reviews"]).as_deref(),
            Some("pull/7/reviews")
        );
        assert_eq!(
            scope(&["pr", "comment", "--body", "9", "7"]).as_deref(),
            Some("pull/7/comments")
        );
    }

    /// Everything not provably ref-free keeps the repository lock. These are
    /// the cases that must NOT narrow: a merge moves a ref, a protection
    /// change is a repository setting, and a selector this cannot resolve
    /// without asking the provider is not a resource it may assume.
    #[test]
    fn anything_not_provably_resource_scoped_stays_repository_wide() {
        assert_eq!(scope(&["pr", "merge", "7"]), None);
        assert_eq!(scope(&["pr", "create", "--title", "x"]), None);
        assert_eq!(scope(&["issue", "create"]), None);
        assert_eq!(
            scope(&[
                "api",
                "repos/o/r/branches/main/protection",
                "--method",
                "PUT"
            ]),
            None
        );
        // A URL or branch selector, not a number.
        assert_eq!(
            scope(&["pr", "comment", "https://github.com/o/r/pull/7"]),
            None
        );
        // Deeper than the collection endpoint: a reaction, not the thread.
        assert_eq!(
            scope(&[
                "api",
                "repos/o/r/issues/comments/99/reactions",
                "--method",
                "POST"
            ]),
            None
        );
        // A different provider never narrows.
        assert_eq!(
            resource_lock_scope(OperationProvider::Git, &gh(&["push", "origin", "main"])),
            None
        );
    }

    /// Repository-wide operations keep the bare canonical key, so #166's
    /// "one repository, one lock" is untouched for anything touching a ref.
    #[test]
    fn lock_keys_separate_resources_without_fragmenting_the_repository() {
        let repo = "github.com/o/r";
        assert_eq!(coordination_lock_key(repo, None), repo);
        assert_ne!(
            coordination_lock_key(repo, Some("pull/7/comments")),
            coordination_lock_key(repo, None)
        );
        assert_ne!(
            coordination_lock_key(repo, Some("pull/7/comments")),
            coordination_lock_key(repo, Some("pull/9/comments")),
        );
        assert_eq!(
            coordination_lock_key(repo, Some("pull/7/comments")),
            coordination_lock_key(repo, Some("pull/7/comments")),
        );
        // The host key validator rejects credential and URL syntax; the
        // separator must survive it.
        assert!(!coordination_lock_key(repo, Some("pull/7/comments")).contains('#'));
    }
    use super::*;

    /// #179's security finding. The coordinated write performs the remote
    /// mutation; the pre-push dry run beside it decides whether that mutation
    /// is allowed. They must be the same binary, and comparing the *program*
    /// rather than the name is the whole assertion -- both spell "git", and
    /// only one of them is the one the probe accepted.
    #[test]
    fn a_coordinated_git_operation_spawns_the_probed_binary() {
        assert_eq!(
            provider_command(OperationProvider::Git).get_program(),
            crate::git::git_command().get_program(),
            "the coordinated write must spawn the binary the dry run verified"
        );

        // The equality above holds trivially if both sides are the bare name
        // `git`, which is exactly what `git_command` falls back to when no
        // candidate probes clean. Where a candidate did, the probe resolved an
        // absolute path, and a regression to `Command::new("git")` becomes
        // visible rather than equal by coincidence.
        if matches!(
            crate::git::git_output_trust(),
            crate::git::GitOutputTrust::Undecorated { .. }
        ) {
            let program = provider_command(OperationProvider::Git);
            assert!(
                Path::new(program.get_program()).is_absolute(),
                "an accepted git resolves to a path, not to whatever PATH offers next: {:?}",
                program.get_program()
            );
        } else {
            eprintln!("no git on PATH probed clean; only the equality was checked");
        }

        // `gh` has no probe and needs none -- nothing in the broker parses its
        // output to authorize anything -- so PATH resolution is correct there.
        assert_eq!(
            provider_command(OperationProvider::Github).get_program(),
            std::ffi::OsStr::new("gh")
        );
    }

    #[test]
    fn wait_durations_read_naturally() {
        assert_eq!(humanize_duration(9), "9s");
        assert_eq!(humanize_duration(59), "59s");
        assert_eq!(humanize_duration(1_688), "28m 8s");
    }

    fn liveness_operation(
        status: OperationStatus,
        heartbeat_age_ms: i64,
        progress_age_ms: i64,
    ) -> CoordinatedOperation {
        let now = unix_now_ms();
        CoordinatedOperation {
            id: 1,
            session_id: 2,
            provider: OperationProvider::Git,
            repository: "owner/repo".into(),
            scope: "repository".into(),
            effect: OperationEffect::Write,
            status,
            authorization_reason: Some("test".into()),
            command_json: "[\"git\",\"push\"]".into(),
            pid: 3,
            exit_code: None,
            details_json: Some(
                json!({
                    "operation_liveness": {
                        "phase": "quality",
                        "progress": "gate 3 of 7",
                        "heartbeat_at": now - heartbeat_age_ms,
                        "progress_at": now - progress_age_ms,
                    }
                })
                .to_string(),
            ),
            created_at: now,
            updated_at: now,
            finished_at: None,
            host_operation_id: None,
            identity_provenance: OperationIdentityProvenance::VerifiedCanonical,
        }
    }

    #[test]
    fn operation_liveness_distinguishes_active_progress_and_dead_heartbeat() {
        assert_eq!(
            operation_liveness_view(&liveness_operation(OperationStatus::Running, 5_000, 5_000,))
                .state,
            "active"
        );
        assert_eq!(
            operation_liveness_view(&liveness_operation(OperationStatus::Running, 5_000, 61_000,))
                .state,
            "progress_stale"
        );
        assert_eq!(
            operation_liveness_view(&liveness_operation(OperationStatus::Running, 31_000, 5_000,))
                .state,
            "heartbeat_stale"
        );
    }

    #[test]
    fn progress_file_accepts_json_and_plain_text_events() {
        let file = tempfile::NamedTempFile::new().unwrap();
        append_progress_event(file.path(), "gate 2 of 7", Some("quality"));
        std::fs::OpenOptions::new()
            .append(true)
            .open(file.path())
            .unwrap()
            .write_all(b"gate 3 of 7\n")
            .unwrap();
        let state = Arc::new(Mutex::new(OperationHeartbeatState {
            phase: "starting".into(),
            progress: "none".into(),
            last_progress_at: 0,
            output_bytes: 0,
        }));
        let mut consumed = 0;
        consume_progress_file(file.path(), &mut consumed, &state);
        let state = state.lock().unwrap();
        assert_eq!(state.phase, "quality");
        assert_eq!(state.progress, "gate 3 of 7");
        assert!(state.last_progress_at > 0);
    }

    /// Issue #138: a blocked caller saw nothing at all, so a long hold was
    /// indistinguishable from a dead command and got re-issued.
    #[test]
    fn a_contended_lock_names_the_operation_holding_it() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("broker.db");
        // Open once so the schema exists, then seed the owning session directly.
        drop(crate::BrokerStore::open(&db).unwrap());
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "INSERT INTO sessions (
                 id, worktree_path, branch, origin, status,
                 created_at, updated_at, last_activity_at
             ) VALUES (37, '/repo/one', 'agent/one', 'adopted', 'active', 1, 1, 1);",
        )
        .unwrap();
        drop(conn);
        let mut store = crate::BrokerStore::open(&db).unwrap();
        let created = store
            .create_coordinated_operation(&crate::NewCoordinatedOperation {
                session_id: 37,
                provider: OperationProvider::Git,
                repository: "owner/repo".into(),
                scope: "refs/heads/feature".into(),
                effect: OperationEffect::Write,
                authorization_reason: Some("test".into()),
                command_json: "[\"push\"]".into(),
                pid: std::process::id() as i64,
                host_operation_id: None,
                identity_provenance: crate::OperationIdentityProvenance::VerifiedCanonical,
            })
            .unwrap();
        store
            .transition_coordinated_operation(created.id, OperationStatus::Running, None, None)
            .unwrap();

        let described = lock_holder_info(&mut store, "owner/repo").description;
        assert!(
            described.contains(&format!("operation {}", created.id))
                && described.contains("session 37")
                && described.contains("refs/heads/feature"),
            "the notice must identify the holder: {described}"
        );

        // A repository with nothing running must not claim a phantom holder.
        let other = lock_holder_info(&mut store, "owner/elsewhere").description;
        assert!(
            other.contains("has not recorded itself"),
            "an unregistered holder must be reported as such: {other}"
        );
    }

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).into()).collect()
    }

    /// A repository with one commit on `main`, a tag, and a GitHub remote.
    fn push_fixture() -> (tempfile::TempDir, crate::ResolvedRemoteTarget) {
        let tmp = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("a.txt"), "a\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "init"]);
        run(&["tag", "v1.0.0"]);
        // A real local remote: planning queries pre-push SHAs, so the remote
        // must be reachable for the plan to resolve.
        let bare = tmp.path().join("remote.git");
        let init = std::process::Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&bare)
            .output()
            .unwrap();
        assert!(init.status.success());
        run(&["remote", "add", "origin", bare.to_str().unwrap()]);
        let repo = crate::GitRepo::discover(tmp.path()).unwrap();
        let target = repo.resolve_remote_target("origin", None).unwrap();
        (tmp, target)
    }

    #[test]
    fn only_head_relative_push_sources_are_worktree_relative() {
        let argv = |items: &[&str]| {
            items
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
        };

        // `HEAD` and its relatives mean something different in each worktree.
        for source in [
            "HEAD:refs/heads/x",
            "HEAD",
            "+HEAD:refs/heads/x",
            "HEAD~1:refs/heads/x",
            "@",
            "@{u}",
        ] {
            let args = argv(&["push", "origin", source]);
            assert_eq!(
                worktree_relative_push_sources(&args),
                vec![source.to_string()],
                "{source} should be recognised as worktree-relative"
            );
        }

        // Refs under `refs/` are shared by every worktree, so a branch name
        // resolves identically wherever the command runs. Refusing these would
        // block safe pushes without catching anything.
        for source in [
            "main:refs/heads/x",
            "refs/heads/main:refs/heads/x",
            "deadbeef:refs/heads/x",
        ] {
            let args = argv(&["push", "origin", source]);
            assert!(
                worktree_relative_push_sources(&args).is_empty(),
                "{source} is shared across worktrees and must be allowed"
            );
        }

        // Options are not refspecs.
        let args = argv(&[
            "push",
            "--force-with-lease=refs/heads/x:abc",
            "origin",
            "abc:refs/heads/x",
        ]);
        assert!(worktree_relative_push_sources(&args).is_empty());

        // Anything that is not a push is none of this function's business.
        let args = argv(&["log", "HEAD"]);
        assert!(worktree_relative_push_sources(&args).is_empty());
    }

    #[test]
    fn containment_survives_a_symlinked_temporary_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("worktree");
        std::fs::create_dir_all(root.join("nested/deeper")).unwrap();

        assert!(is_within(&root, &root), "a worktree contains itself");
        assert!(is_within(&root.join("nested/deeper"), &root));
        assert!(
            !is_within(tmp.path(), &root),
            "the parent is not inside the worktree"
        );

        let sibling = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(!is_within(&sibling, &root), "a sibling checkout is outside");
    }

    fn planned_destinations(
        tmp: &tempfile::TempDir,
        target: &crate::ResolvedRemoteTarget,
        argv: &[&str],
    ) -> Vec<String> {
        match plan_exact_push(tmp.path(), &args(argv), Some(target)) {
            PushPlanning::Planned(plan) => plan
                .destinations
                .iter()
                .map(|d| d.destination_ref.clone())
                .collect(),
            other => panic!("expected a plan for {argv:?}, got {other:?}"),
        }
    }

    /// A push written without an explicit `src:dst` must still be planable:
    /// an unplanned push cannot be classified from remote evidence, so any
    /// failure of it write-blocks the repository (issue #131).
    #[test]
    fn implicit_push_refspecs_resolve_to_their_destination_ref() {
        let (tmp, target) = push_fixture();
        assert_eq!(
            planned_destinations(&tmp, &target, &["push", "-u", "origin", "HEAD"]),
            vec!["refs/heads/main"],
            "`push -u origin HEAD` pushes the current branch"
        );
        assert_eq!(
            planned_destinations(&tmp, &target, &["push", "origin", "main"]),
            vec!["refs/heads/main"],
            "a bare branch name pushes to the same branch"
        );
        assert_eq!(
            planned_destinations(&tmp, &target, &["push", "origin", "refs/tags/v1.0.0"]),
            vec!["refs/tags/v1.0.0"],
            "a fully qualified tag ref pushes to itself"
        );
        // An explicit refspec keeps working unchanged.
        assert_eq!(
            planned_destinations(&tmp, &target, &["push", "origin", "HEAD:refs/heads/main"]),
            vec!["refs/heads/main"]
        );
    }

    /// The case from issue #131: a local pre-push hook rejects the push, so no
    /// ref moved. With the refspec planable, remote evidence proves the write
    /// failed, which must classify as `failed` — not as an unknown outcome that
    /// write-blocks the repository.
    #[test]
    fn a_push_that_moved_no_ref_classifies_as_failed() {
        let (tmp, target) = push_fixture();
        for argv in [
            vec!["push", "-u", "origin", "HEAD"],
            vec!["push", "origin", "main"],
            vec!["push", "origin", "refs/tags/v1.0.0"],
        ] {
            let planning = plan_exact_push(tmp.path(), &args(&argv), Some(&target));
            let (status, value) = reconcile_failed_push(tmp.path(), &planning, None)
                .expect("a planned push reconciles");
            assert_eq!(
                status,
                OperationStatus::Failed,
                "{argv:?} moved no ref and must classify as failed, got {value}"
            );
            assert_eq!(value["evidence"]["classification"], "failed", "{argv:?}");
        }
    }

    /// Resolution must stay conservative: anything that is not exactly one
    /// local ref still refuses to plan rather than inventing a destination.
    #[test]
    fn unresolvable_push_refspecs_still_refuse_to_plan() {
        let (tmp, target) = push_fixture();
        let repo = crate::GitRepo::discover(tmp.path()).unwrap();
        let sha = repo.resolve_push_source("HEAD").unwrap();
        for argv in [
            vec!["push", "origin", "no-such-branch"],
            vec!["push", "origin", sha.as_str()],
        ] {
            match plan_exact_push(tmp.path(), &args(&argv), Some(&target)) {
                PushPlanning::Unsupported { reason } => assert_eq!(
                    reason, "push_refspec_does_not_resolve_to_one_local_ref",
                    "{argv:?}"
                ),
                other => panic!("expected refusal for {argv:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn classifiers_fail_closed_and_detect_destructive_operations() {
        assert_eq!(
            classify_git(&args(&["status"])),
            Some(OperationEffect::Read)
        );
        assert_eq!(classify_git(&args(&["push"])), Some(OperationEffect::Write));
        assert_eq!(
            classify_git(&args(&["push", "--force-with-lease"])),
            Some(OperationEffect::Destructive)
        );
        assert_eq!(classify_git(&args(&["unknown-extension"])), None);
        assert_eq!(
            classify_git(&args(&[
                "-C",
                "/tmp/linked-worktree",
                "merge",
                "--ff-only",
                "abc123"
            ])),
            Some(OperationEffect::Write)
        );
        assert_eq!(classify_git(&args(&["-C"])), None);

        assert_eq!(
            classify_gh(&args(&["pr", "view", "12"])),
            Some(OperationEffect::Read)
        );
        assert_eq!(
            classify_gh(&args(&["pr", "merge", "12"])),
            Some(OperationEffect::Write)
        );
        assert_eq!(
            classify_gh(&args(&["api", "repos/o/r", "--method", "DELETE"])),
            Some(OperationEffect::Destructive)
        );
        assert_eq!(classify_gh(&args(&["extension", "exec", "x"])), None);
    }

    /// Every classification bypass found in the 2026-09-23 audit, one row
    /// each. A row that stops holding reopens a way to hide a destructive or
    /// remote write behind a milder label.
    #[test]
    fn classification_closes_the_audited_bypasses() {
        use OperationEffect::{Destructive, Read, Write};
        let git_rows: &[(&[&str], Option<OperationEffect>)] = &[
            (&["push", "origin", "+main:main"], Some(Destructive)),
            (&["push", "-uf", "origin", "main"], Some(Destructive)),
            (&["push", "-d", "origin", "topic"], Some(Destructive)),
            (&["push", "-u", "origin", "topic"], Some(Write)),
            (&["clean", "-fdx"], Some(Destructive)),
            (&["clean", "-xdf"], Some(Destructive)),
            (&["branch", "-Df", "topic"], Some(Destructive)),
            (&["branch", "-f", "topic", "HEAD~1"], Some(Destructive)),
            (&["branch", "-vv"], Some(Read)),
            (&["tag", "-fa", "v1", "-m", "release"], Some(Destructive)),
            (&["reset", "--hard", "HEAD~1"], Some(Destructive)),
            (&["reset", "HEAD~1"], Some(Write)),
            (&["update-ref", "-d", "refs/heads/main"], Some(Destructive)),
            (&["send-pack", "origin", "+main:main"], Some(Destructive)),
            (&["rev-list", "--count", "HEAD"], Some(Read)),
            (&["config", "--get", "user.name"], Some(Read)),
            (&["config", "user.name", "x"], Some(Write)),
        ];
        for (row, expected) in git_rows {
            assert_eq!(classify_git(&args(row)), *expected, "git {row:?}");
        }
        assert_eq!(
            classify_gh(&args(&["api", "-XDELETE", "repos/o/r/git/refs/heads/x"])),
            Some(Destructive)
        );
        assert_eq!(
            classify_gh(&args(&["api", "-Xget", "repos/o/r"])),
            Some(Read)
        );
    }

    #[test]
    fn a_repeated_program_name_is_refused_before_classification() {
        assert!(
            refuse_repeated_program_name(OperationProvider::Git, &args(&["git", "add", "x"]))
                .is_err()
        );
        assert!(
            refuse_repeated_program_name(
                OperationProvider::Git,
                &args(&["-C", "/tmp/r", "git", "status"])
            )
            .is_err()
        );
        assert!(
            refuse_repeated_program_name(OperationProvider::Github, &args(&["gh", "pr", "merge"]))
                .is_err()
        );
        assert!(
            refuse_repeated_program_name(
                OperationProvider::Git,
                &args(&["push", "origin", "main"])
            )
            .is_ok()
        );
        assert!(
            refuse_repeated_program_name(OperationProvider::Github, &args(&["pr", "view", "1"]))
                .is_ok()
        );
    }

    #[test]
    fn an_unrecognized_command_cannot_be_declared_a_read() {
        assert!(resolve_effect(None, Some(OperationEffect::Read)).is_err());
        assert!(resolve_effect(None, Some(OperationEffect::Write)).is_ok());
        assert!(resolve_effect(Some(OperationEffect::Read), Some(OperationEffect::Read)).is_ok());
    }

    #[test]
    fn code_executing_git_config_is_refused_before_the_subcommand() {
        for row in [
            &["-c", "alias.p=push", "p", "origin", "HEAD:main"][..],
            &["-calias.p=push", "p"][..],
            &["-c", "Core.HooksPath=/tmp/h", "commit", "-m", "x"][..],
            &["--config-env=core.sshCommand=SSH", "fetch"][..],
            &["--exec-path=/tmp/x", "status"][..],
        ] {
            assert!(
                refuse_code_executing_git_options(&args(row)).is_err(),
                "{row:?} must be refused"
            );
        }
        for row in [
            &["-c", "user.name=x", "commit", "-m", "x"][..],
            &["-C", "/tmp/repo", "status"][..],
            // Subcommand options are not global config: `commit -c` reuses a message.
            &["commit", "-c", "HEAD"][..],
        ] {
            assert!(
                refuse_code_executing_git_options(&args(row)).is_ok(),
                "{row:?} must be allowed"
            );
        }
    }

    /// `--no-wait` must still bound its own preparation. Without a budget it
    /// inherits the unbounded wait it exists to avoid (#219).
    #[test]
    fn no_wait_admission_is_bounded_and_forever_is_not() {
        assert!(AdmissionDeadline::start(QueueWait::Refuse).at.is_some());
        assert!(
            AdmissionDeadline::start(QueueWait::Seconds(60))
                .at
                .is_some()
        );
        assert!(AdmissionDeadline::start(QueueWait::Forever).at.is_none());
        assert_eq!(
            AdmissionDeadline::start(QueueWait::Forever).budget_label(),
            "unbounded"
        );
    }

    /// The budget is shared between preparation and the lock, so a bounded
    /// request cannot spend its timeout twice.
    #[test]
    fn the_lock_only_gets_what_preparation_left() {
        let admission = AdmissionDeadline::start(QueueWait::Seconds(60));
        match admission.remaining_queue_wait(QueueWait::Seconds(60)) {
            QueueWait::Seconds(left) => assert!(left <= 60, "{left}"),
            other => panic!("expected a narrowed bound, got {other:?}"),
        }

        // A caller who asked to refuse still refuses; one who asked to wait
        // forever still waits.
        assert_eq!(
            admission.remaining_queue_wait(QueueWait::Refuse),
            QueueWait::Refuse
        );
        assert_eq!(
            AdmissionDeadline::start(QueueWait::Forever).remaining_queue_wait(QueueWait::Forever),
            QueueWait::Forever
        );
    }

    /// The defect itself: preparation that outruns the budget must be killed
    /// and reported, not waited on. A wedged remote is what this stands in for.
    #[test]
    fn preparation_that_outruns_the_budget_is_killed_and_reported() {
        let mut sleeper = Command::new("sleep");
        sleeper.arg("30");
        let started = std::time::Instant::now();
        let error = output_within(
            sleeper,
            AdmissionDeadline::start(QueueWait::Seconds(1)),
            "owner/repo",
            "running the pre-push dry run",
            None,
        )
        .expect_err("a child outliving the budget must not be waited on");

        match error {
            BrokerOpError::AdmissionTimedOut {
                repository,
                stage,
                budget,
            } => {
                assert_eq!(repository, "owner/repo");
                assert_eq!(stage, "running the pre-push dry run");
                assert_eq!(budget, humanize_duration(1));
            }
            other => panic!("expected AdmissionTimedOut, got {other:?}"),
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "returned after {:?}, so the child was waited on rather than killed",
            started.elapsed()
        );
    }

    #[test]
    fn failed_pre_push_hook_marks_remote_write_as_not_contacted() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"{"event":"child_start","child_id":1,"child_class":"hook","hook_name":"pre-push","argv":[".git/hooks/pre-push","origin"]}
{"event":"child_exit","child_id":1,"code":1}
"#,
        )
        .unwrap();
        let trace = inspect_git_transfer_trace(tmp.path());
        assert_eq!(
            trace.remote_contact(),
            Some(RemoteContactEvidence {
                remote_contact: "not_contacted",
                remote_write_contact: "not_contacted",
                remote_not_contacted: true,
            })
        );

        std::fs::write(
            tmp.path(),
            r#"{"event":"child_start","child_id":1,"argv":[".git/hooks/pre-push","origin"]}
{"event":"child_exit","child_id":1,"code":1}
{"event":"child_start","child_id":2,"argv":["git-receive-pack","repo.git"]}
"#,
        )
        .unwrap();
        let trace = inspect_git_transfer_trace(tmp.path());
        assert_eq!(
            trace.remote_contact(),
            Some(RemoteContactEvidence {
                remote_contact: "contacted",
                remote_write_contact: "not_contacted",
                remote_not_contacted: false,
            })
        );
    }

    #[test]
    fn remote_git_detection_ignores_global_checkout_options() {
        assert_eq!(
            git_operation_kind(&args(&["-C", "/tmp/checkout", "push", "origin", "main"])),
            GitOperationKind::Remote
        );
        assert_eq!(
            git_operation_kind(&args(&["-C", "/tmp/checkout", "rebase", "main"])),
            GitOperationKind::Local
        );
    }

    #[test]
    fn git_global_options_are_skipped_before_the_subcommand() {
        for command in [
            args(&["-c", "core.fsmonitor=true", "push"]),
            args(&["-c", "core.fsmonitor=true", "-C", "/tmp/checkout", "push"]),
            args(&["--git-dir", "/tmp/checkout/.git", "push"]),
            args(&["--git-dir=/tmp/checkout/.git", "push"]),
            args(&["--work-tree", "/tmp/checkout", "push"]),
            args(&["--namespace", "namespace", "push"]),
            args(&["--super-prefix", "prefix", "push"]),
            args(&["--config-env", "http.proxy=HTTPS_PROXY", "push"]),
        ] {
            assert_eq!(classify_git(&command), Some(OperationEffect::Write));
            assert_eq!(git_operation_kind(&command), GitOperationKind::Remote);
        }
        assert_eq!(
            classify_git(&args(&["--git-dir"])),
            None,
            "a missing global-option value must not be mistaken for a subcommand"
        );
        assert_eq!(
            git_operation_kind(&args(&["--unknown-global-option", "push"])),
            GitOperationKind::Unknown
        );
    }

    #[test]
    fn redaction_keeps_audit_shape_without_secret_values() {
        let value = redacted_command(
            OperationProvider::Github,
            &args(&["secret", "set", "TOKEN", "--body", "super-secret"]),
        )
        .unwrap();
        assert!(value.contains("[REDACTED]"));
        assert!(!value.contains("super-secret"));
    }

    #[test]
    fn github_target_cannot_be_overridden_after_the_broker_boundary() {
        let err = crate::resolve_github_target(
            "owner/repo",
            &args(&["pr", "merge", "12", "--repo", "other/repo"]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("second repository target"));
    }
}
