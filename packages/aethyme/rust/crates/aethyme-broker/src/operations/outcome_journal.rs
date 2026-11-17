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
//!    atomic rename and fsync of the file and its directory. Only then is the
//!    row updated, in one `IMMEDIATE` transaction that does nothing else,
//!    retried with backoff for up to [`RECORD_RETRY_BUDGET`]. The journal file
//!    is removed once the row holds the outcome.
//! 2. **A pending record recovers itself.** If the database stays busy past
//!    the budget, the command reports the child's true result and names the
//!    operation whose record is pending. Every writable broker open (CLI
//!    commands and the detached worker alike) and every coordinated write,
//!    before it evaluates the write block, replays pending journals for rows
//!    still `running` or `outcome_unknown`. Replay is idempotent and verifies
//!    the operation id, session id and the SHA-256 of the journaled command
//!    before it writes; a journal that does not match is refused (renamed to
//!    `*.refused`) and reported, never applied. A journal for a row that has
//!    already settled or was reconciled is discarded.
//! 3. **No journal, no inference.** A row with no journal -- the process died
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
use crate::store::OperationSettlement;
use crate::types::{CoordinatedOperation, OperationStatus};

/// Schema tag every journal carries; a journal with another tag is refused.
pub(crate) const OUTCOME_JOURNAL_SCHEMA: &str = "aethyme.coordinated-operation-outcome/v1";

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

/// The known result of one coordinated operation, as written to disk before
/// the database is asked to record it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct OutcomeJournal {
    pub schema: String,
    pub operation_id: i64,
    pub session_id: i64,
    /// SHA-256 of the row's exact `command_json`, so a journal can only ever
    /// settle the command that produced it.
    pub command_sha256: String,
    pub status: String,
    pub exit_code: Option<i64>,
    pub details_json: String,
    /// When the child's result became known, in epoch milliseconds.
    pub child_finished_at: i64,
    pub writer_pid: i64,
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

pub(crate) struct RecordedOutcome {
    pub operation: CoordinatedOperation,
    pub pending: Option<PendingOutcomeRecord>,
}

/// What one replay pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct OutcomeReplaySummary {
    pub replayed: usize,
    pub discarded: usize,
    pub refused: usize,
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

/// Atomic, durable publication: a synced temporary in the same directory,
/// renamed over the target, then the directory synced so the entry survives
/// a crash.
fn write_journal(path: &Path, journal: &OutcomeJournal) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(journal).map_err(io::Error::other)?;
    crate::atomic_file::with_synced_temporary(path, &bytes, |temporary| {
        std::fs::rename(temporary, path)
    })?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
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

fn refuse_journal(path: &Path, reason: &str) {
    crate::phase_telemetry::add("operation_outcome_journal_refused", Duration::ZERO);
    let mut refused = path.as_os_str().to_owned();
    refused.push(".refused");
    let moved = std::fs::rename(path, &refused);
    eprintln!(
        "warning: aethyme refused the coordinated operation outcome journal {}: {reason}{}",
        path.display(),
        match moved {
            Ok(()) => format!("; kept as {}", PathBuf::from(&refused).display()),
            Err(error) => format!("; it could not be set aside ({error})"),
        }
    );
}

fn now_ms() -> i64 {
    crate::operations::unix_now_ms()
}

impl Broker {
    /// Record a coordinated operation's outcome once its child has exited.
    ///
    /// See the module invariants: journal first, then a bounded, retried
    /// `IMMEDIATE` update. A database that stays busy past the budget yields
    /// a [`PendingOutcomeRecord`] instead of an error, as long as the journal
    /// was written; without a journal the database error is returned, which
    /// leaves the row for the conservative unknown-outcome path.
    pub(crate) fn record_coordinated_outcome(
        &mut self,
        operation: &CoordinatedOperation,
        status: OperationStatus,
        exit_code: Option<i64>,
        details_json: &str,
    ) -> Result<RecordedOutcome, BrokerOpError> {
        let started = Instant::now();
        let budget = record_retry_budget();
        let child_finished_at = now_ms();
        let journal_path = outcome_journal_dir(self.store().db_path())
            .map(|dir| dir.join(journal_file_name(operation.id)));
        let journal = OutcomeJournal {
            schema: OUTCOME_JOURNAL_SCHEMA.into(),
            operation_id: operation.id,
            session_id: operation.session_id,
            command_sha256: command_sha256(&operation.command_json),
            status: status.as_str().into(),
            exit_code,
            details_json: details_json.into(),
            child_finished_at,
            writer_pid: i64::from(std::process::id()),
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
                operation.id,
                operation.session_id,
                &operation.command_json,
                status,
                exit_code,
                details_json,
                remaining.min(MAX_ATTEMPT_WAIT),
            );
            match attempt {
                Ok(OperationSettlement::Recorded(recorded)) => {
                    if let Some(path) = &journal_path {
                        remove_journal(path);
                    }
                    finish("recorded", attempts);
                    return Ok(RecordedOutcome {
                        operation: recorded,
                        pending: None,
                    });
                }
                Ok(OperationSettlement::AlreadySettled(current)) => {
                    // Another opener replayed this journal first, or an
                    // operator reconciled the row; either way the row now
                    // holds an outcome and this journal has done its job.
                    if let Some(path) = &journal_path {
                        remove_journal(path);
                    }
                    finish("already_settled", attempts);
                    return Ok(RecordedOutcome {
                        operation: current,
                        pending: None,
                    });
                }
                Ok(OperationSettlement::Mismatch(_) | OperationSettlement::Missing) => {
                    if let Some(path) = &journal_path {
                        remove_journal(path);
                    }
                    finish("missing", attempts);
                    return Err(BrokerError::CoordinatedOperationNotFound(operation.id).into());
                }
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
                        return Err(error.into());
                    }
                    finish("pending", attempts);
                    crate::phase_telemetry::add("operation_outcome_record_pending", Duration::ZERO);
                    let mut pending_operation = operation.clone();
                    pending_operation.status = status;
                    pending_operation.exit_code = exit_code;
                    pending_operation.details_json = Some(details_json.to_string());
                    pending_operation.updated_at = child_finished_at;
                    pending_operation.finished_at = Some(child_finished_at);
                    return Ok(RecordedOutcome {
                        operation: pending_operation,
                        pending: Some(PendingOutcomeRecord {
                            operation_id: operation.id,
                            journal_path: journal_path
                                .as_deref()
                                .map(|path| path.display().to_string())
                                .unwrap_or_default(),
                            database_error: error.to_string(),
                            attempts,
                            waited_ms: started.elapsed().as_millis() as u64,
                        }),
                    });
                }
            }
        }
    }

    /// Replay every pending outcome journal (module invariant 2).
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

    fn replay_one_outcome_journal(
        &mut self,
        id: i64,
        path: &Path,
        trigger: &'static str,
    ) -> ReplayResult {
        let journal = match std::fs::read(path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<OutcomeJournal>(&bytes).map_err(|error| error.to_string())
            }) {
            Ok(journal) => journal,
            Err(reason) => {
                // A file another process removed between listing and reading
                // was simply replayed by that process.
                if !path.exists() {
                    return ReplayResult::Discarded;
                }
                refuse_journal(path, &format!("unreadable journal ({reason})"));
                return ReplayResult::Refused;
            }
        };
        if journal.schema != OUTCOME_JOURNAL_SCHEMA || journal.operation_id != id {
            refuse_journal(
                path,
                &format!(
                    "schema {:?} for operation {} does not match file operation {id}",
                    journal.schema, journal.operation_id
                ),
            );
            return ReplayResult::Refused;
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
                refuse_journal(
                    path,
                    &format!(
                        "journaled status {:?} is not a final outcome",
                        journal.status
                    ),
                );
                return ReplayResult::Refused;
            }
        };
        let current = match self.store().coordinated_operation(id) {
            Ok(Some(current)) => current,
            Ok(None) => {
                refuse_journal(path, &format!("operation {id} does not exist"));
                return ReplayResult::Refused;
            }
            Err(error) => {
                crate::warn_unrecorded::<(), _>(
                    "read an operation with a pending outcome journal",
                    Err(error),
                );
                return ReplayResult::Pending;
            }
        };
        if current.session_id != journal.session_id
            || command_sha256(&current.command_json) != journal.command_sha256
        {
            refuse_journal(
                path,
                &format!(
                    "operation {id} belongs to session {} with a different command than the journal (session {})",
                    current.session_id, journal.session_id
                ),
            );
            return ReplayResult::Refused;
        }
        if !matches!(
            current.status,
            OperationStatus::Running | OperationStatus::OutcomeUnknown
        ) {
            remove_journal(path);
            return ReplayResult::Discarded;
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
            id,
            journal.session_id,
            &current.command_json,
            status,
            journal.exit_code,
            &details.to_string(),
            REPLAY_ATTEMPT_WAIT,
        ) {
            Ok(OperationSettlement::Recorded(_)) => {
                remove_journal(path);
                ReplayResult::Replayed
            }
            Ok(OperationSettlement::AlreadySettled(_)) => {
                remove_journal(path);
                ReplayResult::Discarded
            }
            Ok(OperationSettlement::Mismatch(changed)) => {
                refuse_journal(
                    path,
                    &format!(
                        "operation {id} changed while replaying (now session {})",
                        changed.session_id
                    ),
                );
                ReplayResult::Refused
            }
            Ok(OperationSettlement::Missing) => {
                refuse_journal(path, &format!("operation {id} disappeared while replaying"));
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
}

enum ReplayResult {
    Replayed,
    Discarded,
    Refused,
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

/// One `broker status` warning per journal that has stayed unreplayed for
/// more than ten minutes. A fresh journal is normal and silent: the next
/// writable open records it.
pub(crate) fn unreplayed_outcome_journal_advice(
    db_path: &Path,
    now_ms: i64,
) -> Vec<crate::StatusAdvice> {
    unreplayed_outcome_journals(db_path, now_ms)
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
        .collect()
}

#[cfg(test)]
mod tests;
