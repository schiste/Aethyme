//! What a `submit prepare` is doing while it holds a repository's shared
//! preparation cache.
//!
//! Recovery of an unfinished rotation, the budget inventory and a rotation's
//! drain all run under the host-wide cache lease before any repository
//! command starts, and until 2026-11-14 they printed nothing. Measured in the
//! busiest repository, a drain that unlinked about 141,000 entries held the
//! lease for up to six minutes in silence while every other session's
//! prepare waited, and nobody could tell progress from a hang. This module
//! makes that work observable from one place:
//!
//! - a stderr line when an announced operation starts and then at a fixed
//!   interval while it runs (operation, items done of total, bytes, elapsed);
//! - a small JSON record beside the host resource database, rewritten at the
//!   same interval, which `submit prepare status` and a waiting prepare read
//!   to say what the holder is doing;
//! - one summary per operation for the prepare report and its command metric.
//!
//! The work reports through thread-local calls, like phase telemetry, so the
//! recovery and inventory code need no extra parameters, and every call is
//! free when no reporter is installed. The record is observational only: it
//! is never recovery or admission authority, and a missing, stale or
//! malformed record reads as "nothing reported".

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Directory, beside the host resource database, holding the records.
pub(crate) const DIRECTORY: &str = "preparation-cache-activity";
const SCHEMA: &str = "aethyme.preparation-cache-activity/v1";
/// How often a running operation repeats its stderr line and rewrites its
/// record.
pub(crate) const PROGRESS_INTERVAL: Duration = Duration::from_secs(15);
/// A record not rewritten for this long belongs to a holder that stopped
/// reporting (it exited, or one filesystem call is stalled); readers skip it.
pub(crate) const STALE_AFTER_MS: i64 = 90_000;
const MAX_RECORD_BYTES: u64 = 16 * 1024;
const MAX_SCANNED_ENTRIES: usize = 512;
const MAX_RECORDS: usize = 64;

/// The work a preparation does while it holds the shared cache.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreparationCacheOperation {
    /// Finishing a rotation that an earlier preparation left unfinished.
    RotationRecovery,
    /// Measuring the repository's shared store against `[shared_cache] max_bytes`.
    BudgetInventory,
    /// Removing the store this preparation has just rotated away.
    RotationDrain,
    /// Running one repository-declared preparation step.
    PreparationStep,
}

impl PreparationCacheOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RotationRecovery => "rotation_recovery",
            Self::BudgetInventory => "budget_inventory",
            Self::RotationDrain => "rotation_drain",
            Self::PreparationStep => "preparation_step",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::RotationRecovery => "finishing an interrupted shared-cache rotation",
            Self::BudgetInventory => "measuring the shared cache against its budget",
            Self::RotationDrain => "removing the rotated shared cache",
            Self::PreparationStep => "running preparation step",
        }
    }

    fn items(self) -> &'static str {
        match self {
            Self::RotationRecovery | Self::RotationDrain => "entries unlinked",
            Self::BudgetInventory => "entries measured",
            Self::PreparationStep => "items",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::RotationRecovery | Self::RotationDrain => {
                "interrupting is safe: unlinked entries stay removed and the next prepare resumes"
            }
            Self::BudgetInventory => "the measurement stops at its time limit",
            Self::PreparationStep => "",
        }
    }
}

/// The live record a holder rewrites while it works.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreparationCacheActivity {
    pub schema: String,
    /// Repository identity: the store's directory name under the cache root.
    pub repository: String,
    pub session_id: i64,
    pub holder_pid: u32,
    pub operation: PreparationCacheOperation,
    /// The step name for `preparation_step`; null otherwise.
    pub detail: Option<String>,
    pub items_done: u64,
    /// What the operation expects to process; null when unknown.
    pub items_total: Option<u64>,
    /// True when `items_total` is a lower bound (its count stopped at a limit).
    pub items_total_is_lower_bound: bool,
    /// Logical bytes observed so far, for the budget inventory.
    pub bytes_observed: Option<u64>,
    pub lease_acquired_at_ms: i64,
    pub operation_started_at_ms: i64,
    pub updated_at_ms: i64,
}

/// How long one operation ran and how much it did, for the report and the
/// command metric.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreparationCacheOperationSummary {
    pub operation: PreparationCacheOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub elapsed_ms: u64,
    pub items_done: u64,
}

struct Open {
    record: PreparationCacheActivity,
    started: Instant,
    announce: bool,
}

struct Reporter {
    path: Option<PathBuf>,
    repository: String,
    session_id: i64,
    lease_acquired: Instant,
    lease_acquired_at_ms: i64,
    interval: Duration,
    next_emit: Instant,
    open: Option<Open>,
    summaries: Vec<PreparationCacheOperationSummary>,
    sink: Box<dyn FnMut(&str)>,
    wrote_record: bool,
}

thread_local! {
    static ACTIVE: RefCell<Option<Reporter>> = const { RefCell::new(None) };
}

/// Stops reporting when dropped: closes the open operation, removes the
/// record and restores any reporter that was installed before.
pub(crate) struct ActivityGuard {
    previous: Option<Reporter>,
    restored: bool,
}

impl ActivityGuard {
    /// Stop reporting and return one summary per operation, in order.
    pub(crate) fn finish(mut self) -> Vec<PreparationCacheOperationSummary> {
        self.restore()
    }

    fn restore(&mut self) -> Vec<PreparationCacheOperationSummary> {
        if self.restored {
            return Vec::new();
        }
        self.restored = true;
        let previous = self.previous.take();
        let reporter = ACTIVE.with(|active| std::mem::replace(&mut *active.borrow_mut(), previous));
        let Some(mut reporter) = reporter else {
            return Vec::new();
        };
        reporter.close();
        if reporter.wrote_record
            && let Some(path) = reporter.path.as_deref()
        {
            // The holder is done; a leftover record would only age out.
            crate::warn_unrecorded(
                "remove the preparation-cache activity record",
                std::fs::remove_file(path),
            );
        }
        reporter.summaries
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Report this thread's cache work for one preparation until the guard drops.
/// `resource_db_path` places the record; without it only stderr and the
/// summaries are produced.
pub(crate) fn install(
    resource_db_path: Option<&Path>,
    repository: &str,
    session_id: i64,
    lease_acquired_at_ms: i64,
) -> ActivityGuard {
    install_with(
        resource_db_path,
        repository,
        session_id,
        lease_acquired_at_ms,
        PROGRESS_INTERVAL,
        Box::new(|line| eprintln!("{line}")),
    )
}

pub(crate) fn install_with(
    resource_db_path: Option<&Path>,
    repository: &str,
    session_id: i64,
    lease_acquired_at_ms: i64,
    interval: Duration,
    sink: Box<dyn FnMut(&str)>,
) -> ActivityGuard {
    let now = Instant::now();
    let path = resource_db_path
        .and_then(state_directory)
        .map(|directory| directory.join(record_name(repository, session_id)));
    let reporter = Reporter {
        path,
        repository: repository.to_owned(),
        session_id,
        lease_acquired: now,
        lease_acquired_at_ms,
        interval,
        next_emit: now + interval,
        open: None,
        summaries: Vec::new(),
        sink,
        wrote_record: false,
    };
    let previous = ACTIVE.with(|active| active.borrow_mut().replace(reporter));
    ActivityGuard {
        previous,
        restored: false,
    }
}

fn with_reporter(apply: impl FnOnce(&mut Reporter)) {
    ACTIVE.with(|active| {
        if let Ok(mut active) = active.try_borrow_mut()
            && let Some(reporter) = active.as_mut()
        {
            apply(reporter);
        }
    });
}

/// Start `operation`, closing the one before. An announced operation prints
/// one line per interval while it runs; an unannounced one (a repository
/// step, which prints its own output) only keeps the record.
pub(crate) fn enter(operation: PreparationCacheOperation, detail: Option<String>, announce: bool) {
    with_reporter(|reporter| reporter.enter(operation, detail, announce));
}

/// Print the open operation's line now and announce it from here on: for
/// work known to be long as soon as it is found, such as draining a retired
/// store, so the first line does not wait for the interval.
pub(crate) fn announce_now() {
    with_reporter(|reporter| {
        if let Some(open) = reporter.open.as_mut() {
            open.announce = true;
            reporter.emit(Instant::now(), true);
        }
    });
}

/// What the open operation expects to process.
pub(crate) fn set_total(total: u64, lower_bound: bool) {
    with_reporter(|reporter| {
        if let Some(open) = reporter.open.as_mut() {
            open.record.items_total = Some(total);
            open.record.items_total_is_lower_bound = lower_bound;
        }
    });
}

/// The open operation's progress so far.
pub(crate) fn observe(items_done: u64, bytes_observed: Option<u64>) {
    with_reporter(|reporter| {
        if let Some(open) = reporter.open.as_mut() {
            open.record.items_done = items_done;
            if bytes_observed.is_some() {
                open.record.bytes_observed = bytes_observed;
            }
        }
    });
}

/// Print and rewrite the record when the interval has passed. Cheap enough
/// to call before every filesystem mutation.
pub(crate) fn tick() {
    with_reporter(|reporter| reporter.tick(Instant::now()));
}

/// Close the open operation.
pub(crate) fn leave() {
    with_reporter(Reporter::close);
}

impl Reporter {
    fn enter(
        &mut self,
        operation: PreparationCacheOperation,
        detail: Option<String>,
        announce: bool,
    ) {
        self.close();
        let now_ms = now_ms();
        self.open = Some(Open {
            record: PreparationCacheActivity {
                schema: SCHEMA.into(),
                repository: self.repository.clone(),
                session_id: self.session_id,
                holder_pid: std::process::id(),
                operation,
                detail,
                items_done: 0,
                items_total: None,
                items_total_is_lower_bound: false,
                bytes_observed: None,
                lease_acquired_at_ms: self.lease_acquired_at_ms,
                operation_started_at_ms: now_ms,
                updated_at_ms: now_ms,
            },
            started: Instant::now(),
            announce,
        });
        // Write the record at once so waiters see the new operation; the
        // first stderr line waits for the interval or `announce_now`.
        self.emit(Instant::now(), false);
    }

    fn tick(&mut self, now: Instant) {
        if now < self.next_emit {
            return;
        }
        let announce = self.open.as_ref().is_some_and(|open| open.announce);
        self.emit(now, announce);
    }

    fn emit(&mut self, now: Instant, print: bool) {
        self.next_emit = now + self.interval;
        let Some(open) = self.open.as_mut() else {
            return;
        };
        open.record.updated_at_ms = now_ms();
        if print {
            let line = progress_line(
                &open.record,
                now.saturating_duration_since(open.started),
                now.saturating_duration_since(self.lease_acquired),
            );
            (self.sink)(&line);
        }
        if let Some(path) = self.path.as_deref() {
            // Diagnostics must never fail the preparation.
            let written = write_record(path, &open.record);
            self.wrote_record |= written.is_ok();
            crate::warn_unrecorded("record preparation-cache activity", written);
        }
    }

    fn close(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        self.summaries.push(PreparationCacheOperationSummary {
            operation: open.record.operation,
            detail: open.record.detail,
            elapsed_ms: open.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            items_done: open.record.items_done,
        });
    }
}

fn progress_line(
    record: &PreparationCacheActivity,
    operation_elapsed: Duration,
    lease_elapsed: Duration,
) -> String {
    let mut line = format!(
        "preparation cache: {}{} for {}s: {}; lease held {}s, other prepares of this repository wait",
        record.operation.describe(),
        record
            .detail
            .as_deref()
            .map_or_else(String::new, |detail| format!(" {detail:?}")),
        operation_elapsed.as_secs(),
        progress_text(record),
        lease_elapsed.as_secs(),
    );
    let hint = record.operation.hint();
    if !hint.is_empty() {
        line.push_str("; ");
        line.push_str(hint);
    }
    line
}

fn progress_text(record: &PreparationCacheActivity) -> String {
    let mut text = match record.items_total {
        Some(total) if record.items_total_is_lower_bound => format!(
            "{} of at least {total} {}",
            record.items_done,
            record.operation.items()
        ),
        Some(total) => format!(
            "{} of {total} {}",
            record.items_done,
            record.operation.items()
        ),
        None => format!("{} {}", record.items_done, record.operation.items()),
    };
    if let Some(bytes) = record.bytes_observed {
        text.push_str(&format!(", {bytes} bytes"));
    }
    text
}

/// One line naming what a holder is doing, for a waiting prepare.
pub(crate) fn describe_holder(record: &PreparationCacheActivity, now_ms: i64) -> String {
    let elapsed = |since: i64| now_ms.saturating_sub(since).max(0) / 1000;
    format!(
        "the holder, session {} (pid {}), is {}{} for {}s ({}) and has held the cache {}s",
        record.session_id,
        record.holder_pid,
        record.operation.describe(),
        record
            .detail
            .as_deref()
            .map_or_else(String::new, |detail| format!(" {detail:?}")),
        elapsed(record.operation_started_at_ms),
        progress_text(record),
        elapsed(record.lease_acquired_at_ms),
    )
}

fn state_directory(resource_db_path: &Path) -> Option<PathBuf> {
    resource_db_path
        .parent()
        .map(|parent| parent.join(DIRECTORY))
}

fn record_name(repository: &str, session_id: i64) -> String {
    let repository: String = repository
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(16)
        .collect();
    format!("{repository}-{session_id}-{}.json", std::process::id())
}

fn write_record(path: &Path, record: &PreparationCacheActivity) -> std::io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| std::io::Error::other("activity record has no directory"))?;
    std::fs::create_dir_all(directory)?;
    let bytes = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    let temporary = directory.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("record")
    ));
    let written = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        std::fs::rename(&temporary, path)
    })();
    if written.is_err() {
        crate::warn_unrecorded(
            "remove a partial preparation-cache activity record",
            std::fs::remove_file(&temporary),
        );
    }
    written
}

fn read_record(path: &Path) -> std::io::Result<PreparationCacheActivity> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "activity record is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let record: PreparationCacheActivity = serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if record.schema != SCHEMA {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "activity record has an unknown schema",
        ));
    }
    Ok(record)
}

/// Fresh records, optionally for one repository and one session, newest
/// first. Read-only and bounded: opens only the record directory and its
/// small files, never creates state, and skips stale or malformed records.
pub(crate) fn read_current(
    resource_db_path: &Path,
    repository: Option<&str>,
    session_id: Option<i64>,
    now_ms: i64,
) -> Vec<PreparationCacheActivity> {
    let Some(directory) = state_directory(resource_db_path) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut names: Vec<_> = entries
        .take(MAX_SCANNED_ENTRIES)
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| {
            name.to_str()
                .is_some_and(|name| !name.starts_with('.') && name.ends_with(".json"))
        })
        .collect();
    names.sort();
    let mut records: Vec<_> = names
        .into_iter()
        .take(MAX_RECORDS)
        .filter_map(|name| read_record(&directory.join(name)).ok())
        .filter(|record| {
            repository.is_none_or(|repository| record.repository == repository)
                && session_id.is_none_or(|session| record.session_id == session)
                && now_ms.saturating_sub(record.updated_at_ms) <= STALE_AFTER_MS
        })
        .collect();
    records.sort_by_key(|record| std::cmp::Reverse(record.updated_at_ms));
    records
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            elapsed.as_millis().min(i64::MAX as u128) as i64
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    type Lines = Rc<RefCell<Vec<String>>>;
    type Sink = Box<dyn FnMut(&str)>;

    fn capture() -> (Lines, Sink) {
        let lines = Rc::new(RefCell::new(Vec::new()));
        let sink_lines = Rc::clone(&lines);
        (
            lines,
            Box::new(move |line: &str| sink_lines.borrow_mut().push(line.to_owned())),
        )
    }

    #[test]
    fn an_announced_operation_prints_when_it_starts_and_at_each_interval() {
        let (lines, sink) = capture();
        let guard = install_with(None, "repo", 7, now_ms(), Duration::ZERO, sink);
        enter(PreparationCacheOperation::RotationDrain, None, true);
        announce_now();
        set_total(141_558, true);
        observe(61_234, None);
        tick();
        observe(90_000, None);
        tick();
        let summaries = guard.finish();
        let lines = lines.borrow();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].starts_with(
            "preparation cache: removing the rotated shared cache for 0s: 0 entries unlinked"
        ));
        assert!(
            lines[1].contains("61234 of at least 141558 entries unlinked"),
            "{}",
            lines[1]
        );
        assert!(
            lines[2].contains("90000 of at least 141558"),
            "{}",
            lines[2]
        );
        assert!(lines[2].contains("lease held"));
        assert!(lines[2].contains("the next prepare resumes"));
        assert_eq!(summaries.len(), 1);
        assert_eq!(
            summaries[0].operation,
            PreparationCacheOperation::RotationDrain
        );
        assert_eq!(summaries[0].items_done, 90_000);
    }

    #[test]
    fn ticks_before_the_interval_print_nothing_more() {
        let (lines, sink) = capture();
        let guard = install_with(None, "repo", 7, now_ms(), Duration::from_secs(3600), sink);
        enter(PreparationCacheOperation::BudgetInventory, None, true);
        for done in 0..1000 {
            observe(done, Some(done * 10));
            tick();
        }
        announce_now();
        drop(guard);
        assert_eq!(
            lines.borrow().len(),
            1,
            "only the announced line: {:?}",
            lines.borrow()
        );
    }

    #[test]
    fn an_unannounced_step_keeps_the_record_without_printing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("host-resources.sqlite3");
        let (lines, sink) = capture();
        let guard = install_with(Some(&db), "abc123", 9, now_ms(), Duration::ZERO, sink);
        enter(
            PreparationCacheOperation::PreparationStep,
            Some("javascript-dependencies".into()),
            false,
        );
        tick();
        assert!(lines.borrow().is_empty());
        let current = read_current(&db, Some("abc123"), Some(9), now_ms());
        assert_eq!(current.len(), 1);
        assert_eq!(
            current[0].operation,
            PreparationCacheOperation::PreparationStep
        );
        assert_eq!(
            current[0].detail.as_deref(),
            Some("javascript-dependencies")
        );
        let summaries = guard.finish();
        assert_eq!(
            summaries[0].detail.as_deref(),
            Some("javascript-dependencies")
        );
        assert!(
            read_current(&db, Some("abc123"), Some(9), now_ms()).is_empty(),
            "the record is removed when the holder is done"
        );
    }

    #[test]
    fn readers_see_only_fresh_records_of_their_repository_and_session() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("host-resources.sqlite3");
        let guard = install_with(
            Some(&db),
            "repo-a",
            3,
            now_ms(),
            Duration::ZERO,
            Box::new(|_| {}),
        );
        enter(PreparationCacheOperation::RotationRecovery, None, true);
        observe(12, None);
        tick();
        let now = now_ms();
        assert_eq!(read_current(&db, Some("repo-a"), None, now).len(), 1);
        assert_eq!(read_current(&db, Some("repo-a"), Some(3), now).len(), 1);
        assert!(read_current(&db, Some("repo-a"), Some(4), now).is_empty());
        assert!(read_current(&db, Some("repo-b"), None, now).is_empty());
        assert!(
            read_current(&db, Some("repo-a"), None, now + STALE_AFTER_MS + 1).is_empty(),
            "a record the holder stopped rewriting is not reported"
        );
        let record = &read_current(&db, Some("repo-a"), None, now)[0];
        let line = describe_holder(record, now);
        assert!(line.contains("session 3"), "{line}");
        assert!(
            line.contains("finishing an interrupted shared-cache rotation"),
            "{line}"
        );
        assert!(line.contains("12 entries unlinked"), "{line}");
        drop(guard);
    }

    #[test]
    fn calls_without_a_reporter_do_nothing() {
        enter(PreparationCacheOperation::RotationDrain, None, true);
        observe(1, None);
        tick();
        leave();
    }

    #[test]
    fn each_operation_is_summarized_in_order() {
        let guard = install_with(
            None,
            "repo",
            1,
            now_ms(),
            Duration::from_secs(3600),
            Box::new(|_| {}),
        );
        enter(PreparationCacheOperation::RotationRecovery, None, true);
        enter(PreparationCacheOperation::BudgetInventory, None, true);
        observe(5, Some(50));
        leave();
        enter(
            PreparationCacheOperation::PreparationStep,
            Some("a".into()),
            false,
        );
        let summaries = guard.finish();
        let operations: Vec<_> = summaries.iter().map(|summary| summary.operation).collect();
        assert_eq!(
            operations,
            [
                PreparationCacheOperation::RotationRecovery,
                PreparationCacheOperation::BudgetInventory,
                PreparationCacheOperation::PreparationStep,
            ]
        );
        assert_eq!(summaries[1].items_done, 5);
    }
}
