//! Exclusive advisory file locks, shared by every broker lock file.
//!
//! These are `std::fs::File::lock` locks, which on Unix are `flock(2)`: they
//! belong to the open file description, are advisory, and are released when
//! the last descriptor for that description closes. That is exactly what the
//! hand-written `libc::flock` copies this replaces relied on, so dropping the
//! guard (or the process dying) still releases the lock.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// How long a foreground lifecycle command waits for the claim. A start on
/// Mockup (11,437 tracked files, load about 40) held it for 20-25 s, so a 10 s
/// wait failed 102 starts in one day (2026-11-05), each leaving its agent to
/// retry by hand. This covers about seven such holders queued ahead.
const LIFECYCLE_LOCK_WAIT: Duration = Duration::from_secs(180);
const LIFECYCLE_LOCK_POLL: Duration = Duration::from_millis(50);
/// When a waiting command first says so, and how often after that.
const LIFECYCLE_WAIT_NOTICE_AFTER: Duration = Duration::from_secs(5);
const LIFECYCLE_WAIT_NOTICE_EVERY: Duration = Duration::from_secs(30);

/// Open (creating if needed) a lock file without truncating it.
pub(crate) fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

/// Acquire the cross-process lifecycle mutation claim for the primary repo.
///
/// The persistent lock lives under the primary repository's runtime state,
/// never under a checkout that an operation may remove. All guarded checkout
/// creation, adoption, and removal paths use the same file, so registering a
/// nested checkout cannot race deletion of its parent. The claim is deliberately
/// scoped to checkout creation/adoption/removal; ordinary edits, builds, and
/// submissions do not acquire it. The lock file is never unlinked: doing so
/// while another process holds it would allow a second inode at the same path.
///
/// Waits at most [`LIFECYCLE_LOCK_WAIT`], and never past an active command-scoped
/// Git deadline such as `finish --timeout`.
pub(crate) fn acquire_repository_lifecycle_lock(main_root: &Path) -> io::Result<ExclusiveFileLock> {
    let wait = crate::git::active_git_deadline().map_or(LIFECYCLE_LOCK_WAIT, |deadline| {
        LIFECYCLE_LOCK_WAIT.min(deadline.saturating_duration_since(Instant::now()))
    });
    acquire_repository_lifecycle_lock_with_wait(main_root, wait)
}

/// The command holding the claim, for the first words of its arguments: `broker
/// start`, never the free text or paths that follow them.
fn lifecycle_operation() -> String {
    std::env::args()
        .skip(1)
        .take_while(|word| !word.starts_with('-') && !word.contains(['/', '\\']))
        .take(4)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Say who holds the claim, for the next waiter. Telemetry only: a failed write
/// must not fail the operation that already owns the lock, and the waiter reports
/// an unidentified holder.
fn record_lifecycle_holder(lock: &ExclusiveFileLock) {
    crate::warn_unrecorded(
        "record the worktree lifecycle lock holder",
        lock.record_holder(&format!(
            "pid={} since_ms={} operation={}",
            std::process::id(),
            crate::clock::epoch_ms(),
            lifecycle_operation()
        )),
    );
}

/// The holder a contended lock file names, in words for the person waiting.
///
/// flock has no owner to ask, and the record is written just after locking, so
/// it can briefly name the previous holder, and a binary older than this record
/// writes none. A named process that has exited therefore says that the holder
/// is unidentified, not that the lock is free.
fn describe_lifecycle_holder(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let field = |name: &str| {
        text.split_whitespace()
            .find_map(|word| word.strip_prefix(name))
            .map(str::to_owned)
    };
    let (Some(pid), Some(since_ms)) = (
        field("pid=").and_then(|pid| pid.parse::<i64>().ok()),
        field("since_ms=").and_then(|since| since.parse::<i64>().ok()),
    ) else {
        return "the holder did not record itself (an older aethyme binary?)".into();
    };
    if !crate::blockers::process_alive(pid) {
        return format!(
            "the last recorded holder, pid {pid}, has exited, so the current holder is \
             unidentified (an older aethyme binary?)"
        );
    }
    let operation = text
        .split_once("operation=")
        .map(|(_, operation)| operation.trim())
        .filter(|operation| !operation.is_empty())
        .unwrap_or("unknown operation");
    let held_for = (crate::clock::epoch_ms() - since_ms).max(0) / 1000;
    format!("pid {pid} ({operation}) has held it for {held_for} s and is running")
}

/// Make a nonblocking claim for a checkout lifecycle operation.
///
/// Admission paths use this when queueing behind a remover could outlive the
/// caller's bounded maintenance window. `None` means another lifecycle claim
/// is active; path and inode changes remain errors.
pub(crate) fn try_acquire_repository_lifecycle_lock(
    main_root: &Path,
) -> io::Result<Option<ExclusiveFileLock>> {
    let path = repository_lifecycle_lock_path(main_root)?;
    let file = open_lock_file_nofollow(&path)?;
    let Some(lock) = ExclusiveFileLock::try_acquire(file)? else {
        return Ok(None);
    };
    if repository_lifecycle_lock_path(main_root)? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "broker lifecycle lock path changed while it was acquired",
        ));
    }
    verify_named_lock_file(&path, &lock.file)?;
    record_lifecycle_holder(&lock);
    Ok(Some(lock))
}

fn repository_lifecycle_lock_path(main_root: &Path) -> io::Result<std::path::PathBuf> {
    repository_runtime_lock_path(main_root, "worktree-lifecycle.lock")
}

/// Try to claim one session's compact-recovery writer without blocking other
/// lifecycle operations. The persistent lock is released automatically when
/// the process exits, so an interrupted archive attempt can be reconciled.
pub(crate) fn try_acquire_retirement_archive_lock(
    main_root: &Path,
    session_id: i64,
) -> io::Result<Option<ExclusiveFileLock>> {
    if session_id <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "retirement lock requires a positive session id",
        ));
    }
    let name = format!("checkout-retirement-{session_id}.lock");
    let path = repository_runtime_lock_path(main_root, &name)?;
    let file = open_lock_file_nofollow(&path)?;
    let Some(lock) = ExclusiveFileLock::try_acquire(file)? else {
        return Ok(None);
    };
    if repository_runtime_lock_path(main_root, &name)? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "retirement archive lock path changed while it was acquired",
        ));
    }
    verify_named_lock_file(&path, &lock.file)?;
    Ok(Some(lock))
}

fn repository_runtime_lock_path(main_root: &Path, name: &str) -> io::Result<std::path::PathBuf> {
    Ok(repository_runtime_directory(main_root)?.join(name))
}

/// The primary repository's real `.aethyme/run` directory, created if absent
/// and refused when it is a symlink or escapes the repository.
pub(crate) fn repository_run_directory(main_root: &Path) -> io::Result<std::path::PathBuf> {
    repository_runtime_directory(main_root)
}

/// Which side of the per-session command/retirement fence to acquire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionOperationLockMode {
    Shared,
    Exclusive,
}

/// A per-session operation fence. The shared lock remains close-on-exec so a
/// detached helper cannot pin a session forever. Native artifact, gate, and
/// coordinated-operation witnesses protect children after the broker parent
/// exits; retirement refuses when those inventories are unknown.
#[derive(Debug)]
pub(crate) enum SessionOperationGuard {
    Shared { _lock: SharedSessionOperationLock },
    Exclusive { _lock: ExclusiveFileLock },
}

#[derive(Debug)]
pub(crate) struct SharedSessionOperationLock {
    _file: File,
}

/// Try to serialize one session's admitted work against abandoned retirement.
/// The lock file is persistent and remains outside any worktree the operation
/// may remove. A busy lock is an ordinary retryable refusal; path or lock
/// protocol errors remain errors and must not be interpreted as an idle
/// session.
pub(crate) fn try_acquire_session_operation_guard(
    main_root: &Path,
    session_id: i64,
    mode: SessionOperationLockMode,
) -> io::Result<Option<SessionOperationGuard>> {
    if session_id <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session operation guard requires a positive session ID",
        ));
    }
    let name = format!("session-operation-{session_id}.lock");
    let path = repository_runtime_lock_path(main_root, &name)?;
    let file = open_lock_file_nofollow(&path)?;
    match mode {
        SessionOperationLockMode::Shared => {
            match file.try_lock_shared() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
            if repository_runtime_lock_path(main_root, &name)? != path {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "session operation lock path changed while it was acquired",
                ));
            }
            verify_named_lock_file(&path, &file)?;
            Ok(Some(SessionOperationGuard::Shared {
                _lock: SharedSessionOperationLock { _file: file },
            }))
        }
        SessionOperationLockMode::Exclusive => {
            let Some(lock) = ExclusiveFileLock::try_acquire(file)? else {
                return Ok(None);
            };
            if repository_runtime_lock_path(main_root, &name)? != path {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "session operation lock path changed while it was acquired",
                ));
            }
            verify_named_lock_file(&path, &lock.file)?;
            Ok(Some(SessionOperationGuard::Exclusive { _lock: lock }))
        }
    }
}

/// Whether this host supports the nonblocking per-session advisory lock used
/// by the guarded expiry protocol. Descendants are protected separately by
/// durable producer/process witnesses rather than inheriting this descriptor.
pub(crate) const fn session_operation_guard_supported() -> bool {
    cfg!(unix)
}

fn repository_runtime_directory(main_root: &Path) -> io::Result<std::path::PathBuf> {
    let root = main_root.canonicalize()?;
    let state_dir = root.join(".aethyme");
    ensure_real_directory(&state_dir)?;
    let run_dir = state_dir.join("run");
    ensure_real_directory(&run_dir)?;
    let canonical_run = run_dir.canonicalize()?;
    if canonical_run != run_dir || !canonical_run.starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "broker runtime path is not a real directory under the primary repository: {}",
                run_dir.display()
            ),
        ));
    }

    Ok(canonical_run)
}

/// Claim one complete CLI scheduling pass, including repository watches.
/// The file remains persistent so a second inode cannot split ownership.
pub(crate) fn acquire_pr_watch_tick_lock(main_root: &Path) -> io::Result<ExclusiveFileLock> {
    let path = repository_runtime_lock_path(main_root, "pr-watch-tick.lock")?;
    let file = open_lock_file_nofollow(&path)?;
    let lock = ExclusiveFileLock::try_acquire(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "another PR watch tick is running; retry after it finishes",
        )
    })?;
    if repository_runtime_lock_path(main_root, "pr-watch-tick.lock")? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "broker PR watch lock path changed while it was acquired",
        ));
    }
    verify_named_lock_file(&path, &lock.file)?;
    Ok(lock)
}

/// Serialize one environment's external lifecycle effects. The lock lives
/// outside the removable environment directory and is never unlinked. Process
/// exit releases ownership, so an interrupted destroy can safely resume.
pub(crate) fn try_acquire_environment_operation_lock(
    root: &Path,
    env_id: &str,
) -> io::Result<ExclusiveFileLock> {
    use sha2::{Digest, Sha256};
    std::fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    let directory = root.join("operation-locks");
    ensure_real_directory(&directory)?;
    let name = format!("{:x}.lock", Sha256::digest(env_id.as_bytes()));
    let path = directory.join(name);
    let file = open_lock_file_nofollow(&path)?;
    let lock = ExclusiveFileLock::try_acquire(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("environment {env_id} has a lifecycle operation in progress; retry after it finishes"),
        )
    })?;
    verify_named_lock_file(&path, &lock.file)?;
    Ok(lock)
}

/// Claim the cooperative commit lane for one session without serializing
/// commits from other worktrees through the repository-wide Git write lock.
/// The persistent lock file is never removed, so contenders always lock the
/// same inode. A busy lane refuses immediately; it never queues behind a
/// commit hook that may be waiting for another repository operation.
pub(crate) fn try_acquire_session_commit_lock(
    main_root: &Path,
    session_id: i64,
) -> io::Result<ExclusiveFileLock> {
    if session_id <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session commit lock requires a positive session id",
        ));
    }
    let name = format!("session-commit-{session_id}.lock");
    let path = repository_runtime_lock_path(main_root, &name)?;
    let file = open_lock_file_nofollow(&path)?;
    let lock = ExclusiveFileLock::try_acquire(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("another broker commit is running for session {session_id}"),
        )
    })?;
    if repository_runtime_lock_path(main_root, &name)? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "broker session commit lock path changed while it was acquired",
        ));
    }
    verify_named_lock_file(&path, &lock.file)?;
    Ok(lock)
}

pub(crate) fn acquire_repository_lifecycle_lock_with_wait(
    main_root: &Path,
    wait: Duration,
) -> io::Result<ExclusiveFileLock> {
    let path = repository_lifecycle_lock_path(main_root)?;
    let started = Instant::now();
    let deadline = started + wait;
    let mut next_notice = started + LIFECYCLE_WAIT_NOTICE_AFTER;
    loop {
        let file = open_lock_file_nofollow(&path)?;
        match ExclusiveFileLock::try_acquire(file)? {
            Some(lock) => {
                // Revalidate the primary-root-derived location after acquiring
                // the persistent file. This catches a replaced runtime path or
                // lock name before the claim is used for a destructive action.
                if repository_lifecycle_lock_path(main_root)? != path {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "broker lifecycle lock path changed while it was acquired",
                    ));
                }
                verify_named_lock_file(&path, &lock.file)?;
                record_lifecycle_holder(&lock);
                return Ok(lock);
            }
            None if Instant::now() >= deadline => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "another broker lifecycle operation holds {}: {}; waited {} s, retry this command after it finishes",
                        path.display(),
                        describe_lifecycle_holder(&path),
                        started.elapsed().as_secs(),
                    ),
                ));
            }
            None => {
                let now = Instant::now();
                if now >= next_notice {
                    eprintln!(
                        "waiting for the repository lifecycle lock: {}; waited {} of at most {} s",
                        describe_lifecycle_holder(&path),
                        started.elapsed().as_secs(),
                        wait.as_secs(),
                    );
                    next_notice = now + LIFECYCLE_WAIT_NOTICE_EVERY;
                }
                thread::sleep(LIFECYCLE_LOCK_POLL.min(deadline.saturating_duration_since(now)));
            }
        }
    }
}

fn ensure_real_directory(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "broker runtime path is not a real directory: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.file_type().is_dir() {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "broker runtime path changed while it was created: {}",
                        path.display()
                    ),
                ))
            }
        }
        Err(error) => Err(error),
    }
}
fn open_lock_file_nofollow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself. `verify_named_lock_file` rejects it
        // after open rather than following it to an unrelated lock target.
        options.custom_flags(0x0020_0000);
    }
    options.open(path)
}

fn verify_named_lock_file(path: &Path, opened: &File) -> io::Result<()> {
    let named = std::fs::symlink_metadata(path)?;
    let opened = opened.metadata()?;
    if !named.file_type().is_file() || !opened.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "worktree lifecycle lock is not a regular file: {}",
                path.display()
            ),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if named.dev() != opened.dev()
            || named.ino() != opened.ino()
            || named.nlink() != 1
            || opened.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "worktree lifecycle lock path changed while it was opened: {}",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

/// An exclusive lock held on an open file until the guard drops.
#[derive(Debug)]
pub(crate) struct ExclusiveFileLock {
    file: File,
}

impl ExclusiveFileLock {
    /// Block until `file` is locked exclusively.
    pub(crate) fn acquire(file: File) -> io::Result<Self> {
        file.lock()?;
        Ok(Self { file })
    }

    /// Write who holds the lock into the lock file, so a waiter can name its
    /// holder. Only ever written while the lock is held.
    pub(crate) fn record_holder(&self, holder: &str) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.file.set_len(0)?;
        self.file.write_all_at(holder.as_bytes(), 0)
    }

    /// Lock `file` exclusively if nobody holds it; `None` when it is held.
    pub(crate) fn try_acquire(file: File) -> io::Result<Option<Self>> {
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        // Closing the descriptor would release the lock anyway; unlocking
        // first makes the release independent of any duplicated descriptor.
        let _ = self.file.unlock();
    }
}

/// A shared lock held on an open file until the guard drops.
#[derive(Debug)]
pub(crate) struct SharedFileLock {
    file: File,
}

impl SharedFileLock {
    /// Lock `file` shared without waiting; `None` when an exclusive user holds it.
    pub(crate) fn try_acquire(file: File) -> io::Result<Option<Self>> {
        match file.try_lock_shared() {
            Ok(()) => Ok(Some(Self { file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for SharedFileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[path = "artifact_use.rs"]
pub(crate) mod artifact_use;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_lifecycle_claim_returns_none_without_waiting_when_busy() {
        let repository = tempfile::tempdir().unwrap();
        let held = acquire_repository_lifecycle_lock(repository.path()).unwrap();
        assert!(
            try_acquire_repository_lifecycle_lock(repository.path())
                .unwrap()
                .is_none()
        );
        drop(held);
        assert!(
            try_acquire_repository_lifecycle_lock(repository.path())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn session_operation_guards_are_scoped_to_one_session() {
        let repository = tempfile::tempdir().unwrap();
        let shared = try_acquire_session_operation_guard(
            repository.path(),
            41,
            SessionOperationLockMode::Shared,
        )
        .unwrap()
        .expect("first session operation should be admitted");
        assert!(
            try_acquire_session_operation_guard(
                repository.path(),
                41,
                SessionOperationLockMode::Exclusive,
            )
            .unwrap()
            .is_none(),
            "retirement must wait while an operation is admitted"
        );
        let unrelated = try_acquire_session_operation_guard(
            repository.path(),
            42,
            SessionOperationLockMode::Exclusive,
        )
        .unwrap();
        assert!(unrelated.is_some(), "another session stays independent");
        drop(unrelated);
        drop(shared);
        assert!(
            try_acquire_session_operation_guard(
                repository.path(),
                41,
                SessionOperationLockMode::Exclusive,
            )
            .unwrap()
            .is_some(),
            "exclusive retirement can proceed after the operation ends"
        );
    }

    #[cfg(unix)]
    #[test]
    fn child_does_not_extend_shared_session_operation_after_parent_exits() {
        let repository = tempfile::tempdir().unwrap();
        let shared = try_acquire_session_operation_guard(
            repository.path(),
            41,
            SessionOperationLockMode::Shared,
        )
        .unwrap()
        .expect("operation should be admitted");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf ready; read _"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn child behind a readiness barrier");
        let mut ready = [0_u8; 5];
        std::io::Read::read_exact(child.stdout.as_mut().unwrap(), &mut ready)
            .expect("child reaches its read barrier");
        assert_eq!(&ready, b"ready");

        // Session guards stay close-on-exec. Child lifetime is covered by the
        // operation's durable artifact/gate/journal witnesses, not by leaking
        // the lock into unrelated long-lived helpers.
        drop(shared);
        // Another test thread's fork can hold the description open until its
        // exec closes it, so this claim may miss briefly. The child above stays
        // alive throughout, so one that had inherited the lock never lets go.
        let deadline = Instant::now() + Duration::from_secs(30);
        let exclusive = loop {
            let attempt = try_acquire_session_operation_guard(
                repository.path(),
                41,
                SessionOperationLockMode::Exclusive,
            )
            .unwrap();
            if attempt.is_some() || Instant::now() >= deadline {
                break attempt;
            }
            thread::sleep(Duration::from_millis(5));
        };
        assert!(exclusive.is_some());
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"done\n")
            .expect("release child barrier");
        child.wait().expect("wait for lock-holding child");
    }

    #[test]
    fn pr_watch_claim_excludes_another_tick_and_releases_on_drop() {
        let repository = tempfile::tempdir().unwrap();
        let claim = acquire_pr_watch_tick_lock(repository.path()).unwrap();
        let error = acquire_pr_watch_tick_lock(repository.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        // Checkout lifecycle and watch polling must remain independent.
        let lifecycle = acquire_repository_lifecycle_lock(repository.path()).unwrap();
        drop(lifecycle);
        drop(claim);
        let resumed = acquire_pr_watch_tick_lock(repository.path()).unwrap();
        assert!(
            repository
                .path()
                .join(".aethyme/run/pr-watch-tick.lock")
                .is_file()
        );
        drop(resumed);
    }

    #[cfg(unix)]
    #[test]
    fn pr_watch_claim_refuses_redirected_runtime_and_lock_paths() {
        let repository = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(repository.path().join(".aethyme")).unwrap();
        let run = repository.path().join(".aethyme/run");
        std::os::unix::fs::symlink(outside.path(), &run).unwrap();
        assert!(acquire_pr_watch_tick_lock(repository.path()).is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        std::fs::remove_file(&run).unwrap();
        std::fs::create_dir(&run).unwrap();
        let target = outside.path().join("target");
        std::fs::write(&target, b"preserve").unwrap();
        let path = run.join("pr-watch-tick.lock");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(acquire_pr_watch_tick_lock(repository.path()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve");
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&target, &path).unwrap();
        assert!(acquire_pr_watch_tick_lock(repository.path()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve");
    }

    fn lifecycle_claim_contends_in_order(first: &str, second: &str) {
        let repository = tempfile::tempdir().unwrap();
        let repository_root = repository.path().canonicalize().unwrap();
        let checkout = repository_root.join("linked-worktree");
        std::fs::create_dir(&checkout).unwrap();
        let primary_alias = repository_root.join(".");
        let held_by_first = acquire_repository_lifecycle_lock(&repository_root)
            .unwrap_or_else(|error| panic!("{first} claim failed: {error}"));
        let lock_path = repository_lifecycle_lock_path(&primary_alias).unwrap();
        assert!(
            lock_path.starts_with(repository_root.join(".aethyme/run")),
            "the persistent claim must be rooted in primary repository state"
        );
        assert!(
            !lock_path.starts_with(&checkout),
            "removing a linked checkout must not remove the repository claim"
        );
        let second_file = open_lock_file_nofollow(&lock_path).unwrap();
        assert!(
            matches!(
                second_file.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ),
            "{second} must wait while {first} holds the repository lifecycle claim"
        );
        assert!(lock_path.exists(), "the lock file must remain persistent");

        drop(held_by_first);
        second_file.try_lock().unwrap_or_else(|error| {
            panic!("{second} did not acquire after {first} released: {error}")
        });
    }

    #[test]
    fn a_held_lock_excludes_another_open_until_it_drops() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broker.lock");
        let held = ExclusiveFileLock::acquire(open_lock_file(&path).unwrap()).unwrap();

        // A second open file description contends, as another process would.
        let contender = open_lock_file(&path).unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        contender
            .try_lock()
            .expect("the lock is free once the guard drops");
    }

    #[test]
    fn a_holder_written_into_a_held_lock_is_readable_by_a_waiter() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("owner.lock");
        std::fs::write(&path, b"a much longer previous holder line").unwrap();
        let held = ExclusiveFileLock::acquire(open_lock_file(&path).unwrap()).unwrap();
        held.record_holder("s812 since=1").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "s812 since=1");
    }

    #[test]
    fn opening_a_lock_file_keeps_its_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("slot.lock");
        std::fs::write(&path, b"holder").unwrap();
        let _lock = ExclusiveFileLock::acquire(open_lock_file(&path).unwrap()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"holder");
    }

    #[test]
    fn a_foreground_claim_outlasts_a_holder_past_the_old_ten_second_wait() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let held = acquire_repository_lifecycle_lock(&root).unwrap();
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_secs(11));
            drop(held);
        });
        let started = Instant::now();
        acquire_repository_lifecycle_lock(&root).unwrap();
        assert!(started.elapsed() >= Duration::from_secs(10));
        release.join().unwrap();
    }

    #[test]
    fn a_foreground_claim_never_waits_past_the_commands_git_deadline() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let _held = acquire_repository_lifecycle_lock(&root).unwrap();
        let started = Instant::now();
        let error = crate::git::with_git_deadline(Duration::from_millis(200), || {
            acquire_repository_lifecycle_lock(&root)
        })
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_refusal_names_the_holder_how_long_it_held_and_that_it_runs() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let _held = acquire_repository_lifecycle_lock(&root).unwrap();
        let message =
            acquire_repository_lifecycle_lock_with_wait(&root, Duration::from_millis(100))
                .err()
                .unwrap()
                .to_string();
        assert!(
            message.contains(&format!("pid {} (", std::process::id()))
                && message.contains("has held it for")
                && message.contains("is running")
                && message.contains("retry this command"),
            "{message}"
        );
    }

    #[test]
    fn a_refusal_does_not_blame_an_exited_or_unrecorded_holder() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().canonicalize().unwrap();
        let path = repository_lifecycle_lock_path(&root).unwrap();
        // Held by something that recorded nothing, as an older binary does.
        let held = ExclusiveFileLock::acquire(open_lock_file_nofollow(&path).unwrap()).unwrap();
        let refusal = || {
            acquire_repository_lifecycle_lock_with_wait(&root, Duration::from_millis(100))
                .err()
                .unwrap()
                .to_string()
        };
        let message = refusal();
        assert!(message.contains("did not record itself"), "{message}");

        let mut exited = std::process::Command::new("true").spawn().unwrap();
        let pid = exited.id();
        exited.wait().unwrap();
        held.record_holder(&format!("pid={pid} since_ms=1 operation=broker start"))
            .unwrap();
        let message = refusal();
        assert!(
            message.contains(&format!("pid {pid}, has exited")) && message.contains("unidentified"),
            "{message}"
        );
    }

    #[test]
    fn adoption_claim_blocks_cleanup_claim() {
        lifecycle_claim_contends_in_order("adoption", "cleanup");
    }

    #[test]
    fn cleanup_claim_blocks_adoption_claim() {
        lifecycle_claim_contends_in_order("cleanup", "adoption");
    }

    #[test]
    fn session_commit_claims_are_exclusive_per_session_and_independent_between_sessions() {
        let repository = tempfile::tempdir().unwrap();
        let held = try_acquire_session_commit_lock(repository.path(), 42).unwrap();
        let busy = try_acquire_session_commit_lock(repository.path(), 42).unwrap_err();
        assert_eq!(busy.kind(), io::ErrorKind::WouldBlock);
        let independent = try_acquire_session_commit_lock(repository.path(), 43).unwrap();

        drop(held);
        try_acquire_session_commit_lock(repository.path(), 42).unwrap();
        drop(independent);
    }

    #[test]
    fn lifecycle_claim_times_out_with_a_retry_instruction() {
        let repository = tempfile::tempdir().unwrap();
        let _held = acquire_repository_lifecycle_lock(repository.path()).unwrap();

        let error = acquire_repository_lifecycle_lock_with_wait(
            repository.path(),
            Duration::from_millis(10),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(error.to_string().contains("retry this command"));
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_claim_aliases_the_same_canonical_primary_repository() {
        let holder = tempfile::tempdir().unwrap();
        let repository = holder.path().join("repository");
        let alias = holder.path().join("repository-alias");
        std::fs::create_dir(&repository).unwrap();
        std::os::unix::fs::symlink(&repository, &alias).unwrap();

        assert_eq!(
            repository_lifecycle_lock_path(&repository).unwrap(),
            repository_lifecycle_lock_path(&alias).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_claim_refuses_a_symlink_lockfile_without_touching_its_target() {
        let repository = tempfile::tempdir().unwrap();
        let path = repository_lifecycle_lock_path(repository.path()).unwrap();
        let target = repository.path().join("outside-lock-target");
        std::fs::write(&target, b"preserve target").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        assert!(acquire_repository_lifecycle_lock(repository.path()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve target");
    }
}
