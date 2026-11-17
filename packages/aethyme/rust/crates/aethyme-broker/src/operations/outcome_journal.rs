//! Durable outcome journal for coordinated `git`/`gh` operations.
//!
//! Once the provider child has exited, the broker knows the operation's
//! result. Losing that result to a busy broker database used to leave the
//! journal row `running`, which later became `outcome_unknown` and
//! write-blocked the canonical repository for every session until an operator
//! inspected external state and reconciled it by hand -- even though the
//! command itself had succeeded.
//!
//! Invariants:
//!
//! 1. **The known result is durable before the database is asked.** The exit
//!    code, the journal details the broker derived (created URL, pushed refs,
//!    reconciliation evidence) and the child's finish time are written to
//!    `<broker db dir>/run/operation-outcomes/operation-<id>.json` with an
//!    atomic rename and fsync of the file and its directory. Only then are the
//!    operation's coordination locks released and the row updated, in one
//!    `IMMEDIATE` transaction that does nothing else and commits with
//!    `synchronous = FULL`, retried with backoff for up to
//!    [`RECORD_RETRY_BUDGET`]. The journal file is removed only after that
//!    durable commit, so a power loss at any point leaves either the journal
//!    or the record.
//! 2. **A journal settles exactly one run.** It carries the operation id,
//!    session, the SHA-256 of the row's command, the row's `created_at`, the
//!    random `run_nonce` stored on the row when the run started (schema v64),
//!    and the pid that ran the child. The row is settled only when every one
//!    of them matches; a row with no nonce (run by an older binary) is never
//!    settled from a journal. A journal that does not match is set aside as
//!    `*.refused` with its reason and surfaced in `broker status`; it is never
//!    applied.
//! 3. **A pending record recovers itself.** If the database stays busy past
//!    the budget, the command reports the child's true result and names the
//!    operation whose record is pending. Every writable broker open (CLI
//!    commands and the detached worker alike) and every coordinated write,
//!    before it evaluates the write block, replays pending journals for rows
//!    still `running` or `outcome_unknown`. Replay is idempotent. A journal
//!    for a row that already holds the same outcome is discarded.
//! 4. **Disagreement is never silent.** When the row already holds a
//!    different outcome than the one this run observed (an operator
//!    reconciled it another way, for example), the observed result is what
//!    the command reports, an `operation.outcome_conflict` event is recorded,
//!    the journal is kept as `*.conflict`, and `broker status` reports it.
//! 5. **No journal, no inference.** A row with no journal -- the process died
//!    before the child's result was known -- keeps the conservative path:
//!    `outcome_unknown`, a write block, and explicit reconciliation. Success
//!    is never inferred from anything but a journal written after the child
//!    exited.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::broker::{Broker, BrokerOpError};
use crate::error::BrokerError;
use crate::store::{OperationRunBinding, OperationSettlement};
use crate::types::{CoordinatedOperation, OperationStatus};

/// Schema tag every journal carries; a journal with another tag is refused.
pub(crate) const OUTCOME_JOURNAL_SCHEMA: &str = "aethyme.coordinated-operation-outcome/v2";

/// Schema of a journal set aside as refused or conflicting.
const SET_ASIDE_SCHEMA: &str = "aethyme.coordinated-operation-outcome-set-aside/v1";

/// Directory, relative to the broker database's directory, holding journals.
const OUTCOME_JOURNAL_DIR: &str = "run/operation-outcomes";

/// How long the command that ran the child keeps retrying its own record.
pub(crate) const RECORD_RETRY_BUDGET: Duration = Duration::from_secs(60);

/// Longest single attempt: the connection's usual busy timeout.
const MAX_ATTEMPT_WAIT: Duration = Duration::from_secs(5);
const FIRST_BACKOFF: Duration = Duration::from_millis(50);
const MAX_BACKOFF: Duration = Duration::from_secs(2);

/// A replay is opportunistic: it runs on every writable open and before
/// every coordinated write, so it waits at most this long for the lock and
/// leaves the journal for the next opener otherwise.
const REPLAY_ATTEMPT_WAIT: Duration = Duration::from_secs(1);

/// `broker status` advises only about a journal older than this.
pub(crate) const UNREPLAYED_ADVICE_AFTER_MS: i64 = 10 * 60 * 1000;

#[cfg(test)]
thread_local! {
    static RECORD_RETRY_BUDGET_OVERRIDE: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// Shorten the retry budget on this thread, for tests that hold the database
/// locked past it. Recording runs on the caller's thread.
#[cfg(test)]
pub(crate) fn set_record_retry_budget_for_test(budget: Option<Duration>) {
    RECORD_RETRY_BUDGET_OVERRIDE.with(|cell| cell.set(budget));
}

fn record_retry_budget() -> Duration {
    #[cfg(test)]
    if let Some(budget) = RECORD_RETRY_BUDGET_OVERRIDE.with(std::cell::Cell::get) {
        return budget;
    }
    RECORD_RETRY_BUDGET
}

/// The known result of one run of one coordinated operation, as written to
/// disk before the database is asked to record it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct OutcomeJournal {
    pub schema: String,
    pub operation_id: i64,
    pub session_id: i64,
    /// SHA-256 of the row's exact `command_json`.
    pub command_sha256: String,
    /// The row's `created_at`: an id reused after a reset has another one.
    pub operation_created_at: i64,
    /// The random nonce stored on the row when this run started.
    pub run_nonce: String,
    /// The process that ran the child; the row's `pid`.
    pub writer_pid: i64,
    pub status: String,
    pub exit_code: Option<i64>,
    pub details_json: String,
    /// When the child's result became known, in epoch milliseconds.
    pub child_finished_at: i64,
}

impl OutcomeJournal {
    /// The run binding this journal claims, completed with the row's own
    /// command (whose hash was already checked against the journal).
    fn binding(&self, command_json: &str) -> OperationRunBinding {
        OperationRunBinding {
            operation_id: self.operation_id,
            session_id: self.session_id,
            command_json: command_json.to_string(),
            created_at: self.operation_created_at,
            run_nonce: self.run_nonce.clone(),
            pid: self.writer_pid,
        }
    }
}

/// A journal that was refused or conflicted, kept for review with its reason.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SetAsideJournal {
    schema: String,
    operation_id: i64,
    /// `refused` or `conflict`.
    kind: String,
    reason: String,
    set_aside_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recorded_status: Option<String>,
    /// The journal as it was on disk (lossy UTF-8 when it was unreadable).
    journal: String,
}

/// Evidence that an operation's outcome is journaled but not yet recorded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PendingOutcomeRecord {
    pub operation_id: i64,
    pub journal_path: String,
    pub database_error: String,
    pub attempts: u32,
    pub waited_ms: u64,
}

impl PendingOutcomeRecord {
    /// The line a command prints when its record is pending.
    pub fn message(&self, status: OperationStatus) -> String {
        format!(
            "operation {} finished ({}) but the broker database stayed busy for {}; \
             its outcome is journaled and will be recorded automatically by the next \
             broker command (inspect: aethyme broker advanced operations show {} --json)",
            self.operation_id,
            status.as_str(),
            crate::operations::humanize_ms(self.waited_ms),
            self.operation_id,
        )
    }
}

/// This run observed one outcome, the database holds another.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OutcomeConflict {
    pub operation_id: i64,
    /// What this run's child actually did; this is what the command reports.
    pub observed_status: String,
    /// What the database row holds (`missing` when the row is gone).
    pub recorded_status: String,
    pub reason: String,
    /// Where the journal was kept for review, when it could be written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal_path: Option<String>,
}

impl OutcomeConflict {
    pub fn message(&self) -> String {
        format!(
            "operation {}: this run observed {} but the broker database records {} ({}); \
             the observed result is reported, the database was not changed, and the journal \
             is kept{} for review (inspect: aethyme broker advanced operations show {} --json)",
            self.operation_id,
            self.observed_status,
            self.recorded_status,
            self.reason,
            self.journal_path
                .as_deref()
                .map(|path| format!(" as {path}"))
                .unwrap_or_default(),
            self.operation_id,
        )
    }
}

pub(crate) struct RecordedOutcome {
    pub operation: CoordinatedOperation,
    pub pending: Option<PendingOutcomeRecord>,
    pub conflict: Option<OutcomeConflict>,
}

/// What one replay pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct OutcomeReplaySummary {
    pub replayed: usize,
    pub discarded: usize,
    pub refused: usize,
    pub conflicts: usize,
    /// Journals left in place because the database was still busy.
    pub pending: usize,
}

pub(crate) fn command_sha256(command_json: &str) -> String {
    format!("{:x}", Sha256::digest(command_json.as_bytes()))
}

pub(crate) fn outcome_journal_dir(db_path: &Path) -> Option<PathBuf> {
    Some(db_path.parent()?.join(OUTCOME_JOURNAL_DIR))
}

fn journal_file_name(operation_id: i64) -> String {
    format!("operation-{operation_id}.json")
}

fn journal_operation_id(file_name: &str) -> Option<i64> {
    file_name
        .strip_prefix("operation-")?
        .strip_suffix(".json")?
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
}

/// The operation id of a set-aside journal file and its kind.
fn set_aside_operation_id(file_name: &str) -> Option<(i64, &'static str)> {
    let (stem, kind) = if let Some(stem) = file_name.strip_suffix(".refused") {
        (stem, "refused")
    } else {
        (file_name.strip_suffix(".conflict")?, "conflict")
    };
    Some((journal_operation_id(stem)?, kind))
}

/// Whether a journal for `operation_id` is waiting to be recorded.
pub(crate) fn outcome_journal_pending(db_path: &Path, operation_id: i64) -> bool {
    outcome_journal_dir(db_path)
        .is_some_and(|dir| dir.join(journal_file_name(operation_id)).is_file())
}

fn is_busy(error: &BrokerError) -> bool {
    matches!(
        error,
        BrokerError::Sqlite(error)
            if matches!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            )
    )
}

/// The outcome a status stands for, for comparing an observed result with a
/// recorded one. `None` for states that assert nothing (an unknown outcome is
/// not evidence, so it never conflicts).
fn outcome_class(status: OperationStatus) -> Option<&'static str> {
    match status {
        OperationStatus::Succeeded | OperationStatus::ReconciledSucceeded => Some("succeeded"),
        OperationStatus::Failed | OperationStatus::ReconciledFailed => Some("failed"),
        _ => None,
    }
}

fn outcomes_conflict(observed: OperationStatus, recorded: OperationStatus) -> bool {
    matches!(
        (outcome_class(observed), outcome_class(recorded)),
        (Some(left), Some(right)) if left != right
    )
}

/// Atomic, durable publication: a synced temporary in the same directory,
/// renamed over the target, then the directory synced so the entry survives
/// a crash.
fn write_durably(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::atomic_file::with_synced_temporary(path, bytes, |temporary| {
        std::fs::rename(temporary, path)
    })?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn write_journal(path: &Path, journal: &OutcomeJournal) -> io::Result<()> {
    write_durably(
        path,
        &serde_json::to_vec_pretty(journal).map_err(io::Error::other)?,
    )
}

fn remove_journal(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => eprintln!(
            "warning: aethyme could not remove the recorded outcome journal {}: {error}",
            path.display()
        ),
    }
}

fn set_aside_path(journal_path: &Path, kind: &str) -> PathBuf {
    let mut name = journal_path.as_os_str().to_owned();
    name.push(".");
    name.push(kind);
    PathBuf::from(name)
}

/// Keep a journal for review as `<journal>.<kind>` with its reason, then
/// remove the original. `raw` is the journal text when the file is not (or
/// no longer) on disk. Returns where it was kept.
fn set_aside(
    journal_path: &Path,
    operation_id: i64,
    kind: &'static str,
    reason: &str,
    observed: Option<&str>,
    recorded: Option<&str>,
    raw: Option<String>,
) -> Option<PathBuf> {
    crate::phase_telemetry::add(
        if kind == "refused" {
            "operation_outcome_journal_refused"
        } else {
            "operation_outcome_journal_conflict"
        },
        Duration::ZERO,
    );
    let journal = raw.unwrap_or_else(|| {
        std::fs::read(journal_path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    });
    let record = SetAsideJournal {
        schema: SET_ASIDE_SCHEMA.into(),
        operation_id,
        kind: kind.into(),
        reason: reason.into(),
        set_aside_at: now_ms(),
        observed_status: observed.map(str::to_owned),
        recorded_status: recorded.map(str::to_owned),
        journal,
    };
    let target = set_aside_path(journal_path, kind);
    let written = serde_json::to_vec_pretty(&record)
        .map_err(io::Error::other)
        .and_then(|bytes| write_durably(&target, &bytes));
    match written {
        Ok(()) => {
            remove_journal(journal_path);
            eprintln!(
                "warning: aethyme set aside the outcome journal of coordinated operation \
                 {operation_id} as {kind}: {reason}; kept as {}",
                target.display()
            );
            Some(target)
        }
        Err(error) => {
            eprintln!(
                "warning: aethyme could not set aside the outcome journal of coordinated \
                 operation {operation_id} ({kind}: {reason}): {error}"
            );
            None
        }
    }
}

fn now_ms() -> i64 {
    crate::operations::unix_now_ms()
}

/// Why `journal` cannot settle `row` (whose stored nonce is `row_nonce`), or
/// `None` when it is that run's journal.
fn journal_mismatch(
    journal: &OutcomeJournal,
    row: &CoordinatedOperation,
    row_nonce: Option<&str>,
) -> Option<String> {
    if command_sha256(&row.command_json) != journal.command_sha256 {
        return Some("the command differs from the journal's".into());
    }
    journal
        .binding(&row.command_json)
        .mismatch(row, row_nonce)
        .map(|field| format!("the {field} differs from the journal's"))
}

impl Broker {
    /// Record a coordinated operation's outcome once its child has exited.
    ///
    /// See the module invariants: journal first; then `release` (which ends
    /// the operation's host lane and drops its coordination locks, so other
    /// sessions do not queue behind the database), then a bounded, retried
    /// `IMMEDIATE` update bound to this run. Without a journal, `release`
    /// runs only after the record, as before. A database that stays busy
    /// past the budget yields a [`PendingOutcomeRecord`] instead of an error,
    /// as long as the journal was written; without a journal the database
    /// error is returned, which leaves the row for the conservative
    /// unknown-outcome path.
    pub(crate) fn record_coordinated_outcome<R>(
        &mut self,
        operation: &CoordinatedOperation,
        run_nonce: &str,
        status: OperationStatus,
        exit_code: Option<i64>,
        details_json: &str,
        release: R,
    ) -> Result<RecordedOutcome, BrokerOpError>
    where
        R: FnOnce() -> Result<(), BrokerOpError>,
    {
        let started = Instant::now();
        let budget = record_retry_budget();
        let child_finished_at = now_ms();
        let binding = OperationRunBinding {
            operation_id: operation.id,
            session_id: operation.session_id,
            command_json: operation.command_json.clone(),
            created_at: operation.created_at,
            run_nonce: run_nonce.to_string(),
            pid: operation.pid,
        };
        let journal_path = outcome_journal_dir(self.store().db_path())
            .map(|dir| dir.join(journal_file_name(operation.id)));
        let journal = OutcomeJournal {
            schema: OUTCOME_JOURNAL_SCHEMA.into(),
            operation_id: operation.id,
            session_id: operation.session_id,
            command_sha256: command_sha256(&operation.command_json),
            operation_created_at: operation.created_at,
            run_nonce: run_nonce.to_string(),
            writer_pid: operation.pid,
            status: status.as_str().into(),
            exit_code,
            details_json: details_json.into(),
            child_finished_at,
        };
        let journaled = match journal_path.as_deref() {
            Some(path) => match write_journal(path, &journal) {
                Ok(()) => {
                    crate::phase_telemetry::add(
                        "operation_outcome_journal_written",
                        Duration::ZERO,
                    );
                    true
                }
                Err(error) => {
                    crate::phase_telemetry::add(
                        "operation_outcome_journal_write_failed",
                        Duration::ZERO,
                    );
                    eprintln!(
                        "warning: aethyme could not journal operation {}'s outcome before recording it: {error}",
                        operation.id
                    );
                    false
                }
            },
            None => false,
        };
        let mut release = Some(release);
        let mut run_release = || -> Result<(), BrokerOpError> {
            match release.take() {
                Some(release) => release(),
                None => Ok(()),
            }
        };
        if journaled {
            // The outcome is on disk: holding the locks through up to a
            // minute of database retries would only make every other session
            // queue behind a write that is already decided. A writer that
            // takes the lock now replays this journal before its write-block
            // check.
            run_release()?;
            crate::phase_telemetry::add("operation_outcome_locks_released_early", Duration::ZERO);
        }
        let observed = |pending: Option<PendingOutcomeRecord>,
                        conflict: Option<OutcomeConflict>| {
            let mut observed = operation.clone();
            observed.status = status;
            observed.exit_code = exit_code;
            observed.details_json = Some(details_json.to_string());
            observed.updated_at = child_finished_at;
            observed.finished_at = Some(child_finished_at);
            RecordedOutcome {
                operation: observed,
                pending,
                conflict,
            }
        };
        let mut attempts: u32 = 0;
        let mut backoff = FIRST_BACKOFF;
        let finish = |outcome: &str, attempts: u32| {
            let waited = started.elapsed();
            crate::phase_telemetry::add("operation_outcome_record_wait", waited);
            crate::phase_telemetry::attach(
                "operation_outcome_record",
                json!({
                    "operation_id": operation.id,
                    "outcome": outcome,
                    "attempts": attempts,
                    "retries": attempts.saturating_sub(1),
                    "waited_ms": waited.as_millis() as u64,
                    "journal": if journaled { "written" } else { "write_failed" },
                }),
            );
        };
        loop {
            attempts += 1;
            let remaining = budget.saturating_sub(started.elapsed());
            let attempt_started = Instant::now();
            let attempt = self.store().settle_coordinated_operation(
                &binding,
                status,
                exit_code,
                details_json,
                remaining.min(MAX_ATTEMPT_WAIT),
            );
            let disagreement = match attempt {
                Ok(OperationSettlement::Recorded(recorded)) => {
                    // Committed with a synced WAL: the journal may go.
                    if let Some(path) = &journal_path {
                        remove_journal(path);
                    }
                    finish("recorded", attempts);
                    run_release()?;
                    return Ok(RecordedOutcome {
                        operation: recorded,
                        pending: None,
                        conflict: None,
                    });
                }
                Ok(OperationSettlement::AlreadySettled(current))
                    if !outcomes_conflict(status, current.status) =>
                {
                    // Another opener replayed this journal first, or the row
                    // was reconciled to the same outcome: nothing to add.
                    if let Some(path) = &journal_path {
                        remove_journal(path);
                    }
                    finish("already_settled", attempts);
                    run_release()?;
                    return Ok(RecordedOutcome {
                        operation: current,
                        pending: None,
                        conflict: None,
                    });
                }
                Ok(OperationSettlement::AlreadySettled(current)) => (
                    current.status.as_str().to_string(),
                    "the row was already settled with another outcome".to_string(),
                ),
                Ok(OperationSettlement::Mismatch(current)) => {
                    let reason = self
                        .store()
                        .coordinated_operation_run_nonce(operation.id)
                        .ok()
                        .and_then(|nonce| binding.mismatch(&current, nonce.as_deref()))
                        .unwrap_or("run binding");
                    (
                        current.status.as_str().to_string(),
                        format!("the row is no longer this run: its {reason} differs"),
                    )
                }
                Ok(OperationSettlement::Missing) => (
                    "missing".to_string(),
                    "the operation row no longer exists".to_string(),
                ),
                Err(error) => {
                    crate::phase_telemetry::add(
                        "operation_outcome_record_attempt_failed",
                        attempt_started.elapsed(),
                    );
                    let remaining = budget.saturating_sub(started.elapsed());
                    if is_busy(&error) && !remaining.is_zero() {
                        crate::phase_telemetry::add(
                            "operation_outcome_record_retry",
                            Duration::ZERO,
                        );
                        std::thread::sleep(backoff.min(remaining));
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        continue;
                    }
                    if !journaled {
                        finish("failed", attempts);
                        run_release()?;
                        return Err(error.into());
                    }
                    finish("pending", attempts);
                    crate::phase_telemetry::add("operation_outcome_record_pending", Duration::ZERO);
                    let pending = PendingOutcomeRecord {
                        operation_id: operation.id,
                        journal_path: journal_path
                            .as_deref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_default(),
                        database_error: error.to_string(),
                        attempts,
                        waited_ms: started.elapsed().as_millis() as u64,
                    };
                    return Ok(observed(Some(pending), None));
                }
            };
            // Invariant 4: report what this run observed, change nothing in
            // the database row, and keep the evidence where it is seen.
            let (recorded_status, reason) = disagreement;
            finish("conflict", attempts);
            let kept = journal_path.as_deref().and_then(|path| {
                set_aside(
                    path,
                    operation.id,
                    "conflict",
                    &reason,
                    Some(status.as_str()),
                    Some(&recorded_status),
                    (!journaled)
                        .then(|| serde_json::to_string_pretty(&journal).unwrap_or_default()),
                )
            });
            record_conflict_event(
                self,
                operation,
                status.as_str(),
                &recorded_status,
                &reason,
                "record",
            );
            run_release()?;
            return Ok(observed(
                None,
                Some(OutcomeConflict {
                    operation_id: operation.id,
                    observed_status: status.as_str().into(),
                    recorded_status,
                    reason,
                    journal_path: kept.map(|path| path.display().to_string()),
                }),
            ));
        }
    }

    /// Replay every pending outcome journal (module invariant 3).
    ///
    /// `trigger` names the caller in the replayed row's details. Best effort
    /// per journal: a busy database leaves that journal for the next opener.
    pub(crate) fn replay_operation_outcome_journals(
        &mut self,
        trigger: &'static str,
    ) -> Result<OutcomeReplaySummary, BrokerOpError> {
        let mut summary = OutcomeReplaySummary::default();
        let Some(dir) = outcome_journal_dir(self.store().db_path()) else {
            return Ok(summary);
        };
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(summary),
            Err(error) => {
                return Err(BrokerOpError::Store(BrokerError::Io {
                    path: dir,
                    source: error,
                }));
            }
        };
        let mut journals: Vec<(i64, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                let id = journal_operation_id(name.to_str()?)?;
                Some((id, entry.path()))
            })
            .collect();
        if journals.is_empty() {
            return Ok(summary);
        }
        journals.sort();
        let _span = crate::phase_telemetry::span("operation_outcome_journal_replay");
        for (id, path) in journals {
            match self.replay_one_outcome_journal(id, &path, trigger) {
                ReplayResult::Replayed => summary.replayed += 1,
                ReplayResult::Discarded => summary.discarded += 1,
                ReplayResult::Refused => summary.refused += 1,
                ReplayResult::Conflict => summary.conflicts += 1,
                ReplayResult::Pending => summary.pending += 1,
            }
        }
        for (name, count) in [
            ("operation_outcome_journal_replayed", summary.replayed),
            ("operation_outcome_journal_discarded", summary.discarded),
            ("operation_outcome_journal_still_pending", summary.pending),
        ] {
            for _ in 0..count {
                crate::phase_telemetry::add(name, Duration::ZERO);
            }
        }
        Ok(summary)
    }

    /// Read and check `operation`'s journal against the row, without
    /// writing. `Ok` is a journal of exactly the row's current run.
    fn matching_outcome_journal(&mut self, id: i64, path: &Path) -> Result<MatchedJournal, String> {
        let journal = std::fs::read(path)
            .map_err(|error| format!("unreadable journal ({error})"))
            .and_then(|bytes| {
                serde_json::from_slice::<OutcomeJournal>(&bytes)
                    .map_err(|error| format!("unreadable journal ({error})"))
            })?;
        if journal.schema != OUTCOME_JOURNAL_SCHEMA || journal.operation_id != id {
            return Err(format!(
                "schema {:?} for operation {} does not match file operation {id}",
                journal.schema, journal.operation_id
            ));
        }
        let status = match OperationStatus::parse(&journal.status) {
            Ok(status)
                if matches!(
                    status,
                    OperationStatus::Succeeded
                        | OperationStatus::Failed
                        | OperationStatus::OutcomeUnknown
                ) =>
            {
                status
            }
            _ => {
                return Err(format!(
                    "journaled status {:?} is not a final outcome",
                    journal.status
                ));
            }
        };
        let Some(row) = self
            .store()
            .coordinated_operation(id)
            .map_err(|error| format!("database read failed: {error}"))?
        else {
            return Err(format!("operation {id} does not exist"));
        };
        let nonce = self
            .store()
            .coordinated_operation_run_nonce(id)
            .map_err(|error| format!("database read failed: {error}"))?;
        if let Some(reason) = journal_mismatch(&journal, &row, nonce.as_deref()) {
            return Err(reason);
        }
        Ok(MatchedJournal {
            journal,
            status,
            row,
        })
    }

    /// Whether `operation` has a journal of exactly its current run with a
    /// definite outcome (`succeeded` or `failed`). The write-block check uses
    /// this when a replay could not write: such a row is decided, not
    /// unknown, even though its record is late.
    pub(crate) fn has_decided_outcome_journal(&mut self, operation: &CoordinatedOperation) -> bool {
        let Some(path) = outcome_journal_dir(self.store().db_path())
            .map(|dir| dir.join(journal_file_name(operation.id)))
        else {
            return false;
        };
        if !path.is_file() {
            return false;
        }
        self.matching_outcome_journal(operation.id, &path)
            .is_ok_and(|matched| {
                matches!(
                    matched.status,
                    OperationStatus::Succeeded | OperationStatus::Failed
                )
            })
    }

    fn replay_one_outcome_journal(
        &mut self,
        id: i64,
        path: &Path,
        trigger: &'static str,
    ) -> ReplayResult {
        let MatchedJournal {
            journal,
            status,
            row: current,
        } = match self.matching_outcome_journal(id, path) {
            Ok(matched) => matched,
            Err(reason) => {
                // A file another process removed between listing and reading
                // was simply replayed by that process.
                if !path.exists() {
                    return ReplayResult::Discarded;
                }
                if reason.starts_with("database read failed") {
                    return ReplayResult::Pending;
                }
                set_aside(path, id, "refused", &reason, None, None, None);
                return ReplayResult::Refused;
            }
        };
        if !matches!(
            current.status,
            OperationStatus::Running | OperationStatus::OutcomeUnknown
        ) {
            return self.settled_row_meets_journal(path, &journal, status, &current, "replay");
        }
        let mut details = serde_json::from_str::<serde_json::Value>(&journal.details_json)
            .ok()
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| json!({}));
        details["outcome_journal"] = json!({
            "replayed_by": trigger,
            "replayed_at": now_ms(),
            "child_finished_at": journal.child_finished_at,
            "writer_pid": journal.writer_pid,
            "status_before_replay": current.status.as_str(),
        });
        match self.store().settle_coordinated_operation(
            &journal.binding(&current.command_json),
            status,
            journal.exit_code,
            &details.to_string(),
            REPLAY_ATTEMPT_WAIT,
        ) {
            Ok(OperationSettlement::Recorded(_)) => {
                remove_journal(path);
                ReplayResult::Replayed
            }
            Ok(OperationSettlement::AlreadySettled(settled)) => {
                self.settled_row_meets_journal(path, &journal, status, &settled, "replay")
            }
            Ok(OperationSettlement::Mismatch(changed)) => {
                set_aside(
                    path,
                    id,
                    "refused",
                    &format!(
                        "operation {id} changed while replaying (now session {})",
                        changed.session_id
                    ),
                    None,
                    None,
                    None,
                );
                ReplayResult::Refused
            }
            Ok(OperationSettlement::Missing) => {
                set_aside(
                    path,
                    id,
                    "refused",
                    &format!("operation {id} disappeared while replaying"),
                    None,
                    None,
                    None,
                );
                ReplayResult::Refused
            }
            Err(error) => {
                if !is_busy(&error) {
                    crate::warn_unrecorded::<(), _>(
                        "replay a pending coordinated operation outcome",
                        Err(error),
                    );
                }
                ReplayResult::Pending
            }
        }
    }

    /// A journal of this exact run whose row already left `running`: the
    /// same outcome is a no-op, a different one a conflict (invariant 4).
    fn settled_row_meets_journal(
        &mut self,
        path: &Path,
        journal: &OutcomeJournal,
        observed: OperationStatus,
        row: &CoordinatedOperation,
        source: &'static str,
    ) -> ReplayResult {
        if !outcomes_conflict(observed, row.status) {
            remove_journal(path);
            return ReplayResult::Discarded;
        }
        let reason = "the row was already settled with another outcome";
        set_aside(
            path,
            journal.operation_id,
            "conflict",
            reason,
            Some(observed.as_str()),
            Some(row.status.as_str()),
            None,
        );
        record_conflict_event(
            self,
            row,
            observed.as_str(),
            row.status.as_str(),
            reason,
            source,
        );
        ReplayResult::Conflict
    }
}

/// Best effort: the conflict is already on disk as `*.conflict` and in the
/// command's output; the event makes it visible to event consumers too.
fn record_conflict_event(
    broker: &mut Broker,
    operation: &CoordinatedOperation,
    observed: &str,
    recorded: &str,
    reason: &str,
    source: &str,
) {
    let payload = json!({
        "operation_id": operation.id,
        "provider": operation.provider.as_str(),
        "repository": operation.repository,
        "observed_status": observed,
        "recorded_status": recorded,
        "reason": reason,
        "source": source,
    })
    .to_string();
    crate::warn_unrecorded(
        "record a coordinated operation outcome conflict event",
        broker.store().append_event(
            "operation.outcome_conflict",
            Some(operation.session_id),
            Some(&payload),
        ),
    );
}

/// A journal checked against the current run of its row.
struct MatchedJournal {
    journal: OutcomeJournal,
    status: OperationStatus,
    row: CoordinatedOperation,
}

enum ReplayResult {
    Replayed,
    Discarded,
    Refused,
    Conflict,
    Pending,
}

/// Journals that have waited longer than [`UNREPLAYED_ADVICE_AFTER_MS`], as
/// `(operation id, age in ms)`, oldest first.
pub(crate) fn unreplayed_outcome_journals(db_path: &Path, now_ms: i64) -> Vec<(i64, i64)> {
    let Some(dir) = outcome_journal_dir(db_path) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stale: Vec<(i64, i64)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let id = journal_operation_id(entry.file_name().to_str()?)?;
            let written_at = std::fs::read(entry.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<OutcomeJournal>(&bytes).ok())
                .map(|journal| journal.child_finished_at)
                .or_else(|| {
                    let modified = entry.metadata().ok()?.modified().ok()?;
                    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
                    Some(since.as_millis() as i64)
                })?;
            let age = now_ms.saturating_sub(written_at);
            (age > UNREPLAYED_ADVICE_AFTER_MS).then_some((id, age))
        })
        .collect();
    stale.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    stale
}

/// `broker status` warnings for outcome journals:
/// - one per journal that has stayed unreplayed for more than ten minutes (a
///   fresh journal is normal and silent: the next writable open records it);
/// - one per refused journal while its operation is still unresolved, naming
///   the reason, because that operation keeps its write block;
/// - one per conflicting journal, until it is reviewed and removed.
pub(crate) fn outcome_journal_advice(
    store: &crate::BrokerStore,
    now_ms: i64,
) -> Vec<crate::StatusAdvice> {
    let db_path = store.db_path();
    let mut advice: Vec<crate::StatusAdvice> = unreplayed_outcome_journals(db_path, now_ms)
        .into_iter()
        .map(|(operation_id, age_ms)| crate::StatusAdvice {
            id: "operation.outcome-journal-unreplayed",
            severity: crate::StatusAdviceSeverity::Warning,
            reason: "a coordinated operation's known outcome is journaled but not yet recorded",
            summary: format!(
                "operation {operation_id}'s outcome has waited {} in its journal because the \
                 broker database stayed busy; any writable broker command records it",
                crate::operations::humanize_ms(age_ms.max(0) as u64)
            ),
            session_id: None,
            queue_entry_id: None,
            evidence: vec![format!(
                "journal: {OUTCOME_JOURNAL_DIR}/{}",
                journal_file_name(operation_id)
            )],
            commands: vec![format!(
                "aethyme broker advanced operations show {operation_id} --json"
            )],
        })
        .collect();
    let Some(dir) = outcome_journal_dir(db_path) else {
        return advice;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return advice;
    };
    let mut kept: Vec<(i64, &'static str, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let (id, kind) = set_aside_operation_id(entry.file_name().to_str()?)?;
            Some((id, kind, entry.path()))
        })
        .collect();
    kept.sort();
    for (operation_id, kind, path) in kept {
        let record = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SetAsideJournal>(&bytes).ok());
        let reason = record
            .as_ref()
            .map(|record| record.reason.clone())
            .unwrap_or_else(|| "reason not recorded".into());
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let show = format!("aethyme broker advanced operations show {operation_id} --json");
        if kind == "refused" {
            // Only while the refusal still explains something: an operation
            // that has since been settled or reconciled blocks nothing.
            let unresolved = match store.coordinated_operation(operation_id) {
                Ok(Some(row)) => matches!(
                    row.status,
                    OperationStatus::Running | OperationStatus::OutcomeUnknown
                ),
                Ok(None) => true,
                Err(_) => true,
            };
            if !unresolved {
                continue;
            }
            advice.push(crate::StatusAdvice {
                id: "operation.outcome-journal-refused",
                severity: crate::StatusAdviceSeverity::Warning,
                reason: "an outcome journal was refused, so its operation keeps an unknown outcome",
                summary: format!(
                    "operation {operation_id}'s outcome journal was refused ({reason}); it was \
                     not applied, so the operation keeps its unknown outcome and write block \
                     until external state is inspected and it is reconciled"
                ),
                session_id: None,
                queue_entry_id: None,
                evidence: vec![format!("refused journal: {OUTCOME_JOURNAL_DIR}/{file}")],
                commands: vec![show],
            });
        } else {
            let observed = record
                .as_ref()
                .and_then(|record| record.observed_status.clone())
                .unwrap_or_else(|| "unknown".into());
            let recorded = record
                .as_ref()
                .and_then(|record| record.recorded_status.clone())
                .unwrap_or_else(|| "unknown".into());
            advice.push(crate::StatusAdvice {
                id: "operation.outcome-journal-conflict",
                severity: crate::StatusAdviceSeverity::Warning,
                reason: "a coordinated operation's observed outcome disagrees with its record",
                summary: format!(
                    "operation {operation_id}: its run observed {observed} but the database \
                     records {recorded} ({reason}); inspect external state, correct the record \
                     if needed, then remove the conflict file"
                ),
                session_id: None,
                queue_entry_id: None,
                evidence: vec![format!("conflict journal: {OUTCOME_JOURNAL_DIR}/{file}")],
                commands: vec![show],
            });
        }
    }
    advice
}

#[cfg(test)]
mod tests;
