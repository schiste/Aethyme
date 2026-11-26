//! The command-metrics log trim, and the append protocol it relies on.
//!
//! `.aethyme/logs/command-metrics.jsonl` is append-only, and its retention
//! rule is a pure function of each row's `ts` and the plan's cutoff. A plan
//! records the file's hashes as it saw them, but every broker command appends
//! a row, so those hashes are stale within seconds. Apply therefore re-reads
//! whatever the file holds now and drops only rows older than the reviewed
//! cutoff: rows appended since the plan are newer than the cutoff and are kept
//! byte for byte, as are malformed or timestamp-free lines.
//!
//! Writers take [`open_command_metrics_lock`](crate::file_lock::open_command_metrics_lock)
//! shared around each append; the trim holds it exclusively from its read
//! until after its rename. Once every writer takes the lock, no row can land
//! in the inode being replaced. A writer that does not take it (an older
//! binary, or one that gave up waiting) is narrowed, not excluded: an append
//! seen before the rename restarts the attempt, and the replaced inode is
//! watched for a short settle period after the rename, with anything that
//! lands there copied into the new file. A row such a writer writes after the
//! settle period, through a descriptor it opened before the rename, is still
//! lost. When every attempt sees an append, the trim is skipped. It is
//! housekeeping; losing a row inside retention, or failing a journal over it,
//! would cost more.
//!
//! The checkout retirement worker also applies the policy unattended, at most
//! daily (`Broker::trim_command_metrics_log_if_due`). It uses
//! the same lock, row rule, temporary naming, version checks and post-rename
//! tail copy, but stages first: the expensive pass over the old rows runs
//! without the lock into the trim's temporary, and only the bytes appended
//! since (a tail of seconds) are read, filtered and appended to it under the
//! exclusive lock before the rename. On the busiest repository the log was
//! about 500 MB; reading and filtering that while holding the lock would keep
//! every writer waiting past its 2 s allowance, and each one's unlocked
//! fallback append would restart the attempt until the trim gave up. Staging
//! relies on the log being append-only between trims: the locked phase
//! proceeds only on the same inode, at least as long as what was staged, and
//! any trim replaces the inode.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::file_lock::open_command_metrics_lock;

use super::{
    metric_row_timestamp, open_regular_file_nonblocking, retained_metric_bytes_until,
    same_regular_file_version,
};

/// Whole attempts before an apply gives up on a log that keeps growing.
pub(super) const TRIM_MAX_ATTEMPTS: u32 = 3;
/// How long the replaced inode is watched after the rename for rows an
/// unlocked writer appends there.
pub(super) const TAIL_SETTLE: Duration = Duration::from_millis(250);
/// How long the trim waits for writers that hold the append lock. Each holds
/// it for one `write`, so this is only reached by a stuck writer.
const TRIM_LOCK_WAIT: Duration = Duration::from_secs(2);
/// How long a writer waits for a trim to finish before appending unlocked.
const APPEND_LOCK_WAIT: Duration = Duration::from_secs(2);
const LOCK_POLL: Duration = Duration::from_millis(10);
/// The name `atomic_file::with_synced_temporary` gives the trim's temporary:
/// this prefix and six random alphanumeric characters.
const TEMPORARY_PREFIX: &str = ".command-metrics.jsonl-";
const TEMPORARY_RANDOM_LEN: usize = 6;

/// What one apply-time trim did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MetricsTrim {
    /// Expired rows were removed. `recovered_tail_bytes` were appended to the
    /// replaced file after its read and were copied into the new one.
    Trimmed {
        bytes_before: u64,
        bytes_after: u64,
        attempts: u32,
        recovered_tail_bytes: u64,
        stale_temporaries_removed: usize,
    },
    /// No row is older than the cutoff any more; the file was not replaced.
    NothingExpired {
        attempts: u32,
        stale_temporaries_removed: usize,
    },
    /// The log does not exist, so nothing is retained past its cutoff.
    Missing,
    /// Every attempt saw a concurrent append or a busy lock. Nothing was
    /// removed and nothing was lost; the next plan proposes the trim again.
    SkippedConcurrentAppend { attempts: u32 },
    /// The append lock could not be opened or taken, so nothing was trimmed.
    LockUnavailable(String),
    /// The path is not a regular file this trim may replace.
    Refused(String),
}

/// Points where a test can act as a concurrent writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrimPoint {
    /// The trimmed bytes are synced to a temporary; the rename is next.
    BeforePublish { attempt: u32 },
    /// The rename happened; the replaced inode has not been re-checked.
    AfterPublish { attempt: u32 },
}

/// Drop rows with `ts < cutoff` from the log at `path`, keeping everything
/// else in its current order.
pub(super) fn trim_expired_rows(
    main_root: &Path,
    path: &Path,
    cutoff: i64,
) -> io::Result<MetricsTrim> {
    trim_expired_rows_with(
        main_root,
        path,
        cutoff,
        TRIM_MAX_ATTEMPTS,
        TAIL_SETTLE,
        &mut |_| {},
    )
}

pub(super) fn trim_expired_rows_with(
    main_root: &Path,
    path: &Path,
    cutoff: i64,
    max_attempts: u32,
    settle: Duration,
    at: &mut dyn FnMut(TrimPoint),
) -> io::Result<MetricsTrim> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Ok(MetricsTrim::Refused(
                "the command metrics log is a symlink and is never rewritten by GC".into(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(MetricsTrim::Missing),
        Err(error) => return Err(error),
    }
    let mut stale_temporaries_removed = 0;
    for attempt in 1..=max_attempts {
        let exclusive = match lock_exclusive(main_root) {
            Ok(Some(exclusive)) => exclusive,
            Ok(None) => continue,
            Err(error) => {
                return Ok(MetricsTrim::LockUnavailable(format!(
                    "the command metrics append lock is unavailable ({error}); nothing was trimmed"
                )));
            }
        };
        // Only a trim writes these, and only under this lock, so any present
        // now was left by one that stopped before its rename.
        stale_temporaries_removed += remove_stale_temporaries(path, None);
        let mut file = match open_regular_file_nonblocking(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(MetricsTrim::Missing);
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                return Ok(MetricsTrim::Refused(format!(
                    "the command metrics log is not a regular file: {error}"
                )));
            }
            Err(error) => return Err(error),
        };
        let opened = file.metadata()?;
        if !same_regular_file_version(&opened, &std::fs::symlink_metadata(path)?) {
            continue;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).unwrap_or(0));
        file.read_to_end(&mut bytes)?;
        if bytes.len() as u64 != opened.len()
            || !same_regular_file_version(&opened, &file.metadata()?)
        {
            continue;
        }
        let (after, removed, _malformed) = retained_metric_bytes_until(&bytes, cutoff, None)
            .expect("an unbounded metrics trim completes");
        if !removed {
            return Ok(MetricsTrim::NothingExpired {
                attempts: attempt,
                stale_temporaries_removed,
            });
        }
        let published = crate::atomic_file::with_synced_temporary(path, &after, |temporary| {
            at(TrimPoint::BeforePublish { attempt });
            // A writer that does not take the lock may have written since
            // the read. Publishing now would drop its row with the old inode.
            if !same_regular_file_version(&opened, &file.metadata()?)
                || !same_regular_file_version(&opened, &std::fs::symlink_metadata(path)?)
            {
                return Ok(false);
            }
            std::fs::rename(temporary, path)?;
            Ok(true)
        })?;
        if !published {
            continue;
        }
        at(TrimPoint::AfterPublish { attempt });
        let recovered_tail_bytes = copy_late_tail(&mut file, opened.len(), path, settle)?;
        drop(exclusive);
        return Ok(MetricsTrim::Trimmed {
            bytes_before: opened.len(),
            bytes_after: (after.len() as u64).saturating_add(recovered_tail_bytes),
            attempts: attempt,
            recovered_tail_bytes,
            stale_temporaries_removed,
        });
    }
    Ok(MetricsTrim::SkippedConcurrentAppend {
        attempts: max_attempts,
    })
}

/// The longest an unattended trim spends staging. The locked phase after it
/// is bounded by its attempts, the lock wait and the settle period instead.
const UNATTENDED_TRIM_BUDGET: Duration = Duration::from_secs(60);
/// The next unattended trim after one that finished (trimmed, nothing
/// expired, or no log): the cutoff moves by a day a day.
const UNATTENDED_TRIM_INTERVAL_MS: i64 = 24 * 60 * 60 * 1_000;
/// The next unattended trim after one that did not finish.
const UNATTENDED_TRIM_RETRY_MS: i64 = 30 * 60 * 1_000;
/// Broker meta key: the epoch ms at or after which the next unattended trim
/// runs.
pub(crate) const UNATTENDED_TRIM_NEXT_DUE_KEY: &str = "command_metrics_trim.next_due_at_ms";
/// Staging reads and writes through buffers this large.
const STAGE_BUFFER_BYTES: usize = 1024 * 1024;
/// Staging checks its deadline once per this many rows.
const STAGE_DEADLINE_CHECK_ROWS: u64 = 4_096;

/// Whether an unattended trim is due: never run, its next due time reached,
/// or that time more than one interval ahead (the clock went back).
pub(super) fn unattended_trim_due(next_due_ms: Option<i64>, now_ms: i64) -> bool {
    next_due_ms.is_none_or(|next| {
        now_ms >= next || next.saturating_sub(now_ms) > UNATTENDED_TRIM_INTERVAL_MS
    })
}

/// Rows a trim examined, by verdict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RowCounts {
    pub(super) removed: u64,
    pub(super) kept: u64,
}

impl RowCounts {
    /// Count `line` and say whether it is kept: the reviewed trim's rule,
    /// so a malformed or timestamp-free line is kept.
    fn keep(&mut self, line: &[u8], cutoff: i64) -> bool {
        match metric_row_timestamp(line) {
            Some(timestamp) if timestamp < cutoff => {
                self.removed = self.removed.saturating_add(1);
                false
            }
            _ => {
                self.kept = self.kept.saturating_add(1);
                true
            }
        }
    }
}

/// What one unattended trim did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UnattendedTrim {
    /// The trim ran to an outcome. `rows` counts what it examined; rows are
    /// removed only when `outcome` is [`MetricsTrim::Trimmed`].
    Finished {
        outcome: MetricsTrim,
        rows: RowCounts,
    },
    /// Staging ran out of its budget after `bytes_scanned`; nothing changed.
    BudgetExhausted { bytes_scanned: u64 },
}

impl UnattendedTrim {
    /// How long until the next unattended trim (ms).
    pub(super) fn next_due_in_ms(&self) -> i64 {
        match self {
            Self::Finished {
                outcome:
                    MetricsTrim::Trimmed { .. }
                    | MetricsTrim::NothingExpired { .. }
                    | MetricsTrim::Missing,
                ..
            } => UNATTENDED_TRIM_INTERVAL_MS,
            _ => UNATTENDED_TRIM_RETRY_MS,
        }
    }
}

/// The retained rows of a log's whole lines, written (unlocked) to the
/// trim's temporary and synced.
pub(super) struct StagedLog {
    temporary: tempfile::NamedTempFile,
    /// The log as staging opened it.
    source: std::fs::Metadata,
    /// Bytes of the log staged: whole lines from its start.
    staged_len: u64,
    /// Bytes of the temporary those lines kept.
    staged_after: u64,
    rows: RowCounts,
}

/// Drop rows with `ts < cutoff` from the log at `path`, unattended: stage
/// without the lock until `deadline`, then publish under it.
pub(super) fn trim_expired_rows_unattended(
    main_root: &Path,
    path: &Path,
    cutoff: i64,
    deadline: Instant,
) -> io::Result<UnattendedTrim> {
    let staged = {
        let _span = crate::phase_telemetry::span("command_metrics_trim_stage");
        stage_retained_rows(path, cutoff, deadline)?
    };
    match staged {
        Ok(staged) => {
            let _span = crate::phase_telemetry::span("command_metrics_trim_publish");
            publish_staged_rows(
                main_root,
                path,
                cutoff,
                staged,
                TRIM_MAX_ATTEMPTS,
                TAIL_SETTLE,
                &mut |_| {},
            )
        }
        Err(done) => Ok(done),
    }
}

/// Write the rows of the log's whole lines that `cutoff` keeps to a new
/// temporary beside it, without the append lock. An unterminated last line
/// (a row being written) is left to the locked phase. `Err` carries an
/// outcome that needs no locked phase.
pub(super) fn stage_retained_rows(
    path: &Path,
    cutoff: i64,
    deadline: Instant,
) -> io::Result<Result<StagedLog, UnattendedTrim>> {
    let finished = |outcome: MetricsTrim| -> io::Result<Result<StagedLog, UnattendedTrim>> {
        Ok(Err(UnattendedTrim::Finished {
            outcome,
            rows: RowCounts::default(),
        }))
    };
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return finished(MetricsTrim::Refused(
                "the command metrics log is a symlink and is never rewritten by GC".into(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return finished(MetricsTrim::Missing);
        }
        Err(error) => return Err(error),
    }
    let mut source = match open_regular_file_nonblocking(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return finished(MetricsTrim::Missing);
        }
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            return finished(MetricsTrim::Refused(format!(
                "the command metrics log is not a regular file: {error}"
            )));
        }
        Err(error) => return Err(error),
    };
    let opened = source.metadata()?;
    let directory = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the command metrics log has no parent",
        )
    })?;
    // `with_synced_temporary`'s naming, so a temporary left by a stopped
    // trim of either kind is recognised and removed by the next one.
    let mut temporary = tempfile::Builder::new()
        .prefix(TEMPORARY_PREFIX)
        .rand_bytes(TEMPORARY_RANDOM_LEN)
        .tempfile_in(directory)?;
    crate::host_state::protect_host_state_path(temporary.path(), false)?;
    let mut rows = RowCounts::default();
    let mut staged_len = 0_u64;
    let mut staged_after = 0_u64;
    {
        let mut reader =
            BufReader::with_capacity(STAGE_BUFFER_BYTES, Read::take(&mut source, opened.len()));
        let mut writer = BufWriter::with_capacity(STAGE_BUFFER_BYTES, temporary.as_file_mut());
        let mut line = Vec::new();
        let mut rows_until_deadline_check = 0_u64;
        loop {
            if rows_until_deadline_check == 0 {
                if Instant::now() >= deadline {
                    return Ok(Err(UnattendedTrim::BudgetExhausted {
                        bytes_scanned: staged_len,
                    }));
                }
                rows_until_deadline_check = STAGE_DEADLINE_CHECK_ROWS;
            }
            rows_until_deadline_check -= 1;
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 || line.last() != Some(&b'\n') {
                break;
            }
            staged_len = staged_len.saturating_add(read as u64);
            if rows.keep(&line, cutoff) {
                writer.write_all(&line)?;
                staged_after = staged_after.saturating_add(read as u64);
            }
        }
        writer.flush()?;
    }
    if rows.removed == 0 {
        // Dropping the temporary removes it; the log is left as it is.
        return Ok(Err(UnattendedTrim::Finished {
            outcome: MetricsTrim::NothingExpired {
                attempts: 1,
                stale_temporaries_removed: 0,
            },
            rows,
        }));
    }
    temporary.as_file().sync_all()?;
    Ok(Ok(StagedLog {
        temporary,
        source: opened,
        staged_len,
        staged_after,
        rows,
    }))
}

/// Whether `left` and `right` are the same file (not the same version).
/// Without inode numbers nothing proves it, so staging is never published.
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        false
    }
}

/// Under the exclusive append lock: filter what was appended since staging,
/// append it to the staged temporary, and publish that over the log with the
/// reviewed trim's checks (a version change before the rename restarts the
/// attempt; the replaced inode's late tail is copied forward after it).
pub(super) fn publish_staged_rows(
    main_root: &Path,
    path: &Path,
    cutoff: i64,
    mut staged: StagedLog,
    max_attempts: u32,
    settle: Duration,
    at: &mut dyn FnMut(TrimPoint),
) -> io::Result<UnattendedTrim> {
    let unchanged = |outcome: MetricsTrim| UnattendedTrim::Finished {
        outcome,
        rows: RowCounts::default(),
    };
    let mut stale_temporaries_removed = 0;
    for attempt in 1..=max_attempts {
        let exclusive = match lock_exclusive(main_root) {
            Ok(Some(exclusive)) => exclusive,
            Ok(None) => continue,
            Err(error) => {
                return Ok(unchanged(MetricsTrim::LockUnavailable(format!(
                    "the command metrics append lock is unavailable ({error}); nothing was trimmed"
                ))));
            }
        };
        stale_temporaries_removed += remove_stale_temporaries(path, Some(staged.temporary.path()));
        let mut file = match open_regular_file_nonblocking(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(unchanged(MetricsTrim::Missing));
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                return Ok(unchanged(MetricsTrim::Refused(format!(
                    "the command metrics log is not a regular file: {error}"
                ))));
            }
            Err(error) => return Err(error),
        };
        let opened = file.metadata()?;
        if !same_regular_file_version(&opened, &std::fs::symlink_metadata(path)?) {
            continue;
        }
        if !same_file(&staged.source, &opened) || opened.len() < staged.staged_len {
            // Another trim replaced the log (or something truncated it)
            // since staging: what was staged no longer describes its start.
            // Nothing was removed; the next run stages again.
            return Ok(unchanged(MetricsTrim::SkippedConcurrentAppend {
                attempts: attempt,
            }));
        }
        file.seek(SeekFrom::Start(staged.staged_len))?;
        let mut tail = Vec::new();
        Read::take(&mut file, opened.len() - staged.staged_len).read_to_end(&mut tail)?;
        if staged.staged_len.saturating_add(tail.len() as u64) != opened.len()
            || !same_regular_file_version(&opened, &file.metadata()?)
        {
            continue;
        }
        // The reviewed trim's rule for the tail too, an unterminated last
        // line included: it is kept as written.
        let mut tail_rows = RowCounts::default();
        let mut tail_after = Vec::with_capacity(tail.len());
        for line in tail.split_inclusive(|byte| *byte == b'\n') {
            if tail_rows.keep(line, cutoff) {
                tail_after.extend_from_slice(line);
            }
        }
        {
            // An earlier attempt's tail is cut off before this one's.
            let temporary = staged.temporary.as_file_mut();
            temporary.set_len(staged.staged_after)?;
            temporary.seek(SeekFrom::Start(staged.staged_after))?;
            temporary.write_all(&tail_after)?;
            temporary.sync_all()?;
        }
        at(TrimPoint::BeforePublish { attempt });
        // A writer that does not take the lock may have written since the
        // tail was read. Publishing now would drop its row with the old inode.
        if !same_regular_file_version(&opened, &file.metadata()?)
            || !same_regular_file_version(&opened, &std::fs::symlink_metadata(path)?)
        {
            continue;
        }
        let StagedLog {
            temporary,
            staged_after,
            rows,
            ..
        } = staged;
        temporary.persist(path).map_err(|error| error.error)?;
        at(TrimPoint::AfterPublish { attempt });
        let recovered_tail_bytes = copy_late_tail(&mut file, opened.len(), path, settle)?;
        drop(exclusive);
        return Ok(UnattendedTrim::Finished {
            outcome: MetricsTrim::Trimmed {
                bytes_before: opened.len(),
                bytes_after: staged_after
                    .saturating_add(tail_after.len() as u64)
                    .saturating_add(recovered_tail_bytes),
                attempts: attempt,
                recovered_tail_bytes,
                stale_temporaries_removed,
            },
            rows: RowCounts {
                removed: rows.removed.saturating_add(tail_rows.removed),
                kept: rows.kept.saturating_add(tail_rows.kept),
            },
        });
    }
    Ok(unchanged(MetricsTrim::SkippedConcurrentAppend {
        attempts: max_attempts,
    }))
}

/// Record one unattended trim's outcome in the command's telemetry.
fn record_unattended_trim(result: &io::Result<UnattendedTrim>, elapsed: Duration) {
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let add = |name| crate::phase_telemetry::add(name, Duration::ZERO);
    let mut fields = serde_json::Map::new();
    fields.insert("elapsed_ms".into(), elapsed_ms.into());
    let outcome = match result {
        Err(_) => {
            add("command_metrics_trim_failed");
            "failed"
        }
        Ok(UnattendedTrim::BudgetExhausted { bytes_scanned }) => {
            add("command_metrics_trim_budget_exhausted");
            fields.insert("bytes_scanned".into(), (*bytes_scanned).into());
            "budget_exhausted"
        }
        Ok(UnattendedTrim::Finished { outcome, rows }) => {
            fields.insert("rows_removed".into(), rows.removed.into());
            fields.insert("rows_kept".into(), rows.kept.into());
            match outcome {
                MetricsTrim::Trimmed {
                    bytes_before,
                    bytes_after,
                    attempts,
                    recovered_tail_bytes,
                    stale_temporaries_removed,
                } => {
                    add("command_metrics_trim_trimmed");
                    if *recovered_tail_bytes > 0 {
                        add("command_metrics_trim_tail_recovered");
                    }
                    fields.insert("bytes_before".into(), (*bytes_before).into());
                    fields.insert("bytes_after".into(), (*bytes_after).into());
                    fields.insert("attempts".into(), (*attempts).into());
                    fields.insert(
                        "recovered_tail_bytes".into(),
                        (*recovered_tail_bytes).into(),
                    );
                    fields.insert(
                        "stale_temporaries_removed".into(),
                        (*stale_temporaries_removed).into(),
                    );
                    "trimmed"
                }
                MetricsTrim::NothingExpired { .. } => {
                    add("command_metrics_trim_nothing_expired");
                    "nothing_expired"
                }
                MetricsTrim::Missing => "missing",
                MetricsTrim::SkippedConcurrentAppend { attempts } => {
                    add("command_metrics_trim_skipped_concurrent_append");
                    fields.insert("attempts".into(), (*attempts).into());
                    "skipped_concurrent_append"
                }
                MetricsTrim::LockUnavailable(_) => {
                    add("command_metrics_trim_lock_unavailable");
                    "lock_unavailable"
                }
                MetricsTrim::Refused(_) => {
                    add("command_metrics_trim_refused");
                    "refused"
                }
            }
        }
    };
    fields.insert("outcome".into(), outcome.into());
    crate::phase_telemetry::attach("command_metrics_trim", serde_json::Value::Object(fields));
}

impl crate::Broker {
    /// Trim `.aethyme/logs/command-metrics.jsonl` to `command_metrics_days`
    /// when an unattended trim is due (at most daily; 30 minutes after one
    /// that did not finish). The cutoff is the one a GC plan made today
    /// would review. Best effort: a failure is warned about and retried.
    pub(crate) fn trim_command_metrics_log_if_due(&self) {
        let store = self.store_ref();
        if store.is_read_only() {
            return;
        }
        let now_ms = crate::clock::epoch_ms();
        let next_due = store
            .meta_get(UNATTENDED_TRIM_NEXT_DUE_KEY)
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok());
        if !unattended_trim_due(next_due, now_ms) {
            crate::phase_telemetry::add("command_metrics_trim_not_due", Duration::ZERO);
            return;
        }
        let policy = match crate::load_retention_policy(self.main_root()) {
            Ok(policy) => policy,
            Err(error) => {
                crate::warn_unrecorded::<(), _>("load the command metrics retention", Err(error));
                return;
            }
        };
        let record_next_due = |due_in_ms: i64| {
            crate::warn_unrecorded(
                "record when the command metrics log is next trimmed",
                store.meta_set(
                    UNATTENDED_TRIM_NEXT_DUE_KEY,
                    &now_ms.saturating_add(due_in_ms).to_string(),
                ),
            );
        };
        // Claimed before the trim, so a worker that stops mid-trim, or a
        // second worker meanwhile, does not start another at once.
        record_next_due(UNATTENDED_TRIM_RETRY_MS);
        let _span = crate::phase_telemetry::span("command_metrics_trim");
        let main_root = self.main_root();
        let path = main_root.join(crate::retention::COMMAND_METRICS_LOG);
        let cutoff = super::cutoff(super::utc_day_floor_ms(now_ms), policy.command_metrics_days);
        let started = Instant::now();
        let result = trim_expired_rows_unattended(
            main_root,
            &path,
            cutoff,
            started + UNATTENDED_TRIM_BUDGET,
        );
        record_unattended_trim(&result, started.elapsed());
        match &result {
            Ok(trim) => record_next_due(trim.next_due_in_ms()),
            Err(error) => {
                crate::warn_unrecorded::<(), _>("trim the command metrics log", Err(error));
            }
        }
    }
}

/// The append lock held exclusively, or `None` when writers kept it busy;
/// closing the returned file releases it.
fn lock_exclusive(main_root: &Path) -> io::Result<Option<File>> {
    let file = open_command_metrics_lock(main_root)?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < TRIM_LOCK_WAIT => {
                std::thread::sleep(LOCK_POLL);
            }
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
}

/// Remove temporaries an interrupted trim left beside the log: regular files
/// in that directory whose name is exactly the trim's temporary pattern,
/// except `keep`, the calling trim's own staged temporary.
fn remove_stale_temporaries(path: &Path, keep: Option<&Path>) -> usize {
    let Some(directory) = path.parent() else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(random) = name.strip_prefix(TEMPORARY_PREFIX) else {
            continue;
        };
        if random.len() != TEMPORARY_RANDOM_LEN
            || !random.bytes().all(|byte| byte.is_ascii_alphanumeric())
            || !entry.file_type().is_ok_and(|kind| kind.is_file())
            || keep.is_some_and(|keep| keep == entry.path().as_path())
        {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Watch the replaced inode for `settle` and copy what an unlocked writer
/// appended there into the new file, in order. `copied` is how much of it
/// the trim read. While settling only whole lines are copied, so a row being
/// written is not split; whatever remains when the period ends is copied as
/// written, even without a final newline, rather than dropped.
fn copy_late_tail(
    replaced: &mut File,
    mut copied: u64,
    path: &Path,
    settle: Duration,
) -> io::Result<u64> {
    let deadline = Instant::now() + settle;
    let mut recovered = 0_u64;
    loop {
        let settled = Instant::now() >= deadline;
        let length = replaced.metadata()?.len();
        if length > copied {
            replaced.seek(SeekFrom::Start(copied))?;
            let mut tail = Vec::new();
            let mut reader = Read::take(&mut *replaced, length - copied);
            reader.read_to_end(&mut tail)?;
            let whole = match tail.iter().rposition(|byte| *byte == b'\n') {
                _ if settled => tail.len(),
                Some(index) => index + 1,
                None => 0,
            };
            if whole > 0 {
                append_nofollow(path)?.write_all(&tail[..whole])?;
                copied = copied.saturating_add(whole as u64);
                recovered = recovered.saturating_add(whole as u64);
            }
        }
        if settled {
            return Ok(recovered);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(LOCK_POLL.min(remaining));
    }
}

fn append_nofollow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Append one metric row under the shared append lock, and say whether the
/// lock was held. Best effort, like the metric itself: a lock that cannot be
/// opened or stays busy past [`APPEND_LOCK_WAIT`] degrades to the unlocked
/// append older binaries do.
pub(crate) fn append_command_metric_line(main_root: &Path, path: &Path, line: &[u8]) -> bool {
    append_command_metric_line_waiting(main_root, path, line, APPEND_LOCK_WAIT)
}

fn append_command_metric_line_waiting(
    main_root: &Path,
    path: &Path,
    line: &[u8],
    wait: Duration,
) -> bool {
    // Held until `lock` closes at the end of this function.
    let lock = open_command_metrics_lock(main_root).ok();
    let locked = lock.as_ref().is_some_and(|file| wait_shared(file, wait));
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line);
    }
    locked
}

fn wait_shared(file: &File, wait: Duration) -> bool {
    let started = Instant::now();
    loop {
        match file.try_lock_shared() {
            Ok(()) => return true,
            Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < wait => {
                std::thread::sleep(LOCK_POLL);
            }
            Err(_) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "{\"ts\":1,\"command\":\"old\"}\n";
    const KEPT: &str = "{\"ts\":50,\"command\":\"kept\"}\n";
    const MALFORMED: &str = "not-json\n";
    const LATE: &str = "{\"ts\":60,\"command\":\"late\"}\n";
    const LATER: &str = "{\"ts\":70,\"command\":\"later\"}\n";

    fn log(root: &Path, content: &str) -> std::path::PathBuf {
        let directory = root.join(".aethyme/logs");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("command-metrics.jsonl");
        std::fs::write(&path, content).unwrap();
        path
    }

    fn append(path: &Path, line: &str) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }

    /// Three attempts, no settle period, and `at` as the concurrent writer.
    fn trim(root: &Path, path: &Path, at: &mut dyn FnMut(TrimPoint)) -> MetricsTrim {
        let result = trim_expired_rows_with(root, path, 10, 3, Duration::ZERO, at);
        result.unwrap()
    }

    #[test]
    fn trim_drops_only_expired_rows_and_keeps_malformed_lines_in_order() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{MALFORMED}{KEPT}"));

        let outcome = trim(root.path(), &path, &mut |_| {});

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{MALFORMED}{KEPT}")
        );
        assert!(matches!(
            outcome,
            MetricsTrim::Trimmed {
                attempts: 1,
                recovered_tail_bytes: 0,
                ..
            }
        ));
        assert_eq!(
            trim(root.path(), &path, &mut |_| {}),
            MetricsTrim::NothingExpired {
                attempts: 1,
                stale_temporaries_removed: 0,
            }
        );
    }

    #[test]
    fn an_unlocked_append_before_the_rename_restarts_the_attempt_and_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let writer_path = path.clone();

        let outcome = trim(root.path(), &path, &mut |point| {
            if point == (TrimPoint::BeforePublish { attempt: 1 }) {
                append(&writer_path, LATE);
            }
        });

        assert!(
            matches!(outcome, MetricsTrim::Trimmed { attempts: 2, .. }),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{KEPT}{LATE}")
        );
    }

    #[test]
    fn an_append_to_the_replaced_inode_after_the_rename_is_copied_forward() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let writer_path = path.clone();
        // A writer that opened the log before the rename and writes after it.
        let mut early_writer: Option<File> = None;

        let outcome = trim(root.path(), &path, &mut |point| match point {
            TrimPoint::BeforePublish { .. } => {
                early_writer = Some(OpenOptions::new().append(true).open(&writer_path).unwrap());
            }
            TrimPoint::AfterPublish { .. } => {
                early_writer
                    .as_mut()
                    .unwrap()
                    .write_all(LATE.as_bytes())
                    .unwrap();
            }
        });

        assert_eq!(
            outcome,
            MetricsTrim::Trimmed {
                bytes_before: (OLD.len() + KEPT.len()) as u64,
                bytes_after: (KEPT.len() + LATE.len()) as u64,
                attempts: 1,
                recovered_tail_bytes: LATE.len() as u64,
                stale_temporaries_removed: 0,
            }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{KEPT}{LATE}")
        );
    }

    #[test]
    fn a_partial_late_row_is_copied_whole_lines_first_and_never_dropped() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let writer_path = path.clone();
        let mut early_writer: Option<File> = None;
        let mut at = |point: TrimPoint| match point {
            TrimPoint::BeforePublish { .. } => {
                early_writer = Some(OpenOptions::new().append(true).open(&writer_path).unwrap());
            }
            TrimPoint::AfterPublish { .. } => {
                let late = format!("{LATE}{{\"ts\":61");
                let writer = early_writer.as_mut().unwrap();
                writer.write_all(late.as_bytes()).unwrap();
            }
        };

        // Whatever the runner's timing, the settle period copies the whole
        // row first and the unterminated bytes when it ends.
        let settle = Duration::from_millis(30);
        let result = trim_expired_rows_with(root.path(), &path, 10, 3, settle, &mut at);
        let outcome = result.unwrap();

        assert!(
            matches!(outcome, MetricsTrim::Trimmed { attempts: 1, .. }),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{KEPT}{LATE}{{\"ts\":61")
        );
    }

    #[test]
    fn a_log_that_grows_on_every_attempt_is_skipped_without_losing_a_row() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let writer_path = path.clone();

        let outcome = trim(root.path(), &path, &mut |point| {
            if matches!(point, TrimPoint::BeforePublish { .. }) {
                append(&writer_path, LATE);
            }
        });

        assert_eq!(
            outcome,
            MetricsTrim::SkippedConcurrentAppend { attempts: 3 }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{OLD}{KEPT}{LATE}{LATE}{LATE}"),
            "a skipped trim removes nothing and keeps every appended row"
        );
    }

    #[test]
    fn writers_take_the_append_lock_the_trim_holds_exclusively() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let repo = root.path().to_path_buf();
        let log_path = path.clone();
        let mut locked_during_trim = None;

        let outcome = trim(root.path(), &path, &mut |point| {
            if point == (TrimPoint::BeforePublish { attempt: 1 }) {
                let (line, wait) = (LATE.as_bytes(), Duration::ZERO);
                let locked = append_command_metric_line_waiting(&repo, &log_path, line, wait);
                locked_during_trim = Some(locked);
            }
        });

        assert_eq!(
            locked_during_trim,
            Some(false),
            "the trim holds the append lock exclusively"
        );
        assert!(
            append_command_metric_line(root.path(), &path, LATER.as_bytes()),
            "a writer takes the append lock once the trim releases it"
        );
        // The unlocked fallback row restarted the attempt and was kept.
        assert!(
            matches!(outcome, MetricsTrim::Trimmed { attempts: 2, .. }),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{KEPT}{LATE}{LATER}")
        );
    }

    #[test]
    fn a_locked_writer_waits_for_the_trim_and_lands_in_the_new_file() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let main_root = root.path().to_path_buf();
        let writer_path = path.clone();
        let mut writer: Option<std::thread::JoinHandle<bool>> = None;

        let outcome = trim(root.path(), &path, &mut |point| {
            if point == (TrimPoint::BeforePublish { attempt: 1 }) {
                let (main_root, writer_path) = (main_root.clone(), writer_path.clone());
                let (started, wait) = std::sync::mpsc::channel();
                writer = Some(std::thread::spawn(move || {
                    started.send(()).unwrap();
                    append_command_metric_line(&main_root, &writer_path, LATE.as_bytes())
                }));
                wait.recv().unwrap();
            }
        });
        writer.unwrap().join().unwrap();

        // However the runner schedules the writer, its row survives: blocked
        // on the lock it appends to the new file, and any unlocked fallback
        // is caught by the version checks or the post-rename copy.
        assert!(
            matches!(outcome, MetricsTrim::Trimmed { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{KEPT}{LATE}")
        );
    }

    #[test]
    fn only_the_trims_own_stale_temporaries_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), KEPT);
        let directory = path.parent().unwrap();
        let stale = directory.join(".command-metrics.jsonl-a1B2c3");
        let kept = [
            directory.join(".command-metrics.jsonl-a1B2c"),
            directory.join(".command-metrics.jsonl-a1B2c3.tmp"),
            directory.join(".command-metrics.jsonl-a1-2c3"),
            directory.join("command-metrics.jsonl-a1B2c3"),
        ];
        for file in std::iter::once(&stale).chain(&kept) {
            std::fs::write(file, "partial").unwrap();
        }
        let stale_directory = directory.join(".command-metrics.jsonl-d1r2c3");
        std::fs::create_dir(&stale_directory).unwrap();

        assert_eq!(
            trim(root.path(), &path, &mut |_| {}),
            MetricsTrim::NothingExpired {
                attempts: 1,
                stale_temporaries_removed: 1,
            }
        );
        assert!(!stale.exists());
        assert!(kept.iter().all(|file| file.exists()));
        assert!(stale_directory.is_dir(), "only regular files are removed");
    }

    #[test]
    fn an_unavailable_append_lock_trims_nothing() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let run = root.path().join(".aethyme/run");
        std::fs::write(&run, "not a directory").unwrap();

        assert!(matches!(
            trim(root.path(), &path, &mut |_| {}),
            MetricsTrim::LockUnavailable(_)
        ));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{OLD}{KEPT}")
        );
    }

    #[test]
    fn a_missing_log_or_a_symlink_is_never_replaced() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".aethyme/logs/command-metrics.jsonl");
        assert_eq!(trim(root.path(), &path, &mut |_| {}), MetricsTrim::Missing);
        #[cfg(unix)]
        {
            let target = root.path().join("elsewhere.jsonl");
            std::fs::write(&target, OLD).unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(matches!(
                trim(root.path(), &path, &mut |_| {}),
                MetricsTrim::Refused(_)
            ));
            assert_eq!(std::fs::read_to_string(&target).unwrap(), OLD);
        }
    }

    fn far_deadline() -> Instant {
        Instant::now() + Duration::from_secs(600)
    }

    /// Stage, run `between` as a writer, then publish with no settle period.
    fn unattended(
        root: &Path,
        path: &Path,
        between: &mut dyn FnMut(),
        at: &mut dyn FnMut(TrimPoint),
    ) -> UnattendedTrim {
        match stage_retained_rows(path, 10, far_deadline()).unwrap() {
            Ok(staged) => {
                between();
                publish_staged_rows(root, path, 10, staged, 3, Duration::ZERO, at).unwrap()
            }
            Err(done) => done,
        }
    }

    fn trimmed_rows(outcome: &UnattendedTrim) -> RowCounts {
        match outcome {
            UnattendedTrim::Finished {
                outcome: MetricsTrim::Trimmed { .. },
                rows,
            } => *rows,
            other => panic!("expected a trim, got {other:?}"),
        }
    }

    #[test]
    fn unattended_trim_drops_expired_rows_and_keeps_newer_and_malformed_in_order() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{MALFORMED}{OLD}{KEPT}"));

        let outcome = trim_expired_rows_unattended(root.path(), &path, 10, far_deadline());
        let outcome = outcome.unwrap();

        assert_eq!(
            trimmed_rows(&outcome),
            RowCounts {
                removed: 2,
                kept: 2
            }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{MALFORMED}{KEPT}")
        );
        let directory = path.parent().unwrap();
        let leftovers = std::fs::read_dir(directory).unwrap().count();
        assert_eq!(leftovers, 1, "the staged temporary became the log");
    }

    #[test]
    fn unattended_trim_within_policy_leaves_the_log_untouched() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{KEPT}{MALFORMED}{LATE}"));
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&path).unwrap().ino()
        };

        let outcome = trim_expired_rows_unattended(root.path(), &path, 10, far_deadline());

        assert_eq!(
            outcome.unwrap(),
            UnattendedTrim::Finished {
                outcome: MetricsTrim::NothingExpired {
                    attempts: 1,
                    stale_temporaries_removed: 0,
                },
                rows: RowCounts {
                    removed: 0,
                    kept: 3
                },
            }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{KEPT}{MALFORMED}{LATE}")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        }
        let entries = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(entries, 1, "no temporary is left behind");
    }

    #[cfg(unix)]
    #[test]
    fn rows_appended_while_staging_are_filtered_under_the_lock_and_kept() {
        let root = tempfile::tempdir().unwrap();
        // A writer is mid-row when staging reads: the unterminated line is
        // left to the locked phase.
        let path = log(root.path(), &format!("{OLD}{KEPT}{{\"ts\":60,"));
        let writer_path = path.clone();

        let outcome = unattended(
            root.path(),
            &path,
            &mut || {
                append(&writer_path, "\"command\":\"late\"}\n");
                append(&writer_path, OLD);
                append(&writer_path, LATER);
            },
            &mut |_| {},
        );

        assert_eq!(
            trimmed_rows(&outcome),
            RowCounts {
                removed: 2,
                kept: 3
            }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{KEPT}{{\"ts\":60,\"command\":\"late\"}}\n{LATER}")
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unterminated_last_line_is_kept_as_the_reviewed_trim_keeps_it() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}{{\"ts\":61"));

        let outcome = trim_expired_rows_unattended(root.path(), &path, 10, far_deadline());

        assert!(matches!(
            outcome.unwrap(),
            UnattendedTrim::Finished {
                outcome: MetricsTrim::Trimmed { .. },
                ..
            }
        ));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{KEPT}{{\"ts\":61")
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unattended_unlocked_append_before_the_rename_restarts_and_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let writer_path = path.clone();

        let outcome = unattended(root.path(), &path, &mut || {}, &mut |point| {
            if point == (TrimPoint::BeforePublish { attempt: 1 }) {
                append(&writer_path, LATE);
            }
        });

        assert!(
            matches!(
                outcome,
                UnattendedTrim::Finished {
                    outcome: MetricsTrim::Trimmed { attempts: 2, .. },
                    ..
                }
            ),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{KEPT}{LATE}"),
            "the first attempt's tail is not duplicated by the second"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unattended_append_to_the_replaced_inode_is_copied_forward() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let writer_path = path.clone();
        let mut early_writer: Option<File> = None;

        let outcome = unattended(root.path(), &path, &mut || {}, &mut |point| match point {
            TrimPoint::BeforePublish { .. } => {
                early_writer = Some(OpenOptions::new().append(true).open(&writer_path).unwrap());
            }
            TrimPoint::AfterPublish { .. } => {
                let writer = early_writer.as_mut().unwrap();
                writer.write_all(LATE.as_bytes()).unwrap();
            }
        });

        assert!(
            matches!(
                outcome,
                UnattendedTrim::Finished {
                    outcome: MetricsTrim::Trimmed {
                        recovered_tail_bytes,
                        ..
                    },
                    ..
                } if recovered_tail_bytes == LATE.len() as u64
            ),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{KEPT}{LATE}")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_log_replaced_since_staging_is_not_published_over() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{OLD}{KEPT}"));
        let replaced = format!("{KEPT}{LATE}{LATER}");
        let writer_path = path.clone();

        // Another trim (a reviewed `gc apply`) replaced the log meanwhile,
        // with a file at least as long as what was staged.
        let outcome = unattended(
            root.path(),
            &path,
            &mut || {
                let other = writer_path.with_file_name("other");
                std::fs::write(&other, &replaced).unwrap();
                std::fs::rename(&other, &writer_path).unwrap();
            },
            &mut |_| {},
        );

        assert!(
            matches!(
                outcome,
                UnattendedTrim::Finished {
                    outcome: MetricsTrim::SkippedConcurrentAppend { attempts: 1 },
                    rows: RowCounts {
                        removed: 0,
                        kept: 0
                    },
                }
            ),
            "{outcome:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), replaced);
        let entries = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(entries, 1, "the abandoned staging is removed");
    }

    #[test]
    fn an_exhausted_staging_budget_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));

        let outcome = trim_expired_rows_unattended(root.path(), &path, 10, Instant::now());

        assert_eq!(
            outcome.unwrap(),
            UnattendedTrim::BudgetExhausted { bytes_scanned: 0 }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{OLD}{KEPT}")
        );
        let entries = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(entries, 1, "no temporary is left behind");
    }

    #[test]
    fn a_missing_log_is_a_finished_unattended_trim() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".aethyme/logs/command-metrics.jsonl");
        let outcome = trim_expired_rows_unattended(root.path(), &path, 10, far_deadline());
        let outcome = outcome.unwrap();
        assert_eq!(
            outcome,
            UnattendedTrim::Finished {
                outcome: MetricsTrim::Missing,
                rows: RowCounts::default(),
            }
        );
        assert_eq!(outcome.next_due_in_ms(), UNATTENDED_TRIM_INTERVAL_MS);
    }

    #[test]
    fn the_unattended_trim_runs_at_most_on_its_interval() {
        let now = 100 * UNATTENDED_TRIM_INTERVAL_MS;
        assert!(unattended_trim_due(None, now), "never run");
        assert!(unattended_trim_due(Some(now), now), "due now");
        assert!(unattended_trim_due(Some(now - 1), now), "overdue");
        assert!(!unattended_trim_due(Some(now + 1), now), "not yet due");
        assert!(
            !unattended_trim_due(Some(now + UNATTENDED_TRIM_INTERVAL_MS), now),
            "a day ahead is the interval, not a clock that went back"
        );
        assert!(
            unattended_trim_due(Some(now + UNATTENDED_TRIM_INTERVAL_MS + 1), now),
            "a clock that went back runs it"
        );

        let finished = |outcome| UnattendedTrim::Finished {
            outcome,
            rows: RowCounts::default(),
        };
        let nothing = finished(MetricsTrim::NothingExpired {
            attempts: 1,
            stale_temporaries_removed: 0,
        });
        assert_eq!(nothing.next_due_in_ms(), UNATTENDED_TRIM_INTERVAL_MS);
        let skipped = finished(MetricsTrim::SkippedConcurrentAppend { attempts: 3 });
        assert_eq!(skipped.next_due_in_ms(), UNATTENDED_TRIM_RETRY_MS);
        let exhausted = UnattendedTrim::BudgetExhausted { bytes_scanned: 1 };
        assert_eq!(exhausted.next_due_in_ms(), UNATTENDED_TRIM_RETRY_MS);
    }

    #[test]
    fn a_trim_keeps_its_own_staged_temporary_but_removes_stale_ones() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), KEPT);
        let directory = path.parent().unwrap();
        let own = directory.join(".command-metrics.jsonl-Own123");
        let stale = directory.join(".command-metrics.jsonl-Old456");
        std::fs::write(&own, "staged").unwrap();
        std::fs::write(&stale, "partial").unwrap();

        assert_eq!(remove_stale_temporaries(&path, Some(own.as_path())), 1);
        assert!(own.exists());
        assert!(!stale.exists());
    }
}
