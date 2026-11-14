//! What a `submit prepare` is doing while it waits for, or holds, a
//! repository's shared preparation cache.
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
//!   to say what the holder is doing (and what a waiter waits on);
//! - one summary per operation for the prepare report and its command metric.
//!
//! The work reports through thread-local calls, like phase telemetry, so the
//! recovery and inventory code need no extra parameters, and every call is
//! free when no reporter is installed. The record is observational only: it
//! is never recovery or admission authority, and a missing, stale or
//! malformed record reads as "nothing reported". A record left behind by a
//! killed prepare is swept once it is stale, and a rerun of the same session
//! overwrites it.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

/// Directory, beside the host resource database, holding the records.
pub(crate) const DIRECTORY: &str = "preparation-cache-activity";
const SCHEMA: &str = "aethyme.preparation-cache-activity/v1";
/// How often a running operation repeats its stderr line and rewrites its
/// record.
pub(crate) const PROGRESS_INTERVAL: Duration = Duration::from_secs(15);
/// A record not rewritten for this long belongs to a holder that stopped
/// reporting (it exited, or one filesystem call is stalled); readers skip it
/// and sweeps remove it.
pub(crate) const STALE_AFTER_MS: i64 = 90_000;
const MAX_RECORD_BYTES: u64 = 16 * 1024;
/// Directory entries one read or sweep looks at. Sweeps keep the directory
/// near one record per running prepare, so this is only a runaway bound.
const MAX_SCANNED_ENTRIES: usize = 4096;
/// Fresh records one read parses, newest first.
const MAX_RECORDS: usize = 64;

/// The work a preparation does while it waits for or holds the shared cache.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreparationCacheOperation {
    /// Waiting for another preparation to release the shared cache.
    WaitingForCache,
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
            Self::WaitingForCache => "waiting_for_cache",
            Self::RotationRecovery => "rotation_recovery",
            Self::BudgetInventory => "budget_inventory",
            Self::RotationDrain => "rotation_drain",
            Self::PreparationStep => "preparation_step",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::WaitingForCache => "waiting for the shared cache",
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
            Self::WaitingForCache | Self::PreparationStep => "items",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::RotationRecovery | Self::RotationDrain => {
                "interrupting is safe: unlinked entries stay removed and the next prepare resumes"
            }
            Self::BudgetInventory => {
                "interrupting only skips this budget check; nothing has been removed"
            }
            Self::WaitingForCache | Self::PreparationStep => "",
        }
    }

    /// Whether this record describes a holder of the cache, not a waiter.
    pub(crate) fn holds_cache(self) -> bool {
        !matches!(self, Self::WaitingForCache)
    }
}

/// The live record a preparation rewrites while it waits or works. Readers
/// tolerate fields they do not know: brokers of mixed versions share a host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    /// When the cache lease was granted; null while still waiting for it.
    pub lease_acquired_at_ms: Option<i64>,
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
    /// Logical bytes observed, for the budget inventory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_observed: Option<u64>,
    /// How the operation ended, for the budget inventory: `absent`,
    /// `within_budget`, `over_budget`, `inconclusive_time_limit` or
    /// `inconclusive_entry_limit`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<&'static str>,
}

struct Open {
    record: PreparationCacheActivity,
    started: Instant,
    announce: bool,
    outcome: Option<&'static str>,
}

struct Reporter {
    directory: Option<PathBuf>,
    path: Option<PathBuf>,
    repository: String,
    session_id: i64,
    lease_acquired: Option<Instant>,
    lease_acquired_at_ms: Option<i64>,
    interval: Duration,
    next_emit: Instant,
    open: Option<Open>,
    summaries: Vec<PreparationCacheOperationSummary>,
    sink: Box<dyn FnMut(&str)>,
    wrote_record: bool,
    warned_write: bool,
}

thread_local! {
    static ACTIVE: RefCell<Option<Reporter>> = const { RefCell::new(None) };
}

/// Stops reporting when dropped: closes the open operation, removes the
/// record, sweeps stale ones and restores any reporter installed before.
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
            crate::warn_unrecorded(
                "remove the preparation-cache activity record",
                std::fs::remove_file(path),
            );
        }
        if let Some(directory) = reporter.directory.as_deref() {
            sweep_stale(directory, SystemTime::now());
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
/// summaries are produced. Call [`lease_acquired`] once the lease is granted.
pub(crate) fn install(
    resource_db_path: Option<&Path>,
    repository: &str,
    session_id: i64,
) -> ActivityGuard {
    install_with(
        resource_db_path,
        repository,
        session_id,
        PROGRESS_INTERVAL,
        // A closed stderr must never fail or abort a drain in progress.
        Box::new(|line| writeln!(std::io::stderr(), "{line}").unwrap_or(())),
    )
}

pub(crate) fn install_with(
    resource_db_path: Option<&Path>,
    repository: &str,
    session_id: i64,
    interval: Duration,
    sink: Box<dyn FnMut(&str)>,
) -> ActivityGuard {
    let now = Instant::now();
    let directory = resource_db_path.and_then(state_directory);
    if let Some(directory) = directory.as_deref() {
        sweep_stale(directory, SystemTime::now());
    }
    let path = directory
        .as_deref()
        .map(|directory| directory.join(record_name(repository, session_id)));
    let reporter = Reporter {
        directory,
        path,
        repository: repository.to_owned(),
        session_id,
        lease_acquired: None,
        lease_acquired_at_ms: None,
        interval,
        next_emit: now + interval,
        open: None,
        summaries: Vec::new(),
        sink,
        wrote_record: false,
        warned_write: false,
    };
    let previous = ACTIVE.with(|active| active.borrow_mut().replace(reporter));
    ActivityGuard {
        previous,
        restored: false,
    }
}

fn with_reporter<T>(apply: impl FnOnce(&mut Reporter) -> T) -> Option<T> {
    ACTIVE.with(|active| {
        let mut active = active.try_borrow_mut().ok()?;
        active.as_mut().map(apply)
    })
}

/// The cache lease was granted now.
pub(crate) fn lease_acquired() {
    with_reporter(|reporter| {
        reporter.lease_acquired = Some(Instant::now());
        reporter.lease_acquired_at_ms = Some(now_ms());
        if let Some(open) = reporter.open.as_mut() {
            open.record.lease_acquired_at_ms = reporter.lease_acquired_at_ms;
        }
    });
}

/// Start `operation`, closing the one before. An announced operation prints
/// one line per interval while it runs; an unannounced one (a wait, or a
/// repository step, which prints its own output) only keeps the record.
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

/// How the open operation ended (see [`PreparationCacheOperationSummary::outcome`]).
pub(crate) fn set_outcome(outcome: &'static str) {
    with_reporter(|reporter| {
        if let Some(open) = reporter.open.as_mut() {
            open.outcome = Some(outcome);
        }
    });
}

/// The most recent operation of this kind, open or closed, as a summary.
pub(crate) fn last_operation(
    operation: PreparationCacheOperation,
) -> Option<PreparationCacheOperationSummary> {
    with_reporter(|reporter| {
        if let Some(open) = reporter
            .open
            .as_ref()
            .filter(|open| open.record.operation == operation)
        {
            return Some(open.summary());
        }
        reporter
            .summaries
            .iter()
            .rev()
            .find(|summary| summary.operation == operation)
            .cloned()
    })
    .flatten()
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
            outcome: None,
        });
        // Write the record at once so other sessions see the new operation;
        // the first stderr line waits for the interval or `announce_now`.
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
                self.lease_acquired
                    .map(|acquired| now.saturating_duration_since(acquired)),
            );
            (self.sink)(&line);
        }
        if let Some(path) = self.path.as_deref() {
            match write_record(path, &open.record) {
                Ok(()) => self.wrote_record = true,
                // Diagnostics must never fail the preparation, and one
                // warning says it all.
                Err(error) if !self.warned_write => {
                    self.warned_write = true;
                    crate::warn_unrecorded(
                        "record preparation-cache activity (other sessions will not see it)",
                        Err::<(), _>(error),
                    );
                }
                Err(_) => {}
            }
        }
    }

    fn close(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        self.summaries.push(open.summary());
    }
}

impl Open {
    fn summary(&self) -> PreparationCacheOperationSummary {
        PreparationCacheOperationSummary {
            operation: self.record.operation,
            detail: self.record.detail.clone(),
            elapsed_ms: self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            items_done: self.record.items_done,
            bytes_observed: self.record.bytes_observed,
            outcome: self.outcome,
        }
    }
}

fn progress_line(
    record: &PreparationCacheActivity,
    operation_elapsed: Duration,
    lease_elapsed: Option<Duration>,
) -> String {
    let lease = lease_elapsed.map_or_else(
        || "the cache lease is not held yet".to_owned(),
        |elapsed| {
            format!(
                "lease held {}s, other prepares of this repository wait",
                elapsed.as_secs()
            )
        },
    );
    let mut line = format!(
        "preparation cache: {}{} for {}s: {}; {lease}",
        record.operation.describe(),
        record
            .detail
            .as_deref()
            .map_or_else(String::new, |detail| format!(" {detail:?}")),
        operation_elapsed.as_secs(),
        progress_text(record),
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
    let held = record.lease_acquired_at_ms.map_or_else(
        || "is not holding the cache yet".to_owned(),
        |acquired| format!("has held the cache {}s", elapsed(acquired)),
    );
    format!(
        "the holder, session {} (pid {}), is {}{} for {}s ({}) and {held}",
        record.session_id,
        record.holder_pid,
        record.operation.describe(),
        record
            .detail
            .as_deref()
            .map_or_else(String::new, |detail| format!(" {detail:?}")),
        elapsed(record.operation_started_at_ms),
        progress_text(record),
    )
}

fn state_directory(resource_db_path: &Path) -> Option<PathBuf> {
    resource_db_path
        .parent()
        .map(|parent| parent.join(DIRECTORY))
}

/// One record per repository and session, so a rerun after a killed prepare
/// overwrites the record it left instead of adding another.
fn record_name(repository: &str, session_id: i64) -> String {
    let repository: String = repository
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(16)
        .collect();
    format!("{repository}-{session_id}.json")
}

fn write_record(path: &Path, record: &PreparationCacheActivity) -> std::io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| std::io::Error::other("activity record has no directory"))?;
    std::fs::create_dir_all(directory)?;
    let bytes = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("record"),
        std::process::id()
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

/// Record files with their modification time, at most [`MAX_SCANNED_ENTRIES`].
fn record_files(directory: &Path) -> Vec<(PathBuf, SystemTime, bool)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .take(MAX_SCANNED_ENTRIES)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let temporary = name.starts_with('.') && name.ends_with(".tmp");
            if !temporary && (name.starts_with('.') || !name.ends_with(".json")) {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            Some((entry.path(), metadata.modified().ok()?, temporary))
        })
        .collect()
}

fn older_than_stale(modified: SystemTime, now: SystemTime) -> bool {
    now.duration_since(modified)
        .is_ok_and(|age| age.as_millis() > STALE_AFTER_MS as u128)
}

/// Remove records (and partial writes) nobody has rewritten for longer than
/// the stale threshold: what a killed or timed-out prepare left behind.
fn sweep_stale(directory: &Path, now: SystemTime) {
    for (path, modified, _) in record_files(directory) {
        if older_than_stale(modified, now)
            && let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::warn_unrecorded(
                "remove a stale preparation-cache activity record",
                Err::<(), _>(error),
            );
        }
    }
}

/// Fresh records, optionally for one repository and one session, newest
/// first. Read-only and bounded: freshness is judged before any cap, so stale
/// records can never hide a live one; it never creates state.
pub(crate) fn read_current(
    resource_db_path: &Path,
    repository: Option<&str>,
    session_id: Option<i64>,
    now_ms: i64,
) -> Vec<PreparationCacheActivity> {
    let Some(directory) = state_directory(resource_db_path) else {
        return Vec::new();
    };
    let now = SystemTime::now();
    let mut files: Vec<_> = record_files(&directory)
        .into_iter()
        .filter(|(_, modified, temporary)| !temporary && !older_than_stale(*modified, now))
        .collect();
    files.sort_by_key(|(_, modified, _)| std::cmp::Reverse(*modified));
    let mut records: Vec<_> = files
        .into_iter()
        .take(MAX_RECORDS)
        .filter_map(|(path, _, _)| read_record(&path).ok())
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
    SystemTime::now()
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

    fn age(path: &Path, by: Duration) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - by).unwrap();
    }

    #[test]
    fn an_announced_operation_prints_when_it_starts_and_at_each_interval() {
        let (lines, sink) = capture();
        let guard = install_with(None, "repo", 7, Duration::ZERO, sink);
        lease_acquired();
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
        assert!(
            lines[0].starts_with(
                "preparation cache: removing the rotated shared cache for 0s: 0 entries unlinked"
            ),
            "{}",
            lines[0]
        );
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
        let guard = install_with(None, "repo", 7, Duration::from_secs(3600), sink);
        enter(PreparationCacheOperation::BudgetInventory, None, true);
        for done in 0..1000 {
            observe(done, Some(done * 10));
            tick();
        }
        announce_now();
        drop(guard);
        let lines = lines.borrow();
        assert_eq!(lines.len(), 1, "only the announced line: {lines:?}");
        assert!(lines[0].contains("not held yet"), "{}", lines[0]);
        assert!(
            lines[0].contains("nothing has been removed"),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn an_unannounced_step_keeps_the_record_without_printing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("host-resources.sqlite3");
        let (lines, sink) = capture();
        let guard = install_with(Some(&db), "abc123", 9, Duration::ZERO, sink);
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
        let guard = install_with(Some(&db), "repo-a", 3, Duration::ZERO, Box::new(|_| {}));
        lease_acquired();
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
        assert!(line.contains("has held the cache"), "{line}");
        drop(guard);
    }

    #[test]
    fn leftover_records_never_hide_the_live_one_and_are_swept() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("host-resources.sqlite3");
        let directory = state_directory(&db).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        // What killed prepares leave behind: many records that sort before
        // the live one by name and were last written long ago.
        let mut dead = Vec::new();
        for session in 0..200 {
            let path = directory.join(format!("aaaa-{session}.json"));
            std::fs::write(&path, b"{}").unwrap();
            age(&path, Duration::from_secs(3600));
            dead.push(path);
        }
        let partial = directory.join(".aaaa-1.json.99.tmp");
        std::fs::write(&partial, b"{").unwrap();
        age(&partial, Duration::from_secs(3600));

        let guard = install_with(Some(&db), "zzzz", 5, Duration::ZERO, Box::new(|_| {}));
        assert!(
            dead.iter().all(|path| !path.exists()) && !partial.exists(),
            "installing a reporter sweeps stale records"
        );
        // Records that arrive later, still old, are skipped before any cap.
        for path in &dead {
            std::fs::write(path, b"{}").unwrap();
            age(path, Duration::from_secs(3600));
        }
        enter(PreparationCacheOperation::RotationDrain, None, true);
        let current = read_current(&db, Some("zzzz"), None, now_ms());
        assert_eq!(current.len(), 1, "{current:?}");
        assert_eq!(current[0].session_id, 5);
        drop(guard);
        assert!(
            dead.iter().all(|path| !path.exists()),
            "finishing sweeps stale records too"
        );
    }

    #[test]
    fn a_rerun_of_the_same_session_overwrites_its_record() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("host-resources.sqlite3");
        assert_eq!(record_name("abcdef", 4), record_name("abcdef", 4));
        let guard = install_with(Some(&db), "abcdef", 4, Duration::ZERO, Box::new(|_| {}));
        enter(PreparationCacheOperation::BudgetInventory, None, true);
        // Leave the record behind as a killed prepare would.
        std::mem::forget(guard);
        ACTIVE.with(|active| active.borrow_mut().take());
        let rerun = install_with(Some(&db), "abcdef", 4, Duration::ZERO, Box::new(|_| {}));
        enter(PreparationCacheOperation::RotationDrain, None, true);
        let files: Vec<_> = record_files(&state_directory(&db).unwrap())
            .into_iter()
            .filter(|(_, _, temporary)| !temporary)
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        let current = read_current(&db, Some("abcdef"), Some(4), now_ms());
        assert_eq!(
            current[0].operation,
            PreparationCacheOperation::RotationDrain
        );
        drop(rerun);
    }

    #[test]
    fn records_from_a_newer_broker_with_extra_fields_still_read() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("host-resources.sqlite3");
        let directory = state_directory(&db).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let mut value = serde_json::to_value(PreparationCacheActivity {
            schema: SCHEMA.into(),
            repository: "repo".into(),
            session_id: 2,
            holder_pid: 1,
            operation: PreparationCacheOperation::PreparationStep,
            detail: Some("deps".into()),
            items_done: 0,
            items_total: None,
            items_total_is_lower_bound: false,
            bytes_observed: None,
            lease_acquired_at_ms: Some(now_ms()),
            operation_started_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        })
        .unwrap();
        value["added_later"] = serde_json::json!(true);
        std::fs::write(
            directory.join("repo-2.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert_eq!(read_current(&db, Some("repo"), Some(2), now_ms()).len(), 1);
    }

    #[test]
    fn a_failed_record_write_warns_once_and_keeps_reporting() {
        let tmp = tempfile::tempdir().unwrap();
        // The record directory is a file, so every write fails.
        let db = tmp.path().join("host-resources.sqlite3");
        std::fs::write(state_directory(&db).unwrap(), b"not a directory").unwrap();
        let (lines, sink) = capture();
        let guard = install_with(Some(&db), "repo", 1, Duration::ZERO, sink);
        enter(PreparationCacheOperation::RotationDrain, None, true);
        for _ in 0..5 {
            tick();
        }
        let warned = ACTIVE.with(|active| {
            active
                .borrow()
                .as_ref()
                .is_some_and(|reporter| reporter.warned_write && !reporter.wrote_record)
        });
        assert!(warned, "the write failure is noted once, not per tick");
        drop(guard);
        assert_eq!(lines.borrow().len(), 5, "stderr progress continues");
    }

    #[test]
    fn calls_without_a_reporter_do_nothing() {
        enter(PreparationCacheOperation::RotationDrain, None, true);
        observe(1, None);
        set_outcome("within_budget");
        tick();
        leave();
        assert_eq!(
            last_operation(PreparationCacheOperation::BudgetInventory),
            None
        );
    }

    #[test]
    fn each_operation_is_summarized_in_order_with_its_outcome() {
        let guard = install_with(None, "repo", 1, Duration::from_secs(3600), Box::new(|_| {}));
        enter(PreparationCacheOperation::WaitingForCache, None, false);
        lease_acquired();
        enter(PreparationCacheOperation::RotationRecovery, None, true);
        enter(PreparationCacheOperation::BudgetInventory, None, true);
        observe(5, Some(50));
        set_outcome("inconclusive_time_limit");
        leave();
        assert_eq!(
            last_operation(PreparationCacheOperation::BudgetInventory)
                .map(|inventory| inventory.items_done),
            Some(5)
        );
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
                PreparationCacheOperation::WaitingForCache,
                PreparationCacheOperation::RotationRecovery,
                PreparationCacheOperation::BudgetInventory,
                PreparationCacheOperation::PreparationStep,
            ]
        );
        assert_eq!(summaries[2].items_done, 5);
        assert_eq!(summaries[2].bytes_observed, Some(50));
        assert_eq!(summaries[2].outcome, Some("inconclusive_time_limit"));
        assert!(!PreparationCacheOperation::WaitingForCache.holds_cache());
        assert!(PreparationCacheOperation::RotationDrain.holds_cache());
    }
}
