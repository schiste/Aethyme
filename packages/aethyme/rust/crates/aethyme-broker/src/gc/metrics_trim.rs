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

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::file_lock::open_command_metrics_lock;

use super::{
    open_regular_file_nonblocking, retained_metric_bytes_until, same_regular_file_version,
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
        stale_temporaries_removed += remove_stale_temporaries(path);
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
/// in that directory whose name is exactly the trim's temporary pattern.
fn remove_stale_temporaries(path: &Path) -> usize {
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
}
