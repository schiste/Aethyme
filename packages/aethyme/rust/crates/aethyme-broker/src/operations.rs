//! Durable coordination for Git and GitHub CLI operations.
//!
//! The broker does not reimplement either CLI. It fixes the executable,
//! classifies the requested argv, journals a redacted intent, serializes
//! repository writes with `flock`, and records the outcome. A process death
//! after the command starts becomes `outcome_unknown`; later writes fail
//! closed until an operator reconciles that journal row.

use std::collections::{BTreeSet, VecDeque};
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

/// The bound a read-only coordinated operation gets when its caller set none.
///
/// A read takes no lock and no write block applies to it, so the only thing an
/// unbounded read can wait on is the provider. A stalled `gh api` therefore
/// outlived the caller's own timeout and said nothing about why (#555). The
/// bound sits under the 120 s most agent harnesses give a command, so the
/// caller gets the diagnostic instead of a kill; `--queue-timeout` still sets
/// it explicitly, and `AETHYME_BROKER_READ_BUDGET_SECS` changes the default.
pub(crate) const READ_OPERATION_BUDGET: std::time::Duration = std::time::Duration::from_secs(90);

/// A read whose length is the point, so the default read budget must not cut
/// it: following a run or checks until they finish, streaming a run's log,
/// downloading artifacts, or paginating an API list. Only an explicit
/// `--queue-timeout` bounds these (#555).
pub(crate) fn is_long_running_read(provider: OperationProvider, args: &[String]) -> bool {
    if provider != OperationProvider::Github {
        return false;
    }
    let command = args.first().map(String::as_str);
    let action = args.get(1).map(String::as_str);
    let watches = action == Some("watch") || has_any(args, &["--watch", "-w"]);
    let streams_log = command == Some("run")
        && action == Some("view")
        && has_any(args, &["--log", "--log-failed"]);
    let downloads = action == Some("download");
    let paginates = command == Some("api") && has_any(args, &["--paginate"]);
    watches || streams_log || downloads || paginates
}

fn read_operation_budget() -> std::time::Duration {
    std::env::var("AETHYME_BROKER_READ_BUDGET_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map_or(READ_OPERATION_BUDGET, std::time::Duration::from_secs)
}

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
    started: std::time::Instant,
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
        let started = std::time::Instant::now();
        Self {
            started,
            at: budget.map(|budget| started + budget),
            budget,
        }
    }

    /// Bound a read its caller left unbounded. A read never queues for the
    /// lock, so "wait forever" could only mean "wait forever on the provider"
    /// (#555). Writes keep exactly the wait their caller chose, and so does a
    /// read that is long by design ([`is_long_running_read`]).
    fn bounded_for(self, effect: OperationEffect, long_running: bool) -> Self {
        if effect != OperationEffect::Read || long_running || self.budget.is_some() {
            return self;
        }
        let budget = read_operation_budget();
        Self {
            started: self.started,
            at: Some(self.started + budget),
            budget: Some(budget),
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
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
    /// The live session whose branch a destructive write may delete or
    /// rewrite (`--cross-session`). Without it such a write is refused (#393).
    pub cross_session: Option<i64>,
    /// `--ref-write-acknowledged`: the operator confirmed that a gh command
    /// whose branch the broker cannot determine touches no other live
    /// session's branch. Requires `--destructive` (#393).
    pub ref_write_acknowledged: bool,
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
            "aethyme broker advanced operations reconcile --operation {} --outcome succeeded --reason \"external inspection confirmed operation {} took effect\"",
            self.operation_id, self.operation_id
        )
    }

    pub fn failed_command(&self) -> String {
        format!(
            "aethyme broker advanced operations reconcile --operation {} --outcome failed --reason \"external inspection confirmed operation {} did not take effect\"",
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

const OPERATION_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);
const OPERATION_LOCK_PROGRESS_INTERVAL: Duration = Duration::from_secs(2);

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
        session_id: Option<i64>,
        describe_holder: impl FnMut() -> Result<String, BrokerOpError>,
        queue_wait: QueueWait,
        report_progress: impl FnMut(&str, Duration),
    ) -> Result<Self, BrokerOpError> {
        Self::acquire_with_progress_interval(
            main_root,
            repository,
            (operation_id, session_id),
            describe_holder,
            queue_wait,
            OPERATION_LOCK_PROGRESS_INTERVAL,
            report_progress,
        )
    }

    fn acquire_with_progress_interval(
        main_root: &Path,
        repository: &str,
        (operation_id, session_id): (i64, Option<i64>),
        mut describe_holder: impl FnMut() -> Result<String, BrokerOpError>,
        queue_wait: QueueWait,
        progress_interval: Duration,
        mut report_progress: impl FnMut(&str, Duration),
    ) -> Result<Self, BrokerOpError> {
        debug_assert!(!progress_interval.is_zero());
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
        // SAFETY: `file` is a live `File` opened above and still owned here, so
        // `as_raw_fd` yields a valid descriptor. `flock` takes the descriptor by
        // value and only sets an advisory lock on it; it dereferences nothing
        // and cannot invalidate the borrow.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Self {
                file,
                acquired_at: Instant::now(),
                acquired_at_ms: unix_now_ms(),
                queue_wait_ms: 0,
            });
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(BrokerOpError::OperationIo {
                path,
                source: error,
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
        // that died. Showing the holder and periodic elapsed time keeps the wait
        // visible while preserving the caller supplied bound.
        let holder = describe_holder()?;
        eprintln!("[coordination] waiting for the {repository} write lock: {holder}");
        // Visible to `broker status` in other terminals for as long as this
        // process waits; dropped (and removed) on every exit from the loop.
        let mut registration = crate::waiters::WaitRegistration::start(
            main_root,
            session_id,
            crate::waiters::WAIT_COORDINATED_WRITE_LOCK,
            repository,
            &holder,
        );
        let waited = Instant::now();
        let deadline = match queue_wait {
            QueueWait::Refuse => unreachable!("refused above"),
            QueueWait::Forever => None,
            QueueWait::Seconds(seconds) => Some(waited + Duration::from_secs(seconds)),
        };
        let mut next_progress = waited + progress_interval;
        loop {
            // No portable timed flock: poll so even an unbounded wait can report
            // progress. flock grants no FIFO order either way, so polling gives
            // up no queue position a blocking wait would have kept.
            // SAFETY: as above — `file` is live and owned by this function.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(BrokerOpError::OperationIo {
                    path,
                    source: error,
                });
            }
            let now = Instant::now();
            if deadline.is_some_and(|deadline| now >= deadline) {
                return Err(BrokerOpError::CoordinatedLockBusy {
                    repository: repository.into(),
                    holder: describe_holder()?,
                    waited: humanize_duration(waited.elapsed().as_secs()),
                    operation_id,
                });
            }
            if now >= next_progress {
                // Progress is a diagnostic: a holder lookup that fails (a busy
                // database under exactly the contention being reported) must
                // not abort a wait that would otherwise acquire the lock.
                let holder = describe_holder().unwrap_or_else(|_| "holder unavailable".to_string());
                registration.update_holder(&holder);
                report_progress(&holder, waited.elapsed());
                next_progress = now + progress_interval;
            }
            let now = Instant::now();
            let mut sleep_for =
                OPERATION_LOCK_POLL_INTERVAL.min(next_progress.saturating_duration_since(now));
            if let Some(deadline) = deadline {
                sleep_for = sleep_for.min(deadline.saturating_duration_since(now));
            }
            if !sleep_for.is_zero() {
                std::thread::sleep(sleep_for);
            }
        }
        drop(registration);
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
    // SAFETY: `kill` with signal 0 checks existence and permission without
    // delivering a signal, and takes the pid by value. It dereferences no
    // pointer this crate owns, so `pid` only has to be a valid i32.
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

/// The shape of a brokered `git push`, exported to the repository's hooks as
/// `AETHYME_PUSH_KIND` (#264).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushKind {
    /// Every ref update deletes a remote ref; nothing new can reach the remote.
    DeleteOnly,
    /// Every ref update sends local content.
    Update,
    /// Deletions and updates together, or a shape the broker cannot classify
    /// with certainty. Treated like an update: hooks verify as before.
    Mixed,
}

impl PushKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PushKind::DeleteOnly => "delete-only",
            PushKind::Update => "update",
            PushKind::Mixed => "mixed",
        }
    }
}

/// What the hooks of a brokered push are told about it (#264).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PushShape {
    pub(crate) kind: PushKind,
    /// The refspecs as given, one per entry; empty when the push names none.
    pub(crate) refs: Vec<String>,
}

/// Classify a `git push` from its exact arguments.
///
/// A push is `delete-only` only when every refspec is provably a deletion:
/// `--delete`/`-d` with plain ref names, or `:<dst>` refspecs. Anything the
/// parser does not fully understand -- an unknown option that may take a value,
/// a set-expanding option such as `--all` or `--mirror`, or no refspec at all
/// (which pushes whatever `push.default` selects) -- is `mixed`, so the
/// repository's hooks verify exactly as they did before.
pub(crate) fn classify_push(args: &[String]) -> Option<PushShape> {
    let args = git_subcommand_args(args)?;
    if args.first().map(String::as_str) != Some("push") {
        return None;
    }
    let mixed = |refs: Vec<String>| {
        Some(PushShape {
            kind: PushKind::Mixed,
            refs,
        })
    };
    let mut delete_flag = false;
    let mut positionals: Vec<String> = Vec::new();
    let mut tokens = args[1..].iter();
    let mut options_ended = false;
    while let Some(token) = tokens.next() {
        if options_ended || !token.starts_with('-') || token == "-" {
            positionals.push(token.clone());
            continue;
        }
        match token.as_str() {
            "--" => options_ended = true,
            "--delete" | "-d" => delete_flag = true,
            // Flags that change neither the set of refs nor whether content
            // is sent.
            "--force"
            | "-f"
            | "--force-with-lease"
            | "--force-if-includes"
            | "--no-force-if-includes"
            | "--set-upstream"
            | "-u"
            | "--atomic"
            | "--no-atomic"
            | "--quiet"
            | "-q"
            | "--verbose"
            | "-v"
            | "--progress"
            | "--no-progress"
            | "--porcelain"
            | "--no-verify"
            | "--verify"
            | "--thin"
            | "--no-thin"
            | "--signed"
            | "--no-signed"
            | "--ipv4"
            | "-4"
            | "--ipv6"
            | "-6"
            | "--no-recurse-submodules" => {}
            // Value-taking options given as a separate argument.
            "--push-option" | "-o" | "--repo" | "--receive-pack" | "--exec" => {
                if tokens.next().is_none() {
                    return mixed(positionals);
                }
            }
            _ => {
                let self_contained = [
                    "--force-with-lease=",
                    "--push-option=",
                    "--repo=",
                    "--receive-pack=",
                    "--exec=",
                    "--signed=",
                    "--recurse-submodules=",
                ]
                .iter()
                .any(|prefix| token.starts_with(prefix));
                if !self_contained {
                    // `--all`, `--mirror`, `--tags`, `--prune`, `--follow-tags`
                    // expand the ref set; anything unknown might take a value.
                    return mixed(positionals);
                }
            }
        }
    }
    // The first positional names the remote; the rest are refspecs.
    let refs: Vec<String> = positionals.into_iter().skip(1).collect();
    if refs.is_empty() {
        return mixed(refs);
    }
    let mut deletions = 0;
    let mut updates = 0;
    for refspec in &refs {
        let bare = refspec.strip_prefix('+').unwrap_or(refspec);
        if bare.is_empty() {
            return mixed(refs);
        }
        if delete_flag {
            // `--delete` takes plain ref names; a `src:dst` with it is an error
            // Git reports, so the broker does not guess.
            if bare.contains(':') {
                return mixed(refs);
            }
            deletions += 1;
        } else if let Some(destination) = bare.strip_prefix(':') {
            if destination.is_empty() || destination.contains(':') {
                // `:` alone pushes matching refs.
                return mixed(refs);
            }
            deletions += 1;
        } else {
            updates += 1;
        }
    }
    let kind = match (deletions, updates) {
        (_, 0) => PushKind::DeleteOnly,
        (0, _) => PushKind::Update,
        _ => PushKind::Mixed,
    };
    Some(PushShape { kind, refs })
}

/// Tell the repository's hooks what this push is, overriding anything the
/// caller's environment carried under the same names.
fn export_push_shape(command: &mut Command, shape: Option<&PushShape>) {
    match shape {
        Some(shape) => {
            command.env("AETHYME_PUSH_KIND", shape.kind.as_str());
            command.env("AETHYME_PUSH_REFS", shape.refs.join("\n"));
        }
        None => {
            command.env_remove("AETHYME_PUSH_KIND");
            command.env_remove("AETHYME_PUSH_REFS");
        }
    }
}

/// Milliseconds as the shortest readable span: `850ms`, `4.2s`, `1m 31s`.
pub(crate) fn humanize_ms(milliseconds: u64) -> String {
    match milliseconds {
        0..=999 => format!("{milliseconds}ms"),
        1_000..=59_999 => format!("{:.1}s", milliseconds as f64 / 1_000.0),
        _ => humanize_duration(milliseconds / 1_000),
    }
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
        // SAFETY: `self.file` is still owned by `self` for the duration of
        // `drop`, so the descriptor is valid. `flock` takes it by value and
        // dereferences nothing. Discarding the result is deliberate: an
        // unlock failure cannot be acted on here, and the descriptor closes
        // on drop anyway, which releases the lock.
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

/// Branch names a destructive command deletes or rewrites, as a session
/// records them (`agent/<slug>`, no `refs/heads/`). Over-inclusive on
/// purpose: a name that matches no live session's branch refuses nothing.
/// Branches a gh command may touch, split by what it does to them.
#[derive(Debug, Default)]
struct GhRefTargets {
    /// Deleted, force-updated or rebased (a merge's head, `update-branch`,
    /// an API ref delete).
    rewrite: Vec<String>,
    /// Only advanced (a merge's base, a PR's chosen base).
    advance: Vec<String>,
}

impl GhRefTargets {
    fn rewrite(rewrite: Vec<String>) -> Self {
        Self {
            rewrite,
            advance: Vec::new(),
        }
    }

    fn advance(advance: Vec<String>) -> Self {
        Self {
            rewrite: Vec::new(),
            advance,
        }
    }

    fn is_empty(&self) -> bool {
        self.rewrite.is_empty() && self.advance.is_empty()
    }
}

fn destructive_branch_targets(provider: OperationProvider, args: &[String]) -> Vec<String> {
    match provider {
        OperationProvider::Git => git_destructive_branch_targets(args),
        // gh commands are analyzed fail-closed by `gh_ref_guard`, which the
        // guard consults directly.
        OperationProvider::Github => Vec::new(),
    }
}

/// The branch refs an explicit Git push may advance or rewrite. A push with
/// implicit, wildcard, or set-expanding ref selection is rejected by the
/// coordinated-operation guard instead of guessing what Git configuration
/// will update.
#[derive(Debug, Default)]
struct GitPushBranchTargets {
    advance: Vec<String>,
    rewrite: Vec<String>,
}

impl GitPushBranchTargets {
    fn all(&self) -> Vec<String> {
        self.rewrite.iter().chain(&self.advance).cloned().collect()
    }
}

/// Parse the branch targets of push and send-pack. Ok(None) means the argv
/// names another Git subcommand; an error means Git can select or update
/// refs in a way this parser cannot safely classify.
fn git_push_branch_targets(argv: &[String]) -> Result<Option<GitPushBranchTargets>, String> {
    let Some(args) = git_subcommand_args(argv) else {
        return Ok(None);
    };
    let Some((command, rest)) = args.split_first() else {
        return Ok(None);
    };
    if !matches!(command.as_str(), "push" | "send-pack") {
        return Ok(None);
    }

    let mut positionals = Vec::new();
    let mut force_all = false;
    let mut delete_all = false;
    let mut expands_refs = None;
    let mut index = 0;
    let mut end_of_options = false;
    while index < rest.len() {
        let arg = rest[index].as_str();
        index += 1;
        if end_of_options {
            positionals.push(arg);
            continue;
        }
        if arg == "--" {
            end_of_options = true;
            continue;
        }
        match arg {
            "--force" | "--force-if-includes" | "--force-with-lease" => force_all = true,
            "--delete" => delete_all = true,
            "--all" | "--branches" | "--mirror" | "--prune" => {
                expands_refs = Some(arg);
            }
            "-o"
            | "--push-option"
            | "--repo"
            | "--receive-pack"
            | "--exec"
            | "--recurse-submodules" => {
                if index == rest.len() {
                    return Err(format!("{arg} is missing its value"));
                }
                index += 1;
            }
            "--atomic" | "--dry-run" | "--follow-tags" | "--no-follow-tags" | "--no-verify"
            | "--tags" | "--porcelain" | "--progress" | "--no-progress" | "--quiet"
            | "--verbose" | "--set-upstream" | "--signed" | "--no-signed" | "--thin"
            | "--no-thin" | "--ipv4" | "--ipv6" => {}
            _ if arg.starts_with("--force-with-lease=") => force_all = true,
            _ if arg.starts_with("--push-option=")
                || arg.starts_with("--repo=")
                || arg.starts_with("--receive-pack=")
                || arg.starts_with("--exec=")
                || arg.starts_with("--recurse-submodules=")
                || arg.starts_with("--signed=") => {}
            _ if arg.starts_with('-') => {
                let Some(flags) = arg.strip_prefix('-') else {
                    unreachable!();
                };
                if flags.is_empty() || flags.starts_with('-') {
                    return Err(format!("unrecognized push option {arg:?}"));
                }
                for (offset, flag) in flags.char_indices() {
                    match flag {
                        'f' => force_all = true,
                        'd' => delete_all = true,
                        'u' | 'q' | 'v' | 'n' | '4' | '6' => {}
                        'o' => {
                            let attached = flags.get(offset + flag.len_utf8()..).unwrap_or("");
                            if attached.is_empty() {
                                if index == rest.len() {
                                    return Err("-o is missing its value".into());
                                }
                                index += 1;
                            }
                            break;
                        }
                        _ => return Err(format!("unrecognized push option {arg:?}")),
                    }
                }
            }
            _ => positionals.push(arg),
        }
    }

    if let Some(mode) = expands_refs {
        return Err(format!(
            "{mode} selects additional refs outside explicit refspecs"
        ));
    }
    if positionals.is_empty() {
        return Err("no explicit remote was supplied".into());
    }
    let refspecs = &positionals[1..];
    if refspecs.is_empty() {
        return Err("no explicit refspec was supplied; Git configuration selects the refs".into());
    }

    let mut targets = GitPushBranchTargets::default();
    for refspec in refspecs {
        let forced = refspec.starts_with('+');
        let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
        if refspec.is_empty() || refspec.starts_with('+') {
            return Err(format!("invalid or ambiguous refspec {refspec:?}"));
        }
        if refspec.matches(':').count() > 1 {
            return Err(format!("refspec {refspec:?} contains multiple separators"));
        }
        if refspec.chars().any(|ch| matches!(ch, '*' | '?' | '[')) {
            return Err(format!(
                "wildcard refspec {refspec:?} expands the target set"
            ));
        }
        let destination = if delete_all {
            refspec.split_once(':').map_or(refspec, |(_, dst)| dst)
        } else if let Some((source, destination)) = refspec.split_once(':') {
            if destination.contains(':') {
                return Err(format!("refspec {refspec:?} contains multiple separators"));
            }
            if destination.is_empty() {
                source
            } else {
                destination
            }
        } else {
            if refspec == "HEAD" || refspec.contains("@{") {
                return Err(format!(
                    "refspec {refspec:?} needs repository state to determine its destination"
                ));
            }
            refspec
        };
        if destination.is_empty() || destination == "HEAD" || destination.contains("@{") {
            return Err(format!(
                "refspec {refspec:?} has an unresolvable destination"
            ));
        }
        let Some(branch) = push_branch_name(destination)? else {
            continue;
        };
        if force_all || forced || delete_all || refspec.starts_with(':') {
            targets.rewrite.push(branch);
        } else {
            targets.advance.push(branch);
        }
    }
    Ok(Some(targets))
}

/// A push destination without a namespace is a branch. Full branch refs are
/// also branches; tags and other refs (including refs/remotes/*) are not.
fn push_branch_name(reference: &str) -> Result<Option<String>, String> {
    if let Some(branch) = reference.strip_prefix("refs/heads/") {
        if branch.is_empty() || branch.ends_with('/') || branch.contains("//") {
            return Err(format!("invalid branch destination {reference:?}"));
        }
        return Ok(Some(branch.to_string()));
    }
    if reference.starts_with("refs/") {
        return Ok(None);
    }
    Ok(Some(reference.to_string()))
}

fn git_destructive_branch_targets(args: &[String]) -> Vec<String> {
    let Some(subcommand_args) = git_subcommand_args(args) else {
        return Vec::new();
    };
    let Some((command, rest)) = subcommand_args.split_first() else {
        return Vec::new();
    };
    if matches!(command.as_str(), "push" | "send-pack") {
        return git_push_branch_targets(args)
            .ok()
            .flatten()
            .map_or_else(Vec::new, |targets| targets.all());
    }
    let positionals = git_positionals(rest);
    let references: Vec<&str> = match command.as_str() {
        "branch" if has_any(rest, &["-r", "--remotes"]) || has_short_flag(rest, 'r') => {
            // branch -dr origin/<branch> names the remote-tracking ref.
            positionals
                .iter()
                .map(|name| name.split_once('/').map_or(*name, |(_, branch)| branch))
                .collect()
        }
        "branch" | "update-ref" => positionals,
        _ => Vec::new(),
    };
    branch_names(references)
}

/// Strip refs/heads/ and refs/remotes/<remote>/ to the branch name.
fn branch_names(references: Vec<&str>) -> Vec<String> {
    references
        .into_iter()
        .map(|reference| {
            if let Some(branch) = reference.strip_prefix("refs/heads/") {
                branch
            } else if let Some(rest) = reference.strip_prefix("refs/remotes/") {
                rest.split_once('/').map_or(rest, |(_, branch)| branch)
            } else {
                reference
            }
        })
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
        .collect()
}

/// The subset of git_destructive_branch_targets a git command deletes or
/// moves non-fast-forward. A plain push is a fast-forward the remote enforces;
/// the parsed push plan distinguishes it from deletions and forced updates.
fn git_rewritten_branches(argv: &[String]) -> Vec<String> {
    if let Some(args) = git_subcommand_args(argv)
        && let Some((command, _)) = args.split_first()
        && matches!(command.as_str(), "push" | "send-pack")
    {
        return git_push_branch_targets(argv)
            .ok()
            .flatten()
            .map_or_else(Vec::new, |targets| targets.rewrite);
    }
    git_destructive_branch_targets(argv)
}

fn same_directory(left: &Path, right: &Path) -> bool {
    matches!(
        (left.canonicalize(), right.canonicalize()),
        (Ok(left), Ok(right)) if left == right
    )
}

/// The branches a pull request joins, read in one call.
struct PullRequestRefs {
    head: String,
    head_oid: String,
    base: String,
}

/// The head branch, head commit and base branch of pull request `number`,
/// read once with the same directory and `GH_REPO` the command will use.
fn gh_pr_refs(
    number: &str,
    cwd: &Path,
    target: &crate::ResolvedGithubTarget,
) -> Result<PullRequestRefs, String> {
    let output = provider_command(OperationProvider::Github)
        .args([
            "pr",
            "view",
            number,
            "--json",
            "headRefName,headRefOid,baseRefName",
        ])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("GH_REPO", &target.display_slug)
        .output()
        .map_err(|error| format!("cannot run gh pr view {number}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "gh pr view {number} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("gh pr view {number} returned no JSON: {error}"))?;
    let field = |name: &str| value[name].as_str().unwrap_or_default().to_string();
    let (head, head_oid, base) = (
        field("headRefName"),
        field("headRefOid"),
        field("baseRefName"),
    );
    if head.is_empty()
        || base.is_empty()
        || !(1..=64).contains(&head_oid.len())
        || !head_oid
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(format!(
            "gh pr view {number} did not name a head branch, a hex head commit and a base branch"
        ));
    }
    Ok(PullRequestRefs {
        head,
        head_oid,
        base,
    })
}

/// The `--repo` target, required to be this checkout's `origin` on
/// github.com with no `GH_HOST` pointing gh elsewhere.
fn verify_github_origin<'t>(
    cwd: &Path,
    github_target: Option<&'t crate::ResolvedGithubTarget>,
) -> Result<&'t crate::ResolvedGithubTarget, String> {
    let target = github_target.ok_or("no --repo target")?;
    if let Some(host) = std::env::var_os("GH_HOST")
        && !host.to_string_lossy().eq_ignore_ascii_case("github.com")
    {
        return Err(format!(
            "GH_HOST={} points gh at another host",
            host.to_string_lossy()
        ));
    }
    match canonical_local_repository(cwd, None) {
        Ok(Some(origin)) if origin.eq_ignore_ascii_case(&target.coordination_key) => Ok(target),
        Ok(Some(origin)) => Err(format!(
            "--repo {} is not this checkout's origin ({origin})",
            target.display_slug
        )),
        _ => Err(format!(
            "cannot verify that --repo {} is this checkout's origin",
            target.display_slug
        )),
    }
}

/// Positional arguments of a Git subcommand, skipping options and the values
/// of the options that take one as a separate argument.
fn git_positionals(args: &[String]) -> Vec<&str> {
    let mut positionals = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            positionals.extend(iter.map(String::as_str));
            break;
        }
        if matches!(
            arg.as_str(),
            "-o" | "--push-option" | "--repo" | "--receive-pack" | "--exec"
        ) {
            iter.next();
            continue;
        }
        if !arg.starts_with('-') {
            positionals.push(arg.as_str());
        }
    }
    positionals
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

/// The HTTP method `gh api` will use. gh's flag parser keeps the LAST of
/// `-X GET … -X DELETE`, so this must too: taking the first would classify a
/// branch deletion as a read (#393).
fn gh_method(args: &[String]) -> Option<&str> {
    let mut method = None;
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if matches!(arg.as_str(), "-X" | "--method") {
            method = iter.peek().map(|value| value.as_str());
        } else if let Some(value) = arg.strip_prefix("--method=") {
            method = Some(value);
        } else if let Some(value) = arg.strip_prefix("-X").filter(|value| !value.is_empty()) {
            // `-XDELETE`: the method bundled into the flag, as curl also accepts.
            method = Some(value);
        }
    }
    method
}

pub fn classify_gh(args: &[String]) -> Option<OperationEffect> {
    let command = args.first()?.as_str();
    let action = args.get(1).map(String::as_str);
    // The two exact forms `gh_ref_guard` resolves to a branch delete are
    // destructive; anything else that may write a ref is refused by the
    // guard unless acknowledged (#393).
    if matches!(
        crate::gh_ref_guard::assess(args, ""),
        crate::gh_ref_guard::Verdict::PrMerge {
            deletes_head: true,
            ..
        }
    ) || args.first().map(String::as_str) == Some("api")
        && args.get(1).map(String::as_str) == Some("-X")
        && args.get(2).map(String::as_str) == Some("DELETE")
    {
        return Some(OperationEffect::Destructive);
    }
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

pub(crate) const DESTRUCTIVE_FLAG_REQUIRED: &str =
    "destructive operation requires --destructive after resolving exact targets";
pub(crate) const GH_REPO_REQUIRED: &str = "broker gh requires --repo owner/name";
pub(crate) const REMOTE_GIT_REPO_REQUIRED: &str = "remote Git operation requires --repo owner/name";

/// The flags a refused coordinated operation was missing, when the refusal is
/// one a re-run with those flags would get past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MissingOperationFlags {
    pub destructive: bool,
    /// `Some` when `--repo` is missing: the repository inferred from the
    /// remote the command targets, or `None` inside when it cannot be.
    pub repository: Option<Option<String>>,
    /// The command writes, so the re-run needs `--reason` too.
    pub reason_required: bool,
}

/// Which flags `reason` says were missing. The destructive check runs before
/// the repository check, so a refusal for `--destructive` also reports a
/// `--repo` the same command would be refused for next.
pub(crate) fn missing_operation_flags(
    reason: &str,
    provider: OperationProvider,
    args: &[String],
    declared_effect: Option<OperationEffect>,
    has_repository: bool,
    repo: &crate::GitRepo,
) -> Option<MissingOperationFlags> {
    let needs_repository = !has_repository
        && (provider == OperationProvider::Github
            || git_operation_kind(args) == GitOperationKind::Remote);
    let destructive = match reason {
        DESTRUCTIVE_FLAG_REQUIRED => true,
        GH_REPO_REQUIRED | REMOTE_GIT_REPO_REQUIRED => false,
        _ => return None,
    };
    let repository = needs_repository.then(|| inferred_repository(provider, args, repo));
    let inferred = match provider {
        OperationProvider::Git => classify_git(args),
        OperationProvider::Github => classify_gh(args),
    };
    let reason_required = resolve_effect(inferred, declared_effect)
        .map_or(true, |(effect, _)| effect != OperationEffect::Read);
    Some(MissingOperationFlags {
        destructive,
        repository,
        reason_required,
    })
}

/// The `owner/name` a command without `--repo` would act on: the remote a Git
/// command names (or its default), or `origin` for `gh`.
fn inferred_repository(
    provider: OperationProvider,
    args: &[String],
    repo: &crate::GitRepo,
) -> Option<String> {
    let target = match provider {
        OperationProvider::Git => {
            repo.resolve_remote_command_target(git_subcommand_args(args)?, None)
        }
        OperationProvider::Github => repo.resolve_remote_target("origin", None),
    };
    target.ok().map(|target| target.display_slug)
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

/// Refuse a `--repo` that none of the session worktree's remotes identify.
///
/// A session id is resolved against the broker of the caller's directory, so
/// `--session 4 --repo owner/other` run from the wrong checkout picks up this
/// repository's session 4 and would run, journaled under it, against an
/// unrelated repository. Any configured remote counts, so a fork whose
/// `upstream` names the target still passes. A worktree whose remotes cannot
/// be resolved at all gives no evidence either way and is left alone.
pub(crate) fn refuse_session_repository_mismatch(
    session_id: i64,
    worktree: &Path,
    requested: &str,
) -> Result<(), BrokerOpError> {
    let Ok(repo) = crate::GitRepo::discover(worktree) else {
        return Ok(());
    };
    let Ok(remotes) = repo.remotes() else {
        return Ok(());
    };
    let mut mismatched = Vec::new();
    for remote in &remotes {
        match repo.resolve_remote_target(remote, Some(requested)) {
            Ok(_) => return Ok(()),
            Err(crate::RemoteTargetError::AssertionMismatch { resolved, .. }) => {
                mismatched.push(format!("{remote}: {resolved}"));
            }
            Err(_) => {}
        }
    }
    if mismatched.is_empty() {
        return Ok(());
    }
    Err(BrokerOpError::SessionRepositoryMismatch {
        session_id,
        requested: requested.trim().to_string(),
        session_repositories: mismatched.join(", "),
        worktree: worktree.display().to_string(),
    })
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

mod push_and_coordinated;
mod redaction;
pub(crate) use push_and_coordinated::{github_command, is_within, worktree_relative_push_sources};
use push_and_coordinated::{is_github_pull_request_merge, provider_command};
