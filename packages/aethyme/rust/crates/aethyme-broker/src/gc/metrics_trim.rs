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
//!
//! Staging that runs out of its budget keeps its temporary and records where
//! it stopped (broker meta key [`UNATTENDED_TRIM_RESUME_KEY`]): the log's
//! device and inode, the line boundary reached, a fingerprint of the bytes
//! before it, the temporary's length and the row counts so far. The next run
//! continues from there when the log, the fingerprint and the temporary still
//! match the record, and starts over otherwise. Rows staged under an earlier
//! cutoff were judged against that cutoff, which is never later than the
//! current one (a record from a later cutoff is not resumed), so at worst
//! they keep a row the current cutoff would remove.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::file_lock::open_command_metrics_lock;

use super::{
    metric_row_verdict, open_regular_file_nonblocking, retained_metric_bytes_until,
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
/// else in its current order. `spare` is an unattended trim's resumable
/// temporary ([`resumable_temporary`]), which the stale-temporary sweep keeps.
pub(super) fn trim_expired_rows(
    main_root: &Path,
    path: &Path,
    cutoff: i64,
    spare: Option<&Path>,
) -> io::Result<MetricsTrim> {
    trim_expired_rows_with(
        main_root,
        path,
        cutoff,
        TRIM_MAX_ATTEMPTS,
        TAIL_SETTLE,
        spare,
        &mut |_| {},
    )
}

pub(super) fn trim_expired_rows_with(
    main_root: &Path,
    path: &Path,
    cutoff: i64,
    max_attempts: u32,
    settle: Duration,
    spare: Option<&Path>,
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
        // Only a trim writes these: the reviewed one under this lock, an
        // unattended one also while staging without it. The sweep removes
        // only those older than any staging lasts, which were left by a trim
        // that stopped before its rename, except a resumable one.
        stale_temporaries_removed += remove_stale_temporaries(path, spare);
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
/// The next unattended trim after staging ran out of budget and recorded
/// where to resume: the next retirement run, since its progress is kept.
const UNATTENDED_TRIM_RESUME_MS: i64 = 0;
/// Broker meta key: the epoch ms at or after which the next unattended trim
/// runs.
pub(crate) const UNATTENDED_TRIM_NEXT_DUE_KEY: &str = "command_metrics_trim.next_due_at_ms";
/// Broker meta key: where the next unattended trim resumes staging (a JSON
/// [`StagingResume`]), or empty when it starts over.
pub(crate) const UNATTENDED_TRIM_RESUME_KEY: &str = "command_metrics_trim.resume";
/// How long a resume record spares its temporary, counted from when the
/// record was written (every attempt that runs out of budget rewrites it).
/// Past this the record no longer resumes or spares anything: it is cleared
/// and its temporary removed, so a temporary is never kept indefinitely
/// once unattended trims stop running.
pub(super) const RESUME_SPARE_MAX_AGE_MS: i64 = 24 * 60 * 60 * 1_000;
/// A resume record fingerprints this many bytes from the log's start and as
/// many ending at the offset reached, so a log whose inode number was reused
/// by another file is not mistaken for the one staged.
const RESUME_FINGERPRINT_BYTES: u64 = 64 * 1024;
/// The least staging budget worth starting a trim with; with less left in
/// the retirement run, the trim waits for a later run.
const UNATTENDED_TRIM_MIN_BUDGET: Duration = Duration::from_secs(5);
/// A trim temporary modified more recently than this may be an unattended
/// trim's staging in progress (it writes at least every buffer's worth), so
/// no sweep removes it: the staging budget and a margin.
const STALE_TEMPORARY_MIN_AGE: Duration = Duration::from_secs(120);
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
    /// so a malformed or timestamp-free line is kept. `newest` tracks the
    /// latest `ts` seen.
    fn keep(&mut self, line: &[u8], cutoff: i64, newest: &mut Option<i64>) -> bool {
        let (expired, timestamp) = metric_row_verdict(line, cutoff);
        if let Some(timestamp) = timestamp {
            *newest = Some(newest.map_or(timestamp, |seen| seen.max(timestamp)));
        }
        if expired {
            self.removed = self.removed.saturating_add(1);
            false
        } else {
            self.kept = self.kept.saturating_add(1);
            true
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
    /// Staging ran out of its budget at log offset `bytes_scanned` (counting
    /// what earlier attempts staged when this one resumed); the log is
    /// unchanged. `resume` is where the next attempt continues, its staged
    /// temporary kept; `None` when nothing was staged. Boxed: the record is
    /// several times larger than every other variant.
    BudgetExhausted {
        bytes_scanned: u64,
        resume: Option<Box<StagingResume>>,
    },
    /// Every row with a timestamp is older than the cutoff, newest included:
    /// the clock is suspect (or the repository has been idle for the whole
    /// retention window), so nothing was removed.
    ClockSuspect {
        cutoff: i64,
        newest_row_ts: i64,
        rows: RowCounts,
    },
    /// Without inode numbers (non-Unix) staging cannot prove the log was
    /// not replaced before publishing, so the unattended trim does not run;
    /// the reviewed `gc apply` still trims there.
    Unsupported,
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
            }
            | Self::ClockSuspect { .. }
            | Self::Unsupported => UNATTENDED_TRIM_INTERVAL_MS,
            Self::BudgetExhausted {
                resume: Some(_), ..
            } => UNATTENDED_TRIM_RESUME_MS,
            _ => UNATTENDED_TRIM_RETRY_MS,
        }
    }
}

/// Where an unattended trim whose staging ran out of budget resumes. Stored
/// as JSON under [`UNATTENDED_TRIM_RESUME_KEY`]; the next attempt resumes
/// only when the log is the same file (device, inode and fingerprint), at
/// least `source_offset` long, the cutoff is not earlier than `cutoff`, and
/// the temporary still exists with exactly `staged_bytes`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct StagingResume {
    /// The staged log's device number.
    pub(super) device: u64,
    /// The staged log's inode number.
    pub(super) inode: u64,
    /// SHA-256 of the log's first bytes and of the bytes ending at
    /// `source_offset` ([`RESUME_FINGERPRINT_BYTES`] each).
    pub(super) fingerprint: String,
    /// Bytes of the log staged: whole lines from its start.
    pub(super) source_offset: u64,
    /// Bytes of the temporary those lines kept.
    pub(super) staged_bytes: u64,
    /// Rows staged so far, by verdict.
    pub(super) removed: u64,
    pub(super) kept: u64,
    /// The newest `ts` staged so far, for the clock check.
    pub(super) newest: Option<i64>,
    /// The cutoff the rows were staged under.
    pub(super) cutoff: i64,
    /// The temporary's file name, beside the log.
    pub(super) temporary: String,
    /// When the record was written (epoch ms). A record from before this
    /// field reads as 0, so it counts as expired.
    #[serde(default)]
    pub(super) recorded_at_ms: i64,
}

impl StagingResume {
    /// Parse a stored record; `None` for an empty or unreadable one.
    pub(super) fn parse(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }

    /// Whether the record is [`RESUME_SPARE_MAX_AGE_MS`] old or older at
    /// `now_ms`. A record dated ahead of the clock is not expired.
    pub(super) fn expired(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.recorded_at_ms) >= RESUME_SPARE_MAX_AGE_MS
    }

    fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a resume record serializes")
    }

    /// The recorded temporary's path, when its name is a trim temporary's.
    fn temporary_path(&self, log: &Path) -> Option<PathBuf> {
        if !is_temporary_name(&self.temporary) {
            return None;
        }
        log.parent()
            .map(|directory| directory.join(&self.temporary))
    }

    /// Whether `log` is the file staged (by device and inode) and still
    /// holds the staged bytes.
    fn describes(&self, log: &std::fs::Metadata) -> bool {
        file_identity(log) == Some((self.device, self.inode)) && log.len() >= self.source_offset
    }
}

/// The recorded resumable temporary of the log at `log`, while the record
/// still describes the log and the temporary and is younger than
/// [`RESUME_SPARE_MAX_AGE_MS`]; a trim's stale-temporary sweep spares it.
/// An older record is cleared (only if unchanged since it was read) and its
/// temporary removed.
pub(super) fn resumable_temporary(store: &crate::BrokerStore, log: &Path) -> Option<PathBuf> {
    let recorded = store.meta_get(UNATTENDED_TRIM_RESUME_KEY).ok()??;
    let resume = StagingResume::parse(&recorded)?;
    if expire_resume_if_old(&resume, log, crate::clock::epoch_ms()) {
        crate::warn_unrecorded(
            "clear an expired command metrics trim resume record",
            store
                .meta_compare_and_set(UNATTENDED_TRIM_RESUME_KEY, Some(&recorded), "")
                .map(drop),
        );
        return None;
    }
    recorded_temporary_if_valid(&resume, log)
}

/// Whether `resume` is expired at `now_ms` ([`StagingResume::expired`]);
/// when it is, its temporary (a regular file with a trim temporary's name,
/// beside `log`) is removed, unless it was modified within
/// [`STALE_TEMPORARY_MIN_AGE`] (a trim may be staging into it: the
/// stale-temporary sweep removes it later), and the expiry counted.
fn expire_resume_if_old(resume: &StagingResume, log: &Path, now_ms: i64) -> bool {
    if !resume.expired(now_ms) {
        return false;
    }
    crate::phase_telemetry::add("command_metrics_trim_resume_expired", Duration::ZERO);
    if let Some(temporary) = resume.temporary_path(log) {
        if modified_at_least(
            &temporary,
            STALE_TEMPORARY_MIN_AGE,
            std::time::SystemTime::now(),
        ) {
            remove_recorded_temporary(&temporary);
        } else {
            crate::phase_telemetry::add(
                "command_metrics_trim_resume_expired_recently_modified",
                Duration::ZERO,
            );
        }
    }
    true
}

/// Whether the file at `path` was last modified at least `age` before
/// `now`. A missing file or an unreadable time is not.
fn modified_at_least(path: &Path, age: Duration, now: std::time::SystemTime) -> bool {
    std::fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|elapsed| elapsed >= age)
}

/// `resume`'s temporary, while the record still describes the log at `log`
/// and the temporary.
fn recorded_temporary_if_valid(resume: &StagingResume, log: &Path) -> Option<PathBuf> {
    let temporary = resume.temporary_path(log)?;
    let log_metadata = std::fs::symlink_metadata(log).ok()?;
    let temporary_metadata = std::fs::symlink_metadata(&temporary).ok()?;
    (log_metadata.is_file()
        && resume.describes(&log_metadata)
        && temporary_metadata.is_file()
        && temporary_metadata.len() == resume.staged_bytes)
        .then_some(temporary)
}

/// The device and inode number of `metadata`, where the platform has them.
fn file_identity(metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

/// SHA-256 (hex) of the first [`RESUME_FINGERPRINT_BYTES`] of `source` and
/// of as many ending at `offset`, with `offset` itself.
fn source_fingerprint(source: &mut File, offset: u64) -> io::Result<String> {
    let mut digest = Sha256::new();
    digest.update(offset.to_le_bytes());
    let window = offset.min(RESUME_FINGERPRINT_BYTES);
    for start in [0, offset - window] {
        source.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::with_capacity(usize::try_from(window).unwrap_or(0));
        Read::take(&mut *source, window).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != window {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the command metrics log is shorter than what was staged",
            ));
        }
        digest.update(&bytes);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// The retained rows of a log's whole lines, written (unlocked) to the
/// trim's temporary and synced.
pub(super) struct StagedLog {
    temporary: tempfile::NamedTempFile,
    /// The log staging read, held open until the trim ends so its inode
    /// cannot be freed and reused by another file meanwhile.
    _source: File,
    /// The log as staging opened it.
    source: std::fs::Metadata,
    /// Bytes of the log staged: whole lines from its start.
    staged_len: u64,
    /// Bytes of the temporary those lines kept.
    staged_after: u64,
    rows: RowCounts,
}

/// What staging produced, and the log offset it resumed from (0 when it
/// started from the first byte).
pub(super) struct Staging {
    pub(super) result: Result<StagedLog, UnattendedTrim>,
    pub(super) resumed_from_bytes: u64,
}

/// Staging's position: what it has read and kept so far.
#[derive(Default)]
struct StagingProgress {
    staged_len: u64,
    staged_after: u64,
    rows: RowCounts,
    newest: Option<i64>,
}

/// Drop rows with `ts < cutoff` from the log at `path`, unattended: stage
/// without the lock until `deadline`, then publish under it. Staging resumes
/// from `resume` when it still matches. Returns the outcome and the log
/// offset staging resumed from.
pub(super) fn trim_expired_rows_resuming(
    main_root: &Path,
    path: &Path,
    cutoff: i64,
    deadline: Instant,
    resume: Option<&StagingResume>,
) -> io::Result<(UnattendedTrim, u64)> {
    if cfg!(not(unix)) {
        return Ok((UnattendedTrim::Unsupported, 0));
    }
    let staging = {
        let _span = crate::phase_telemetry::span("command_metrics_trim_stage");
        stage_retained_rows_resuming(path, cutoff, deadline, resume)?
    };
    let outcome = match staging.result {
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
            )?
        }
        Err(done) => done,
    };
    Ok((outcome, staging.resumed_from_bytes))
}

/// [`trim_expired_rows_resuming`] from the log's first byte.
#[cfg(test)]
pub(super) fn trim_expired_rows_unattended(
    main_root: &Path,
    path: &Path,
    cutoff: i64,
    deadline: Instant,
) -> io::Result<UnattendedTrim> {
    trim_expired_rows_resuming(main_root, path, cutoff, deadline, None).map(|(trim, _)| trim)
}

/// [`stage_retained_rows_resuming`] from the log's first byte.
#[cfg(test)]
pub(super) fn stage_retained_rows(
    path: &Path,
    cutoff: i64,
    deadline: Instant,
) -> io::Result<Result<StagedLog, UnattendedTrim>> {
    stage_retained_rows_resuming(path, cutoff, deadline, None).map(|staging| staging.result)
}

/// Write the rows of the log's whole lines that `cutoff` keeps to a
/// temporary beside it, without the append lock: continuing `resume`'s
/// temporary when it still matches, else a new one. An unterminated last
/// line (a row being written) is left to the locked phase. `Err` carries an
/// outcome that needs no locked phase. A recorded temporary that is not
/// resumed is removed.
pub(super) fn stage_retained_rows_resuming(
    path: &Path,
    cutoff: i64,
    deadline: Instant,
    resume: Option<&StagingResume>,
) -> io::Result<Staging> {
    stage_retained_rows_checking(path, cutoff, resume, STAGE_DEADLINE_CHECK_ROWS, &mut || {
        Instant::now() >= deadline
    })
}

/// [`stage_retained_rows_resuming`], asking `out_of_time` before every
/// `check_every`th row (tests run out of budget at a chosen row).
fn stage_retained_rows_checking(
    path: &Path,
    cutoff: i64,
    resume: Option<&StagingResume>,
    check_every: u64,
    out_of_time: &mut dyn FnMut() -> bool,
) -> io::Result<Staging> {
    let recorded = resume.and_then(|resume| resume.temporary_path(path));
    let discard_recorded = || {
        if let Some(recorded) = &recorded {
            remove_recorded_temporary(recorded);
        }
    };
    let done = |result: UnattendedTrim, resumed_from_bytes: u64| -> io::Result<Staging> {
        Ok(Staging {
            result: Err(result),
            resumed_from_bytes,
        })
    };
    let finished = |outcome: MetricsTrim| -> io::Result<Staging> {
        discard_recorded();
        done(
            UnattendedTrim::Finished {
                outcome,
                rows: RowCounts::default(),
            },
            0,
        )
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
    let resumed = match resume {
        Some(resume) => resume_staged_temporary(path, &mut source, &opened, cutoff, resume)?,
        None => None,
    };
    let (mut temporary, mut progress) = match resumed {
        Some(resumed) => resumed,
        None => {
            discard_recorded();
            // `with_synced_temporary`'s naming, so a temporary left by a
            // stopped trim of either kind is recognised and removed by the
            // next one.
            let temporary = tempfile::Builder::new()
                .prefix(TEMPORARY_PREFIX)
                .rand_bytes(TEMPORARY_RANDOM_LEN)
                .tempfile_in(directory)?;
            crate::host_state::protect_host_state_path(temporary.path(), false)?;
            (temporary, StagingProgress::default())
        }
    };
    let resumed_from_bytes = progress.staged_len;
    let mut exhausted = false;
    {
        source.seek(SeekFrom::Start(progress.staged_len))?;
        let unread = opened.len().saturating_sub(progress.staged_len);
        let mut reader =
            BufReader::with_capacity(STAGE_BUFFER_BYTES, Read::take(&mut source, unread));
        let file = temporary.as_file_mut();
        file.seek(SeekFrom::Start(progress.staged_after))?;
        let mut writer = BufWriter::with_capacity(STAGE_BUFFER_BYTES, file);
        let mut line = Vec::new();
        let mut rows_until_deadline_check = 0_u64;
        loop {
            if rows_until_deadline_check == 0 {
                if out_of_time() {
                    exhausted = true;
                    break;
                }
                rows_until_deadline_check = check_every.max(1);
            }
            rows_until_deadline_check -= 1;
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 || line.last() != Some(&b'\n') {
                break;
            }
            progress.staged_len = progress.staged_len.saturating_add(read as u64);
            if progress.rows.keep(&line, cutoff, &mut progress.newest) {
                writer.write_all(&line)?;
                progress.staged_after = progress.staged_after.saturating_add(read as u64);
            }
        }
        writer.flush()?;
    }
    if exhausted {
        if progress.staged_len == 0 {
            // Nothing staged: dropping the temporary removes it.
            return done(
                UnattendedTrim::BudgetExhausted {
                    bytes_scanned: 0,
                    resume: None,
                },
                resumed_from_bytes,
            );
        }
        // Synced before it is recorded, so a record never describes bytes
        // a crash could lose.
        temporary.as_file().sync_all()?;
        let fingerprint = source_fingerprint(&mut source, progress.staged_len)?;
        let (device, inode) = file_identity(&opened).ok_or_else(|| {
            io::Error::new(io::ErrorKind::Unsupported, "the log has no inode number")
        })?;
        let name = temporary
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        temporary.keep().map_err(|error| error.error)?;
        return done(
            UnattendedTrim::BudgetExhausted {
                bytes_scanned: progress.staged_len,
                resume: Some(Box::new(StagingResume {
                    device,
                    inode,
                    fingerprint,
                    source_offset: progress.staged_len,
                    staged_bytes: progress.staged_after,
                    removed: progress.rows.removed,
                    kept: progress.rows.kept,
                    newest: progress.newest,
                    cutoff,
                    temporary: name,
                    recorded_at_ms: crate::clock::epoch_ms(),
                })),
            },
            resumed_from_bytes,
        );
    }
    let rows = progress.rows;
    if rows.removed == 0 {
        // Dropping the temporary removes it; the log is left as it is.
        return done(
            UnattendedTrim::Finished {
                outcome: MetricsTrim::NothingExpired {
                    attempts: 1,
                    stale_temporaries_removed: 0,
                },
                rows,
            },
            resumed_from_bytes,
        );
    }
    if let Some(newest_row_ts) = progress.newest.filter(|newest| *newest < cutoff) {
        return done(
            UnattendedTrim::ClockSuspect {
                cutoff,
                newest_row_ts,
                rows,
            },
            resumed_from_bytes,
        );
    }
    temporary.as_file().sync_all()?;
    Ok(Staging {
        result: Ok(StagedLog {
            temporary,
            _source: source,
            source: opened,
            staged_len: progress.staged_len,
            staged_after: progress.staged_after,
            rows,
        }),
        resumed_from_bytes,
    })
}

/// `resume`'s temporary and progress, when the record still matches the log
/// staging opened (`opened`, read through `source`) and the temporary.
fn resume_staged_temporary(
    path: &Path,
    source: &mut File,
    opened: &std::fs::Metadata,
    cutoff: i64,
    resume: &StagingResume,
) -> io::Result<Option<(tempfile::NamedTempFile, StagingProgress)>> {
    let Some(temporary_path) = resume.temporary_path(path) else {
        return Ok(None);
    };
    // Rows staged under a later cutoff than this one may have been removed
    // where this cutoff keeps them.
    if resume.cutoff > cutoff || resume.source_offset == 0 || !resume.describes(opened) {
        return Ok(None);
    }
    if source_fingerprint(source, resume.source_offset)? != resume.fingerprint {
        return Ok(None);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let Ok(file) = options.open(&temporary_path) else {
        return Ok(None);
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != resume.staged_bytes {
        return Ok(None);
    }
    // Touched, so a sweep that judges temporaries by age sees it in use.
    let _ = file.set_modified(std::time::SystemTime::now());
    let temporary = tempfile::NamedTempFile::from_parts(
        file,
        tempfile::TempPath::try_from_path(temporary_path)?,
    );
    Ok(Some((
        temporary,
        StagingProgress {
            staged_len: resume.source_offset,
            staged_after: resume.staged_bytes,
            rows: RowCounts {
                removed: resume.removed,
                kept: resume.kept,
            },
            newest: resume.newest,
        },
    )))
}

/// Remove a recorded temporary that will not be resumed. Only a regular
/// file is removed; anything else is left to the stale-temporary sweep.
fn remove_recorded_temporary(path: &Path) {
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        let _ = std::fs::remove_file(path);
    }
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
    // What staging found, reported with an outcome that removed nothing.
    let staged_rows = staged.rows;
    let unchanged = |outcome: MetricsTrim| UnattendedTrim::Finished {
        outcome,
        rows: staged_rows,
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
            if tail_rows.keep(line, cutoff, &mut None) {
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
/// `resumed_from_bytes` is the log offset staging resumed from (0 when it
/// started from the first byte).
fn record_unattended_trim(
    result: &io::Result<UnattendedTrim>,
    resumed_from_bytes: u64,
    elapsed: Duration,
) {
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let add = |name| crate::phase_telemetry::add(name, Duration::ZERO);
    let mut fields = serde_json::Map::new();
    fields.insert("elapsed_ms".into(), elapsed_ms.into());
    fields.insert("resumed_from_bytes".into(), resumed_from_bytes.into());
    if resumed_from_bytes > 0 {
        add("command_metrics_trim_resumed");
    }
    let outcome = match result {
        Err(_) => {
            add("command_metrics_trim_failed");
            "failed"
        }
        Ok(UnattendedTrim::BudgetExhausted {
            bytes_scanned,
            resume,
        }) => {
            add("command_metrics_trim_budget_exhausted");
            fields.insert("bytes_scanned".into(), (*bytes_scanned).into());
            fields.insert("resume_recorded".into(), resume.is_some().into());
            "budget_exhausted"
        }
        Ok(UnattendedTrim::ClockSuspect {
            cutoff,
            newest_row_ts,
            rows,
        }) => {
            add("command_metrics_trim_clock_suspect");
            fields.insert("rows_expired".into(), rows.removed.into());
            fields.insert("rows_kept".into(), rows.kept.into());
            fields.insert("rows_removed".into(), 0_u64.into());
            fields.insert("cutoff_ms".into(), (*cutoff).into());
            fields.insert("newest_row_ts".into(), (*newest_row_ts).into());
            "clock_suspect"
        }
        Ok(UnattendedTrim::Unsupported) => {
            add("command_metrics_trim_unsupported");
            "unsupported"
        }
        Ok(UnattendedTrim::Finished { outcome, rows }) => {
            let trimmed = matches!(outcome, MetricsTrim::Trimmed { .. });
            fields.insert("rows_expired".into(), rows.removed.into());
            fields.insert("rows_kept".into(), rows.kept.into());
            let removed = if trimmed { rows.removed } else { 0 };
            fields.insert("rows_removed".into(), removed.into());
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
                MetricsTrim::Missing => {
                    add("command_metrics_trim_missing");
                    "missing"
                }
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
    /// when an unattended trim is due (at most daily; on the next run after
    /// staging ran out of budget with its progress recorded; 30 minutes
    /// after any other outcome that did not finish). The cutoff is the one a
    /// GC plan made today would review. Staging stops by `deadline` (the
    /// calling run's) or after [`UNATTENDED_TRIM_BUDGET`]; with less than
    /// [`UNATTENDED_TRIM_MIN_BUDGET`] left the trim waits for a later run.
    /// Best effort: a failure is warned about and retried.
    pub(crate) fn trim_command_metrics_log_if_due(&self, deadline: Instant) {
        let store = self.store_ref();
        if store.is_read_only() {
            return;
        }
        let now_ms = crate::clock::epoch_ms();
        let recorded_next_due = store.meta_get(UNATTENDED_TRIM_NEXT_DUE_KEY).ok().flatten();
        let next_due = recorded_next_due
            .as_deref()
            .and_then(|value| value.parse().ok());
        if !unattended_trim_due(next_due, now_ms) {
            crate::phase_telemetry::add("command_metrics_trim_not_due", Duration::ZERO);
            return;
        }
        let budget = deadline
            .saturating_duration_since(Instant::now())
            .min(UNATTENDED_TRIM_BUDGET);
        if budget < UNATTENDED_TRIM_MIN_BUDGET {
            // Due, but not this run: the next due time is left as it is.
            crate::phase_telemetry::add("command_metrics_trim_no_time_this_run", Duration::ZERO);
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
        // Claimed before the trim, atomically, so a worker that stops
        // mid-trim, or a second worker meanwhile, does not start another at
        // once: two would otherwise resume into the same temporary.
        let claimed = store.meta_compare_and_set(
            UNATTENDED_TRIM_NEXT_DUE_KEY,
            recorded_next_due.as_deref(),
            &now_ms.saturating_add(UNATTENDED_TRIM_RETRY_MS).to_string(),
        );
        match claimed {
            Ok(true) => {}
            Ok(false) => {
                crate::phase_telemetry::add(
                    "command_metrics_trim_claimed_elsewhere",
                    Duration::ZERO,
                );
                return;
            }
            Err(error) => {
                crate::warn_unrecorded::<(), _>("claim the command metrics trim", Err(error));
                return;
            }
        }
        let _span = crate::phase_telemetry::span("command_metrics_trim");
        let main_root = self.main_root();
        let path = main_root.join(crate::retention::COMMAND_METRICS_LOG);
        let cutoff = super::cutoff(super::utc_day_floor_ms(now_ms), policy.command_metrics_days);
        let recorded_resume = store
            .meta_get(UNATTENDED_TRIM_RESUME_KEY)
            .ok()
            .flatten()
            .filter(|value| !value.is_empty());
        // A record older than a day is not resumed: its temporary goes and
        // staging starts over (the outcome below clears or replaces it).
        let resume = recorded_resume
            .as_deref()
            .and_then(StagingResume::parse)
            .filter(|resume| !expire_resume_if_old(resume, &path, now_ms));
        let started = Instant::now();
        let (result, resumed_from_bytes) = match trim_expired_rows_resuming(
            main_root,
            &path,
            cutoff,
            started + budget,
            resume.as_ref(),
        ) {
            Ok((trim, resumed_from_bytes)) => (Ok(trim), resumed_from_bytes),
            Err(error) => (Err(error), 0),
        };
        record_unattended_trim(&result, resumed_from_bytes, started.elapsed());
        // A new resume record replaces the old one; any other outcome
        // clears it (its temporary was resumed, published or removed).
        match &result {
            Ok(UnattendedTrim::BudgetExhausted {
                resume: Some(next), ..
            }) => crate::warn_unrecorded(
                "record where the command metrics trim resumes",
                store.meta_set(UNATTENDED_TRIM_RESUME_KEY, &next.to_json()),
            ),
            _ if recorded_resume.is_some() => crate::warn_unrecorded(
                "clear where the command metrics trim resumes",
                store.meta_set(UNATTENDED_TRIM_RESUME_KEY, ""),
            ),
            _ => {}
        }
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
/// except `keep` (the calling trim's own staged temporary, or an unattended
/// trim's resumable one whose record still matches), and any modified
/// within [`STALE_TEMPORARY_MIN_AGE`] (an unattended trim may be staging it
/// without the lock; a modification time in the future counts as recent).
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
        if !is_temporary_name(&name)
            || !entry.file_type().is_ok_and(|kind| kind.is_file())
            || keep.is_some_and(|keep| keep == entry.path().as_path())
            || !temporary_is_stale(&entry)
        {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Whether `name` is exactly a trim temporary's: the prefix and
/// [`TEMPORARY_RANDOM_LEN`] alphanumeric characters.
fn is_temporary_name(name: &str) -> bool {
    name.strip_prefix(TEMPORARY_PREFIX).is_some_and(|random| {
        random.len() == TEMPORARY_RANDOM_LEN
            && random.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
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

/// Whether a trim temporary was last modified at least
/// [`STALE_TEMPORARY_MIN_AGE`] ago. An unreadable time is not stale.
fn temporary_is_stale(entry: &std::fs::DirEntry) -> bool {
    entry
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= STALE_TEMPORARY_MIN_AGE)
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

/// [`append_command_metric_line`] with a shorter bound on the lock wait, for
/// a writer that has its own deadline (an interrupted command flushing its
/// row before it dies).
pub(crate) fn append_command_metric_line_within(
    main_root: &Path,
    path: &Path,
    line: &[u8],
    wait: Duration,
) -> bool {
    append_command_metric_line_waiting(main_root, path, line, wait.min(APPEND_LOCK_WAIT))
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
        let result = trim_expired_rows_with(root, path, 10, 3, Duration::ZERO, None, at);
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
        let result = trim_expired_rows_with(root.path(), &path, 10, 3, settle, None, &mut at);
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
            age(file);
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

    /// Make `file` look last written an hour ago.
    fn age(file: &Path) {
        let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3_600);
        let handle = OpenOptions::new().write(true).open(file).unwrap();
        handle.set_modified(hour_ago).unwrap();
    }

    #[cfg(unix)]
    fn far_deadline() -> Instant {
        Instant::now() + Duration::from_secs(600)
    }

    /// Stage, run `between` as a writer, then publish with no settle period.
    #[cfg(unix)]
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

    #[cfg(unix)]
    fn trimmed_rows(outcome: &UnattendedTrim) -> RowCounts {
        match outcome {
            UnattendedTrim::Finished {
                outcome: MetricsTrim::Trimmed { .. },
                rows,
            } => *rows,
            other => panic!("expected a trim, got {other:?}"),
        }
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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
                        removed: 2,
                        kept: 1
                    },
                }
            ),
            "{outcome:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), replaced);
        let entries = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(entries, 1, "the abandoned staging is removed");
    }

    #[cfg(unix)]
    #[test]
    fn an_exhausted_staging_budget_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));

        let outcome = trim_expired_rows_unattended(root.path(), &path, 10, Instant::now());

        assert_eq!(
            outcome.unwrap(),
            UnattendedTrim::BudgetExhausted {
                bytes_scanned: 0,
                resume: None,
            }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{OLD}{KEPT}")
        );
        let entries = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(entries, 1, "no temporary is left behind");
    }

    #[cfg(unix)]
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
        let exhausted = UnattendedTrim::BudgetExhausted {
            bytes_scanned: 0,
            resume: None,
        };
        assert_eq!(exhausted.next_due_in_ms(), UNATTENDED_TRIM_RETRY_MS);
        let resumable = UnattendedTrim::BudgetExhausted {
            bytes_scanned: 1,
            resume: Some(Box::new(StagingResume {
                device: 1,
                inode: 2,
                fingerprint: String::new(),
                source_offset: 1,
                staged_bytes: 0,
                removed: 1,
                kept: 0,
                newest: None,
                cutoff: 10,
                temporary: ".command-metrics.jsonl-a1B2c3".into(),
                recorded_at_ms: 0,
            })),
        };
        assert_eq!(
            resumable.next_due_in_ms(),
            UNATTENDED_TRIM_RESUME_MS,
            "kept progress is resumed on the next run"
        );
    }

    #[test]
    fn a_trim_keeps_its_own_staged_temporary_but_removes_stale_ones() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), KEPT);
        let directory = path.parent().unwrap();
        let own = directory.join(".command-metrics.jsonl-Own123");
        let stale = directory.join(".command-metrics.jsonl-Old456");
        let young = directory.join(".command-metrics.jsonl-New789");
        std::fs::write(&own, "staged").unwrap();
        std::fs::write(&stale, "partial").unwrap();
        std::fs::write(&young, "staging").unwrap();
        age(&own);
        age(&stale);

        assert_eq!(remove_stale_temporaries(&path, Some(own.as_path())), 1);
        assert!(own.exists());
        assert!(!stale.exists());
        assert!(
            young.exists(),
            "a recently written temporary may be another trim's staging"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_log_whose_newest_row_is_older_than_the_cutoff_is_not_trimmed() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{MALFORMED}{OLD}"));

        let outcome = trim_expired_rows_unattended(root.path(), &path, 10, far_deadline());

        assert_eq!(
            outcome.unwrap(),
            UnattendedTrim::ClockSuspect {
                cutoff: 10,
                newest_row_ts: 1,
                rows: RowCounts {
                    removed: 2,
                    kept: 1
                },
            }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{OLD}{MALFORMED}{OLD}")
        );
        let entries = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(entries, 1, "no temporary is left behind");
    }

    /// Stage from `resume` under `cutoff`, running out of budget once
    /// `budget_rows` rows were read in this attempt (never with `None`).
    #[cfg(unix)]
    fn stage_rows(
        path: &Path,
        cutoff: i64,
        resume: Option<&StagingResume>,
        budget_rows: Option<u64>,
    ) -> Staging {
        let mut checks = 0_u64;
        stage_retained_rows_checking(path, cutoff, resume, 1, &mut || {
            let out = budget_rows.is_some_and(|rows| checks >= rows);
            checks += 1;
            out
        })
        .unwrap()
    }

    /// The resume record of a staging that ran out of budget.
    #[cfg(unix)]
    fn exhausted(staging: Staging) -> StagingResume {
        match staging.result {
            Err(UnattendedTrim::BudgetExhausted {
                bytes_scanned,
                resume: Some(resume),
            }) => {
                assert_eq!(bytes_scanned, resume.source_offset);
                *resume
            }
            Err(other) => panic!("expected an exhausted budget, got {other:?}"),
            Ok(_) => panic!("expected an exhausted budget, got a staged log"),
        }
    }

    /// Publish a staging that finished, with no settle period.
    #[cfg(unix)]
    fn publish(root: &Path, path: &Path, staging: Staging) -> UnattendedTrim {
        match staging.result {
            Ok(staged) => {
                publish_staged_rows(root, path, 10, staged, 3, Duration::ZERO, &mut |_| {}).unwrap()
            }
            Err(done) => panic!("expected a staged log, got {done:?}"),
        }
    }

    /// What a one-shot unattended trim leaves of `content`.
    #[cfg(unix)]
    fn one_shot(content: &str) -> (String, RowCounts) {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), content);
        let outcome = trim_expired_rows_unattended(root.path(), &path, 10, far_deadline());
        let rows = trimmed_rows(&outcome.unwrap());
        (std::fs::read_to_string(&path).unwrap(), rows)
    }

    #[cfg(unix)]
    fn temporaries(path: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| is_temporary_name(name))
            .collect();
        names.sort();
        names
    }

    #[cfg(unix)]
    #[test]
    fn exhausted_staging_resumes_and_publishes_what_a_one_shot_trim_would() {
        let root = tempfile::tempdir().unwrap();
        let first = format!("{OLD}{KEPT}{OLD}{MALFORMED}");
        let path = log(root.path(), &first);

        let resume = exhausted(stage_rows(&path, 10, None, Some(2)));
        assert_eq!(resume.source_offset, (OLD.len() + KEPT.len()) as u64);
        assert_eq!(resume.staged_bytes, KEPT.len() as u64);
        assert_eq!((resume.removed, resume.kept), (1, 1));
        assert_eq!(resume.newest, Some(50));
        assert_eq!(temporaries(&path), vec![resume.temporary.clone()]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first, "unchanged");

        // Writers append between attempts; the next attempt continues.
        append(&path, OLD);
        append(&path, LATE);
        let second = stage_rows(&path, 10, Some(&resume), Some(2));
        assert_eq!(second.resumed_from_bytes, resume.source_offset);
        let resume_again = exhausted(second);
        assert_eq!(resume_again.temporary, resume.temporary);
        assert_eq!(
            resume_again.source_offset,
            resume.source_offset + (OLD.len() + MALFORMED.len()) as u64
        );

        // A later cutoff resumes rows staged under an earlier one.
        append(&path, LATER);
        let last = stage_rows(&path, 11, Some(&resume_again), None);
        assert_eq!(last.resumed_from_bytes, resume_again.source_offset);
        let outcome = publish(root.path(), &path, last);

        let content = format!("{first}{OLD}{LATE}{LATER}");
        let (expected, rows) = one_shot(&content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        assert_eq!(trimmed_rows(&outcome), rows);
        assert!(
            temporaries(&path).is_empty(),
            "the temporary became the log"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_log_replaced_or_rewritten_between_attempts_restarts_staging() {
        for rewrite_in_place in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = log(root.path(), &format!("{OLD}{KEPT}{OLD}"));
            let resume = exhausted(stage_rows(&path, 10, None, Some(2)));
            // Same length and longer, so only the identity or the
            // fingerprint tells the logs apart.
            let replaced = format!("{KEPT}{OLD}{OLD}{LATE}");
            if rewrite_in_place {
                std::fs::write(&path, &replaced).unwrap();
            } else {
                let other = path.with_file_name("other");
                std::fs::write(&other, &replaced).unwrap();
                std::fs::rename(&other, &path).unwrap();
            }

            let staging = stage_rows(&path, 10, Some(&resume), None);

            assert_eq!(
                staging.resumed_from_bytes, 0,
                "in place: {rewrite_in_place}"
            );
            assert!(
                !path.with_file_name(&resume.temporary).exists(),
                "the recorded temporary is removed"
            );
            let outcome = publish(root.path(), &path, staging);
            let (expected, rows) = one_shot(&replaced);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
            assert_eq!(trimmed_rows(&outcome), rows);
            assert!(temporaries(&path).is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_truncated_or_missing_temporary_or_an_earlier_cutoff_restarts_staging() {
        enum Damage {
            Truncated,
            Extended,
            Missing,
            EarlierCutoff,
        }
        for damage in [
            Damage::Truncated,
            Damage::Extended,
            Damage::Missing,
            Damage::EarlierCutoff,
        ] {
            let root = tempfile::tempdir().unwrap();
            let content = format!("{OLD}{KEPT}{OLD}{LATE}");
            let path = log(root.path(), &content);
            let resume = exhausted(stage_rows(&path, 10, None, Some(2)));
            let temporary = path.with_file_name(&resume.temporary);
            let mut cutoff = 10;
            match damage {
                Damage::Truncated => {
                    let file = OpenOptions::new().write(true).open(&temporary).unwrap();
                    file.set_len(resume.staged_bytes - 1).unwrap();
                }
                Damage::Extended => append(&temporary, LATE),
                Damage::Missing => std::fs::remove_file(&temporary).unwrap(),
                Damage::EarlierCutoff => cutoff = 9,
            }

            let staging = stage_rows(&path, cutoff, Some(&resume), None);

            assert_eq!(staging.resumed_from_bytes, 0);
            assert!(!temporary.exists(), "the recorded temporary is removed");
            let outcome = match staging.result {
                Ok(staged) => publish_staged_rows(
                    root.path(),
                    &path,
                    cutoff,
                    staged,
                    3,
                    Duration::ZERO,
                    &mut |_| {},
                )
                .unwrap(),
                Err(done) => panic!("expected a staged log, got {done:?}"),
            };
            let (expected, rows) = one_shot(&content);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
            assert_eq!(trimmed_rows(&outcome), rows);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_resume_that_finishes_without_publishing_removes_its_temporary() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{OLD}{MALFORMED}"));
        let resume = exhausted(stage_rows(&path, 10, None, Some(1)));

        // Every timestamped row is expired: the clock check keeps the log.
        let staging = stage_rows(&path, 10, Some(&resume), None);

        assert_eq!(staging.resumed_from_bytes, resume.source_offset);
        assert!(matches!(
            staging.result,
            Err(UnattendedTrim::ClockSuspect {
                newest_row_ts: 1,
                rows: RowCounts {
                    removed: 2,
                    kept: 1
                },
                ..
            })
        ));
        assert!(temporaries(&path).is_empty());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{OLD}{OLD}{MALFORMED}")
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_temporary_sweeps_spare_a_resumable_temporary_while_its_record_matches() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}{OLD}{MALFORMED}"));
        let resume = exhausted(stage_rows(&path, 10, None, Some(2)));
        let temporary = path.with_file_name(&resume.temporary);
        let stale = path.with_file_name(".command-metrics.jsonl-Old456");
        std::fs::write(&stale, "partial").unwrap();
        age(&temporary);
        age(&stale);

        let spare = recorded_temporary_if_valid(&resume, &path);
        assert_eq!(spare.as_deref(), Some(temporary.as_path()));
        // A reviewed trim's sweep keeps it and removes the stale one.
        let outcome = trim_expired_rows_with(
            root.path(),
            &path,
            10,
            3,
            Duration::ZERO,
            spare.as_deref(),
            &mut |_| {},
        )
        .unwrap();
        assert!(
            matches!(
                outcome,
                MetricsTrim::Trimmed {
                    stale_temporaries_removed: 1,
                    ..
                }
            ),
            "{outcome:?}"
        );
        assert!(temporary.exists());
        assert!(!stale.exists());

        // That trim replaced the log, so the record no longer matches: the
        // temporary is no longer spared, and the next unattended attempt
        // starts over and removes it.
        assert_eq!(recorded_temporary_if_valid(&resume, &path), None);
        let staging = stage_rows(&path, 10, Some(&resume), None);
        assert_eq!(staging.resumed_from_bytes, 0);
        assert!(!temporary.exists());
        assert!(temporaries(&path).is_empty());
    }

    /// A resume record spares its temporary for a day after it was written;
    /// past that (or with no time recorded) the record is cleared, only if
    /// unchanged, and the temporary removed, so a temporary is never spared
    /// indefinitely once unattended trims stop.
    #[cfg(unix)]
    #[test]
    fn a_resume_record_spares_its_temporary_for_a_day_only() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        git_repository(&root);
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&root).unwrap();
        let path = log(broker.main_root(), &format!("{OLD}{KEPT}{OLD}{MALFORMED}"));
        let mut resume = exhausted(stage_rows(&path, 10, None, Some(2)));
        let temporary = path.with_file_name(&resume.temporary);
        let now = crate::clock::epoch_ms();
        assert!(now - resume.recorded_at_ms < 60_000, "dated when written");

        // Pure expiry: a day after it was written, a legacy record without a
        // time, but not a record dated ahead of the clock.
        assert!(!resume.expired(resume.recorded_at_ms + RESUME_SPARE_MAX_AGE_MS - 1));
        assert!(resume.expired(resume.recorded_at_ms + RESUME_SPARE_MAX_AGE_MS));
        assert!(!resume.expired(resume.recorded_at_ms - 1));
        let legacy = StagingResume::parse(
            &serde_json::to_string(&serde_json::json!({
                "device": 1, "inode": 2, "fingerprint": "", "source_offset": 1,
                "staged_bytes": 0, "removed": 0, "kept": 0, "newest": null,
                "cutoff": 10, "temporary": ".command-metrics.jsonl-L3gacy",
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(legacy.recorded_at_ms, 0);
        assert!(legacy.expired(now));

        // Fresh: spared, nothing removed.
        broker
            .store_ref()
            .meta_set(UNATTENDED_TRIM_RESUME_KEY, &resume.to_json())
            .unwrap();
        assert_eq!(
            resumable_temporary(broker.store_ref(), &path).as_deref(),
            Some(temporary.as_path())
        );
        assert!(temporary.exists());

        // A day old but its temporary just written: not spared, the record
        // cleared, but the temporary stays for the stale-temporary sweep
        // (a trim may be staging into it).
        resume.recorded_at_ms = now - RESUME_SPARE_MAX_AGE_MS - 1;
        broker
            .store_ref()
            .meta_set(UNATTENDED_TRIM_RESUME_KEY, &resume.to_json())
            .unwrap();
        assert_eq!(resumable_temporary(broker.store_ref(), &path), None);
        assert!(temporary.exists(), "a recently modified temporary stays");

        // A day old and quiet for the guard: the temporary removed, the
        // record cleared.
        std::fs::File::options()
            .write(true)
            .open(&temporary)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - STALE_TEMPORARY_MIN_AGE)
            .unwrap();
        broker
            .store_ref()
            .meta_set(UNATTENDED_TRIM_RESUME_KEY, &resume.to_json())
            .unwrap();
        assert_eq!(resumable_temporary(broker.store_ref(), &path), None);
        assert!(!temporary.exists(), "the expired temporary is removed");
        assert_eq!(
            broker
                .store_ref()
                .meta_get(UNATTENDED_TRIM_RESUME_KEY)
                .unwrap()
                .as_deref(),
            Some("")
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn the_unattended_trim_does_not_run_without_inode_numbers() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let deadline = Instant::now() + Duration::from_secs(600);

        let outcome = trim_expired_rows_unattended(root.path(), &path, 10, deadline).unwrap();

        assert_eq!(outcome, UnattendedTrim::Unsupported);
        assert_eq!(outcome.next_due_in_ms(), UNATTENDED_TRIM_INTERVAL_MS);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{OLD}{KEPT}")
        );
    }

    #[cfg(unix)]
    fn git_repository(root: &Path) {
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Trim test",
                    "-c",
                    "user.email=trim@example.invalid",
                ])
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&["add", "README.md"]);
        git(&["commit", "-qm", "fixture"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_broker_trims_to_command_metrics_days_and_schedules_the_next_trim() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        git_repository(&root);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\ncommand_metrics_days = 7\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&root).unwrap();
        let main_root = broker.main_root().to_path_buf();
        let path = main_root.join(crate::retention::COMMAND_METRICS_LOG);
        let before_ms = crate::clock::epoch_ms();
        let cutoff = super::super::cutoff(super::super::utc_day_floor_ms(before_ms), 7);
        let row = |ts: i64, command: &str| format!("{{\"ts\":{ts},\"command\":\"{command}\"}}\n");
        let outside = row(cutoff - 1, "outside");
        let inside = row(cutoff, "inside");
        let fresh = row(before_ms, "fresh");
        let content = format!("{outside}{inside}{fresh}");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &content).unwrap();
        let next_due = |broker: &crate::Broker| -> Option<i64> {
            broker
                .store_ref()
                .meta_get(UNATTENDED_TRIM_NEXT_DUE_KEY)
                .unwrap()
                .map(|value| value.parse().unwrap())
        };
        let deadline = || Instant::now() + Duration::from_secs(120);

        // A read-only store neither trims nor schedules.
        let connection = broker.store_ref().connection();
        connection.pragma_update(None, "query_only", true).unwrap();
        assert!(broker.store_ref().is_read_only());
        broker.trim_command_metrics_log_if_due(deadline());
        connection.pragma_update(None, "query_only", false).unwrap();
        assert!(!broker.store_ref().is_read_only());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        assert_eq!(next_due(&broker), None);

        // No time left in the calling run: due, but not run, not scheduled.
        broker.trim_command_metrics_log_if_due(Instant::now());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        assert_eq!(next_due(&broker), None);

        // A resume record that no longer matches the log (another inode) is
        // not resumed: its temporary is removed and the record cleared.
        let orphan = path.with_file_name(".command-metrics.jsonl-Orph4n");
        std::fs::write(&orphan, "staged").unwrap();
        let stale_resume = StagingResume {
            device: 0,
            inode: 0,
            fingerprint: String::new(),
            source_offset: 1,
            staged_bytes: 6,
            removed: 1,
            kept: 0,
            newest: None,
            cutoff,
            temporary: ".command-metrics.jsonl-Orph4n".into(),
            recorded_at_ms: crate::clock::epoch_ms(),
        };
        broker
            .store_ref()
            .meta_set(UNATTENDED_TRIM_RESUME_KEY, &stale_resume.to_json())
            .unwrap();

        broker.trim_command_metrics_log_if_due(deadline());
        assert!(!orphan.exists(), "the unmatched temporary is removed");
        assert_eq!(
            broker
                .store_ref()
                .meta_get(UNATTENDED_TRIM_RESUME_KEY)
                .unwrap()
                .as_deref(),
            Some(""),
            "a finished trim clears the resume record"
        );
        let after_ms = crate::clock::epoch_ms();
        if super::super::utc_day_floor_ms(after_ms) != super::super::utc_day_floor_ms(before_ms) {
            // The UTC day turned during the test and moved the cutoff.
            return;
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{inside}{fresh}"),
            "the row one millisecond outside the window is removed"
        );
        let due = next_due(&broker).expect("the next trim is scheduled");
        assert!(
            (before_ms + UNATTENDED_TRIM_INTERVAL_MS..=after_ms + UNATTENDED_TRIM_INTERVAL_MS)
                .contains(&due),
            "a finished trim schedules the next one a day ahead: {due}"
        );

        // Not due again: a newly expired row stays until the next due time.
        let mut appended = OpenOptions::new().append(true).open(&path).unwrap();
        appended.write_all(outside.as_bytes()).unwrap();
        broker.trim_command_metrics_log_if_due(deadline());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{inside}{fresh}{outside}")
        );
        assert_eq!(next_due(&broker), Some(due));
    }
}
