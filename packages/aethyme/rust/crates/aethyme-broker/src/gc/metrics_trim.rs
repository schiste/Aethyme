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
//! shared around each append; the trim holds it exclusively from its read to
//! its rename, so a current writer can never append to the inode being
//! replaced. A writer that predates the lock is caught by version checks: an
//! append seen before the rename restarts the attempt, and one that lands in
//! the replaced inode after the rename is copied into the new file. When every
//! attempt sees an append, the trim is skipped. It is housekeeping; losing a
//! row inside retention, or failing a journal over it, would cost more.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::file_lock::open_command_metrics_lock;

use super::{open_regular_file_nonblocking, retained_metric_bytes_until, same_regular_file_version};

/// Whole attempts before an apply gives up on a log that keeps growing.
pub(super) const TRIM_MAX_ATTEMPTS: u32 = 3;
/// How long the trim waits for writers that hold the append lock. Each holds
/// it for one `write`, so this is only reached by a stuck writer.
const TRIM_LOCK_WAIT: Duration = Duration::from_secs(2);
/// How long a writer waits for a trim to finish before appending unlocked,
/// which the trim's post-rename copy still covers.
const APPEND_LOCK_WAIT: Duration = Duration::from_secs(2);
const LOCK_POLL: Duration = Duration::from_millis(10);
/// Passes over the replaced inode for rows a pre-lock writer appended there.
const TAIL_RECOVERY_PASSES: u32 = 4;

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
    },
    /// No row is older than the cutoff any more; the file was not replaced.
    NothingExpired { attempts: u32 },
    /// The log does not exist, so nothing is retained past its cutoff.
    Missing,
    /// Every attempt saw a concurrent append or a busy lock. Nothing was
    /// removed and nothing was lost; the next plan proposes the trim again.
    SkippedConcurrentAppend { attempts: u32 },
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
    trim_expired_rows_with(main_root, path, cutoff, TRIM_MAX_ATTEMPTS, &mut |_| {})
}

pub(super) fn trim_expired_rows_with(
    main_root: &Path,
    path: &Path,
    cutoff: i64,
    max_attempts: u32,
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
    for attempt in 1..=max_attempts {
        let Some(_exclusive) = wait_exclusive(open_command_metrics_lock(main_root)?)? else {
            continue;
        };
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
            return Ok(MetricsTrim::NothingExpired { attempts: attempt });
        }
        let published = crate::atomic_file::with_synced_temporary(path, &after, |temporary| {
            at(TrimPoint::BeforePublish { attempt });
            // A writer that predates the append lock may have written since
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
        let recovered_tail_bytes = copy_late_tail(&mut file, opened.len(), path)?;
        return Ok(MetricsTrim::Trimmed {
            bytes_before: opened.len(),
            bytes_after: (after.len() as u64).saturating_add(recovered_tail_bytes),
            attempts: attempt,
            recovered_tail_bytes,
        });
    }
    Ok(MetricsTrim::SkippedConcurrentAppend {
        attempts: max_attempts,
    })
}

/// The append lock held exclusively; closing the returned file releases it.
fn wait_exclusive(file: File) -> io::Result<Option<File>> {
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

/// Copy rows a pre-lock writer appended to the replaced inode after the read
/// into the new file, in order. `copied` is how much of it the trim read.
fn copy_late_tail(replaced: &mut File, mut copied: u64, path: &Path) -> io::Result<u64> {
    let mut recovered = 0_u64;
    for _ in 0..TAIL_RECOVERY_PASSES {
        let length = replaced.metadata()?.len();
        if length <= copied {
            break;
        }
        replaced.seek(SeekFrom::Start(copied))?;
        let mut tail = Vec::new();
        replaced
            .by_ref()
            .take(length - copied)
            .read_to_end(&mut tail)?;
        let mut target = OpenOptions::new().append(true).open(path)?;
        target.write_all(&tail)?;
        copied = copied.saturating_add(tail.len() as u64);
        recovered = recovered.saturating_add(tail.len() as u64);
    }
    Ok(recovered)
}

/// Append one metric row under the shared append lock. Best effort, like the
/// metric itself: a lock that cannot be opened or stays busy past
/// [`APPEND_LOCK_WAIT`] degrades to the unlocked append older binaries do.
pub(crate) fn append_command_metric_line(main_root: &Path, path: &Path, line: &[u8]) {
    let lock = open_command_metrics_lock(main_root).ok();
    let locked = lock.as_ref().is_some_and(wait_shared);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line);
    }
    if locked && let Some(lock) = &lock {
        let _ = lock.unlock();
    }
}

fn wait_shared(file: &File) -> bool {
    let started = Instant::now();
    loop {
        match file.try_lock_shared() {
            Ok(()) => return true,
            Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < APPEND_LOCK_WAIT => {
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

    #[test]
    fn trim_drops_only_expired_rows_and_keeps_malformed_lines_in_order() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{MALFORMED}{KEPT}"));

        let outcome = trim_expired_rows(root.path(), &path, 10).unwrap();

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
            trim_expired_rows(root.path(), &path, 10).unwrap(),
            MetricsTrim::NothingExpired { attempts: 1 }
        );
    }

    #[test]
    fn an_unlocked_append_before_the_rename_restarts_the_attempt_and_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let writer_path = path.clone();

        let outcome = trim_expired_rows_with(root.path(), &path, 10, 3, &mut |point| {
            if point == (TrimPoint::BeforePublish { attempt: 1 }) {
                append(&writer_path, LATE);
            }
        })
        .unwrap();

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

        let outcome = trim_expired_rows_with(root.path(), &path, 10, 3, &mut |point| match point {
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
        })
        .unwrap();

        assert_eq!(
            outcome,
            MetricsTrim::Trimmed {
                bytes_before: (OLD.len() + KEPT.len()) as u64,
                bytes_after: (KEPT.len() + LATE.len()) as u64,
                attempts: 1,
                recovered_tail_bytes: LATE.len() as u64,
            }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{KEPT}{LATE}")
        );
    }

    #[test]
    fn a_log_that_grows_on_every_attempt_is_skipped_without_losing_a_row() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let writer_path = path.clone();

        let outcome = trim_expired_rows_with(root.path(), &path, 10, 3, &mut |point| {
            if matches!(point, TrimPoint::BeforePublish { .. }) {
                append(&writer_path, LATE);
            }
        })
        .unwrap();

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
    fn a_locked_writer_waits_for_the_trim_and_lands_in_the_new_file() {
        let root = tempfile::tempdir().unwrap();
        let path = log(root.path(), &format!("{OLD}{KEPT}"));
        let main_root = root.path().to_path_buf();
        let writer_path = path.clone();
        let mut writer: Option<std::thread::JoinHandle<()>> = None;

        let outcome = trim_expired_rows_with(root.path(), &path, 10, 3, &mut |point| {
            if point == (TrimPoint::BeforePublish { attempt: 1 }) {
                let (main_root, writer_path) = (main_root.clone(), writer_path.clone());
                let (started, wait) = std::sync::mpsc::channel();
                writer = Some(std::thread::spawn(move || {
                    started.send(()).unwrap();
                    append_command_metric_line(&main_root, &writer_path, LATE.as_bytes());
                }));
                wait.recv().unwrap();
            }
        })
        .unwrap();
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
    fn a_missing_log_or_a_symlink_is_never_replaced() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".aethyme/logs/command-metrics.jsonl");
        assert_eq!(
            trim_expired_rows(root.path(), &path, 10).unwrap(),
            MetricsTrim::Missing
        );
        #[cfg(unix)]
        {
            let target = root.path().join("elsewhere.jsonl");
            std::fs::write(&target, OLD).unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(matches!(
                trim_expired_rows(root.path(), &path, 10).unwrap(),
                MetricsTrim::Refused(_)
            ));
            assert_eq!(std::fs::read_to_string(&target).unwrap(), OLD);
        }
    }
}
