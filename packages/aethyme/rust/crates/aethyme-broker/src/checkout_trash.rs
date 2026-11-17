//! Retired checkouts: renamed into a broker trash directory inside their
//! worktree pool, then deleted later by one host-wide, self-paced deleter.
//!
//! Removing a checkout used to mean `git worktree remove`, which unlinks the
//! whole tree (often hundreds of thousands of build outputs) inside the
//! caller's deadline and under the repository lifecycle claim. Here the
//! caller does two O(1) steps instead: `renameat(2)` the checkout into the
//! pool's trash (same parent directory, so the same volume) and drop its Git
//! registration. The branch ref is never touched. The deleter later unlinks
//! the tree at a fixed rate that slows under load but never below a floor,
//! never waits for memory pressure, and runs unpaced below the disk headroom.
//!
//! Scope is narrow on purpose:
//!
//! - Only a checkout under a broker-owned pool root (proved by the broker's
//!   own records, by the caller) is moved; everything else is removed by Git
//!   as before.
//! - The trash is [`TRASH_DIR`], created fresh with mode 0700 and a marker
//!   file; an existing one that is a symlink, not a directory, not owned by
//!   this user, open to others, or without the marker is refused. Every step
//!   works relative to directory descriptors opened without following links.
//! - The registration is looked up from the repository side and must name
//!   exactly this checkout; a worktree locked by anyone but the broker is
//!   refused.
//! - The deleter deletes only entries that match the broker's name format
//!   and that the broker recorded at rename time with their device and inode,
//!   re-checked on the pinned directory before deletion. Anything else in the
//!   trash is left alone and counted (`checkout_trash_unrecognized`). The tree
//!   removal never follows a symlink and never crosses a mount point.
//!
//! Only a caller that has already established that every byte of the
//! checkout is in Git, in a verified recovery archive, or catalogued
//! regenerable output may move it here.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The broker's trash directory inside one worktree pool. Pool scanners skip
/// names that start with a dot (`is_worktree_root_infrastructure`).
pub(crate) const TRASH_DIR: &str = ".aethyme-checkout-trash";
/// Marker file proving the trash directory is the broker's.
const TRASH_MARKER: &str = ".aethyme-trash-marker";
const TRASH_MARKER_CONTENT: &str = "aethyme checkout trash v1\n";
/// After this many failed deletions an entry is reported by name and tried
/// after every other entry, so it never wedges the queue.
pub(crate) const TRASH_FAILURE_REPORT_AFTER: u32 = 3;
/// The deleter's steady rate.
pub(crate) const DELETER_STEADY_UNLINKS_PER_SECOND: u32 = 2_000;
/// The rate the deleter never goes below, however loaded the host.
pub(crate) const DELETER_FLOOR_UNLINKS_PER_SECOND: u32 = 200;
/// File name of the host-wide single-flight lock.
pub(crate) const DELETER_LOCK_FILE: &str = "checkout-trash-deleter.lock";

static TRASH_NONCE: AtomicU64 = AtomicU64::new(0);

/// What the broker recorded about one trash entry when it moved a checkout
/// in: the directory's identity, its session, and failed deletions so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TrashRow {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) session_id: i64,
    #[serde(default)]
    pub(crate) failures: u32,
}

/// Recorded trash entries: trash directory path, then entry name.
pub(crate) type TrashRows = BTreeMap<PathBuf, BTreeMap<String, TrashRow>>;

/// A checkout moved into the trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrashedCheckout {
    pub(crate) trash_dir: PathBuf,
    pub(crate) entry: String,
    pub(crate) row: TrashRow,
    /// The Git registration (worktree admin directory) is gone. When it could
    /// not be removed, `git worktree prune` drops it later: it names a path
    /// that no longer exists.
    pub(crate) registration_removed: bool,
}

impl TrashedCheckout {
    pub(crate) fn path(&self) -> PathBuf {
        self.trash_dir.join(&self.entry)
    }
}

fn c_name(name: &str) -> std::io::Result<CString> {
    CString::new(name).map_err(|_| std::io::Error::other("a name holds a NUL byte"))
}

fn c_path(path: &Path) -> std::io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("a path holds a NUL byte"))
}

fn checked(result: libc::c_int) -> std::io::Result<libc::c_int> {
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Open a directory without following a symlink at its last component.
fn open_dir_nofollow(path: &Path) -> std::io::Result<OwnedFd> {
    let path = c_path(path)?;
    // SAFETY: `path` is NUL-terminated and outlives the call.
    let fd = checked(unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })?;
    // SAFETY: `fd` was just returned by `open` and is owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Open `name` inside `dir` as a directory, never following a symlink.
fn openat_dir_nofollow(dir: RawFd, name: &str) -> std::io::Result<OwnedFd> {
    let name = c_name(name)?;
    // SAFETY: `name` is NUL-terminated and `dir` is an open descriptor.
    let fd = checked(unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })?;
    // SAFETY: as above.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn fstat(fd: RawFd) -> std::io::Result<libc::stat> {
    // SAFETY: an all-zero stat is a valid output buffer.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open and `stat` is writable.
    checked(unsafe { libc::fstat(fd, &mut stat) })?;
    Ok(stat)
}

fn fstatat_nofollow(dir: RawFd, name: &str) -> std::io::Result<libc::stat> {
    let name = c_name(name)?;
    // SAFETY: an all-zero stat is a valid output buffer.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is NUL-terminated, `dir` open, `stat` writable.
    checked(unsafe { libc::fstatat(dir, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) })?;
    Ok(stat)
}

fn is_directory(stat: &libc::stat) -> bool {
    stat.st_mode & libc::S_IFMT == libc::S_IFDIR
}

/// The broker trash of one pool, opened and verified.
pub(crate) struct TrashDir {
    pool: OwnedFd,
    trash: OwnedFd,
    path: PathBuf,
}

impl TrashDir {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Open the trash of `pool`, creating it fresh (mode 0700, with the marker)
/// when it does not exist. An existing one is used only when it is a real
/// directory (no symlink is followed), owned by this user, closed to group
/// and others, and holds the broker's marker.
pub(crate) fn open_trash(pool: &Path) -> Result<TrashDir, String> {
    let pool_fd = open_dir_nofollow(pool)
        .map_err(|error| format!("cannot open the pool {}: {error}", pool.display()))?;
    let name = c_name(TRASH_DIR).map_err(|error| error.to_string())?;
    // SAFETY: `name` is NUL-terminated and `pool_fd` is open.
    let created = match checked(unsafe { libc::mkdirat(pool_fd.as_raw_fd(), name.as_ptr(), 0o700) })
    {
        Ok(_) => true,
        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => false,
        Err(error) => return Err(format!("cannot create the trash: {error}")),
    };
    let trash_fd = openat_dir_nofollow(pool_fd.as_raw_fd(), TRASH_DIR)
        .map_err(|error| format!("the trash is not a real directory: {error}"))?;
    let stat = fstat(trash_fd.as_raw_fd()).map_err(|error| error.to_string())?;
    // SAFETY: `geteuid` has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if !is_directory(&stat) || stat.st_uid != uid || stat.st_mode & 0o077 != 0 {
        return Err("the trash is not a private directory owned by this user".into());
    }
    let marker = c_name(TRASH_MARKER).map_err(|error| error.to_string())?;
    if created {
        // SAFETY: `marker` is NUL-terminated and `trash_fd` is open.
        let fd = checked(unsafe {
            libc::openat(
                trash_fd.as_raw_fd(),
                marker.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        })
        .map_err(|error| format!("cannot mark the trash: {error}"))?;
        // SAFETY: `fd` was just opened and is owned here.
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        std::io::Write::write_all(&mut file, TRASH_MARKER_CONTENT.as_bytes())
            .map_err(|error| format!("cannot mark the trash: {error}"))?;
    } else {
        // SAFETY: as above, read-only.
        let fd = checked(unsafe {
            libc::openat(
                trash_fd.as_raw_fd(),
                marker.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })
        .map_err(|_| "the trash directory lacks the broker's marker".to_string())?;
        // SAFETY: `fd` was just opened and is owned here.
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut content = String::new();
        std::io::Read::read_to_string(&mut file, &mut content)
            .map_err(|error| format!("cannot read the trash marker: {error}"))?;
        if content != TRASH_MARKER_CONTENT {
            return Err("the trash directory's marker is not the broker's".into());
        }
    }
    Ok(TrashDir {
        pool: pool_fd,
        trash: trash_fd,
        path: pool.join(TRASH_DIR),
    })
}

/// Whether `name` has the shape of an entry the broker names:
/// `<13-digit ms>-<session>-<pid>-<nonce>`, all decimal.
pub(crate) fn is_broker_entry_name(name: &str) -> bool {
    let parts = name.split('-').collect::<Vec<_>>();
    parts.len() == 4
        && parts[0].len() == 13
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// The registration (worktree admin directory) of `worktree`, looked up from
/// the repository side: Git must list the checkout, exactly one directory
/// directly under `<common>/worktrees/` must have a `gitdir` naming
/// `<worktree>/.git`, and the checkout's own `.git` file must point at that
/// same directory. Any mismatch is an error.
pub(crate) fn registration_of(
    repo: &crate::git::GitRepo,
    common_dir: &Path,
    worktree: &Path,
) -> Result<PathBuf, String> {
    let canonical_worktree = worktree
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", worktree.display()))?;
    let listed = repo
        .worktree_inventory()
        .map_err(|error| format!("cannot list worktrees: {error}"))?
        .iter()
        .any(|entry| entry.path.canonicalize().ok().as_ref() == Some(&canonical_worktree));
    if !listed {
        return Err("Git does not list this checkout as a worktree".into());
    }
    let expected_dot_git = canonical_worktree.join(".git");
    let registrations = common_dir
        .join("worktrees")
        .canonicalize()
        .map_err(|error| format!("cannot resolve the worktree registrations: {error}"))?;
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&registrations)
        .map_err(|error| format!("cannot read the worktree registrations: {error}"))?
        .filter_map(Result::ok)
    {
        let admin = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&admin) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(admin.join("gitdir")) else {
            continue;
        };
        let named = PathBuf::from(text.trim());
        if named.canonicalize().ok().as_ref() == Some(&expected_dot_git) {
            found.push(admin);
        }
    }
    let [admin] = found.as_slice() else {
        return Err(format!("{} registrations name this checkout", found.len()));
    };
    let pointed = std::fs::read_to_string(&expected_dot_git)
        .ok()
        .and_then(|text| {
            text.lines()
                .next()
                .and_then(|line| line.strip_prefix("gitdir:"))
                .map(|target| PathBuf::from(target.trim()))
        })
        .map(|target| {
            if target.is_absolute() {
                target
            } else {
                canonical_worktree.join(target)
            }
        })
        .and_then(|target| target.canonicalize().ok());
    if pointed.as_ref() != admin.canonicalize().ok().as_ref() {
        return Err("the checkout's .git file names another registration".into());
    }
    Ok(admin.clone())
}

/// Move `worktree`, a checkout directly inside the broker-owned pool `pool`,
/// into the pool's trash and drop its Git registration (looked up from
/// `repo`, whose Git common directory is `common_dir`). A lock the broker
/// placed is released; any other lock refuses the move. Environments bound
/// to the checkout are destroyed first (their hooks live in it). Nothing in
/// the checkout is unlinked here.
pub(crate) fn move_worktree_to_trash(
    repo: &crate::git::GitRepo,
    common_dir: &Path,
    pool: &Path,
    worktree: &Path,
    session_id: i64,
) -> Result<TrashedCheckout, String> {
    let _span = crate::phase_telemetry::span("checkout_trash_move");
    if worktree.parent() != Some(pool) {
        return Err(format!(
            "{} is not directly inside the pool {}",
            worktree.display(),
            pool.display()
        ));
    }
    let name = worktree
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no usable name", worktree.display()))?
        .to_owned();
    let admin = registration_of(repo, common_dir, worktree)?;
    let lock = admin.join("locked");
    match std::fs::read_to_string(&lock) {
        Ok(reason) if reason.trim() == crate::git::BROKER_WORKTREE_LOCK_REASON => {
            std::fs::remove_file(&lock)
                .map_err(|error| format!("cannot release the broker's worktree lock: {error}"))?;
        }
        Ok(reason) => {
            return Err(format!(
                "the worktree is locked ({}); its lock is somebody else's protection",
                reason.trim()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot read the worktree lock: {error}")),
    }
    let trash = open_trash(pool)?;
    let source = fstatat_nofollow(trash.pool.as_raw_fd(), &name)
        .map_err(|error| format!("cannot inspect the checkout: {error}"))?;
    if !is_directory(&source) {
        return Err(format!("{} is not a real directory", worktree.display()));
    }
    crate::environments::lifecycle::destroy_for_worktree(worktree)?;
    let entry = format!(
        "{:013}-{session_id}-{}-{}",
        crate::clock::epoch_ms().max(0),
        std::process::id(),
        TRASH_NONCE.fetch_add(1, Ordering::Relaxed)
    );
    let from = c_name(&name).map_err(|error| error.to_string())?;
    let to = c_name(&entry).map_err(|error| error.to_string())?;
    // SAFETY: both names are NUL-terminated; both descriptors are open
    // directories on the same volume (the trash is inside the pool).
    checked(unsafe {
        libc::renameat(
            trash.pool.as_raw_fd(),
            from.as_ptr(),
            trash.trash.as_raw_fd(),
            to.as_ptr(),
        )
    })
    .map_err(|error| format!("cannot move the checkout into the trash: {error}"))?;
    let moved = fstatat_nofollow(trash.trash.as_raw_fd(), &entry)
        .map_err(|error| format!("cannot inspect the trashed checkout: {error}"))?;
    #[allow(clippy::unnecessary_cast)]
    let row = TrashRow {
        device: moved.st_dev as u64,
        inode: moved.st_ino as u64,
        session_id,
        failures: 0,
    };
    crate::phase_telemetry::add("checkout_trashed", Duration::ZERO);
    let registration_removed = match std::fs::remove_dir_all(&admin) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => {
            crate::phase_telemetry::add("checkout_trash_registration_left", Duration::ZERO);
            false
        }
    };
    Ok(TrashedCheckout {
        trash_dir: trash.path.clone(),
        entry,
        row,
        registration_removed,
    })
}

/// How fast the deleter may unlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleterPace {
    /// The steady rate.
    Steady,
    /// The host is loaded: the floor rate.
    Floor,
    /// The volume is below its headroom: as fast as the disk allows.
    Unpaced,
}

impl DeleterPace {
    /// The pace for a volume that is (or is not) below its headroom on a
    /// host that is (or is not) loaded. Memory pressure is deliberately not
    /// an input: unlinking frees page cache rather than using memory.
    pub(crate) const fn for_host(below_headroom: bool, loaded: bool) -> Self {
        if below_headroom {
            Self::Unpaced
        } else if loaded {
            Self::Floor
        } else {
            Self::Steady
        }
    }

    pub(crate) const fn unlinks_per_second(self) -> Option<u32> {
        match self {
            Self::Steady => Some(DELETER_STEADY_UNLINKS_PER_SECOND),
            Self::Floor => Some(DELETER_FLOOR_UNLINKS_PER_SECOND),
            Self::Unpaced => None,
        }
    }
}

/// Whether the host's one-minute load average exceeds its parallelism.
pub(crate) fn host_loaded() -> bool {
    let mut load = [0f64; 3];
    // SAFETY: `load` has room for the three samples requested.
    let samples = unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
    let parallelism = std::thread::available_parallelism().map_or(1, usize::from) as f64;
    samples >= 1 && load[0] > parallelism
}

/// Holds unlinks to a rate by sleeping only when ahead of it.
struct UnlinkLimiter {
    per_second: Option<u32>,
    started: Instant,
    unlinks: u64,
}

impl UnlinkLimiter {
    fn new(pace: DeleterPace) -> Self {
        Self {
            per_second: pace.unlinks_per_second(),
            started: Instant::now(),
            unlinks: 0,
        }
    }

    fn before_unlink(&mut self) {
        self.unlinks = self.unlinks.saturating_add(1);
        let Some(per_second) = self.per_second.filter(|rate| *rate > 0) else {
            return;
        };
        let due = Duration::from_secs_f64(self.unlinks as f64 / f64::from(per_second));
        let elapsed = self.started.elapsed();
        // Sleep in batches of at least 5 ms so pacing costs no syscall per
        // unlink.
        if let Some(ahead) = due.checked_sub(elapsed)
            && ahead >= Duration::from_millis(5)
        {
            std::thread::sleep(ahead);
        }
    }
}

/// The host-wide single-flight deleter lock. It is a kernel file lock, so a
/// holder that died releases it with its descriptors: a dead owner never
/// keeps the next deleter out. The file names the current holder.
pub(crate) struct DeleterLock {
    _lock: crate::file_lock::ExclusiveFileLock,
}

/// Take the deleter lock at `path` if nobody holds it.
pub(crate) fn try_deleter_lock(path: &Path) -> std::io::Result<Option<DeleterLock>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let Some(lock) = crate::file_lock::ExclusiveFileLock::try_acquire(file)? else {
        return Ok(None);
    };
    crate::warn_unrecorded(
        "name the checkout trash deleter",
        lock.record_holder(&format!(
            "pid {} since {}\n",
            std::process::id(),
            crate::clock::epoch_ms()
        )),
    );
    Ok(Some(DeleterLock { _lock: lock }))
}

/// Clear the user-immutable and append-only flags that make an unlink fail
/// with `EPERM` on macOS (Finder's "Locked"). Best effort, never follows a
/// link, never leaves the entry; nothing to do elsewhere.
#[cfg(target_os = "macos")]
fn clear_deletion_flags(root: &Path) {
    fn visit(path: &Path, depth: usize) {
        if depth > 256 {
            return;
        }
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return;
        };
        if metadata.file_type().is_symlink() {
            return;
        }
        if let Ok(c_path) = c_path(path) {
            // SAFETY: `c_path` is NUL-terminated and outlives the call.
            unsafe {
                libc::lchflags(c_path.as_ptr(), 0);
            }
        }
        if metadata.is_dir()
            && let Ok(children) = std::fs::read_dir(path)
        {
            for child in children.filter_map(Result::ok) {
                visit(&child.path(), depth + 1);
            }
        }
    }
    visit(root, 0);
}

#[cfg(not(target_os = "macos"))]
fn clear_deletion_flags(_root: &Path) {}

/// What one deleter run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TrashDrainReport {
    pub(crate) entries_removed: usize,
    /// The run's deadline stopped an entry part-way; the next run resumes it.
    pub(crate) entries_interrupted: usize,
    pub(crate) unlinks: usize,
    pub(crate) bytes: u64,
    /// Entries that failed this run.
    pub(crate) failed: Vec<String>,
    /// Entries that have failed at least [`TRASH_FAILURE_REPORT_AFTER`] times.
    pub(crate) reported: Vec<String>,
    /// Names in a trash the broker did not record (left alone).
    pub(crate) unrecognized: usize,
    /// Trash directories that failed verification (left alone).
    pub(crate) invalid_trash_dirs: usize,
    pub(crate) pace: Option<DeleterPace>,
    /// Recorded entries still in the trash after the run.
    pub(crate) entries_pending: usize,
}

/// Delete recorded entries of the trash directories in `rows` oldest first
/// until `deadline`, at `pace`; `rows` is updated (removed entries dropped,
/// failures counted, entries gone missing forgotten). The caller holds the
/// deleter lock and persists `rows`. Each trash directory is re-verified,
/// each entry must have the broker's name format and its recorded device and
/// inode, re-checked on the pinned directory immediately before deletion.
pub(crate) fn drain_trash(
    rows: &mut TrashRows,
    deadline: Instant,
    pace: DeleterPace,
) -> TrashDrainReport {
    let _span = crate::phase_telemetry::span("checkout_trash_drain");
    let mut report = TrashDrainReport {
        pace: Some(pace),
        ..TrashDrainReport::default()
    };
    match pace {
        DeleterPace::Unpaced => {
            crate::phase_telemetry::add("checkout_trash_unpaced_runs", Duration::ZERO);
        }
        DeleterPace::Floor => {
            crate::phase_telemetry::add("checkout_trash_floor_runs", Duration::ZERO);
        }
        DeleterPace::Steady => {}
    }
    let started = Instant::now();
    // The worker's unattended pacer would stop this deleter on memory
    // pressure; it paces itself instead.
    crate::gc::unattended_pacing::without_pacer(|| {
        'dirs: for (trash_path, entries) in rows.iter_mut() {
            let Some(pool) = trash_path.parent() else {
                report.invalid_trash_dirs += 1;
                continue;
            };
            if trash_path.file_name().and_then(|name| name.to_str()) != Some(TRASH_DIR) {
                report.invalid_trash_dirs += 1;
                continue;
            }
            let Ok(trash) = open_trash(pool) else {
                report.invalid_trash_dirs += 1;
                crate::phase_telemetry::add("checkout_trash_invalid_dir", Duration::ZERO);
                continue;
            };
            let listed = std::fs::read_dir(trash.path())
                .map(|listing| {
                    listing
                        .filter_map(Result::ok)
                        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                        .filter(|name| name != TRASH_MARKER)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for name in &listed {
                if !is_broker_entry_name(name) || !entries.contains_key(name) {
                    report.unrecognized += 1;
                    crate::phase_telemetry::add("checkout_trash_unrecognized", Duration::ZERO);
                }
            }
            // Recorded entries no longer present are forgotten.
            entries.retain(|name, _| listed.contains(name));
            let mut order = entries
                .iter()
                .filter(|(name, _)| is_broker_entry_name(name))
                .map(|(name, row)| (row.failures >= TRASH_FAILURE_REPORT_AFTER, name.clone()))
                .collect::<Vec<_>>();
            order.sort();
            for (_, name) in order {
                if Instant::now() >= deadline {
                    break 'dirs;
                }
                let Some(row) = entries.get(&name).copied() else {
                    continue;
                };
                let path = trash.path().join(&name);
                let pinned = match crate::gc::PinnedTreeRemoval::open(&path) {
                    Ok(pinned) => pinned,
                    Err(_) => {
                        report.unrecognized += 1;
                        continue;
                    }
                };
                if pinned.object_identity().ok()
                    != Some(crate::gc::TreeObjectIdentity {
                        device: row.device,
                        inode: row.inode,
                    })
                {
                    report.unrecognized += 1;
                    crate::phase_telemetry::add("checkout_trash_unrecognized", Duration::ZERO);
                    continue;
                }
                let mut limiter = UnlinkLimiter::new(pace);
                match pinned.remove_with_progress_and_observed_file_bytes_and_guard(
                    None,
                    Some(deadline),
                    || {
                        limiter.before_unlink();
                        Ok(())
                    },
                ) {
                    Ok((crate::gc::TreeRemoval::Complete, unlinks, bytes)) => {
                        report.entries_removed += 1;
                        report.unlinks = report.unlinks.saturating_add(unlinks);
                        report.bytes = report.bytes.saturating_add(bytes);
                        entries.remove(&name);
                        crate::phase_telemetry::add(
                            "checkout_trash_entries_removed",
                            Duration::ZERO,
                        );
                    }
                    Ok((crate::gc::TreeRemoval::Interrupted, unlinks, bytes)) => {
                        report.entries_interrupted += 1;
                        report.unlinks = report.unlinks.saturating_add(unlinks);
                        report.bytes = report.bytes.saturating_add(bytes);
                        break 'dirs;
                    }
                    Err(error) => {
                        report.unlinks = report.unlinks.saturating_add(error.entries_reclaimed);
                        clear_deletion_flags(&path);
                        if let Some(row) = entries.get_mut(&name) {
                            row.failures = row.failures.saturating_add(1);
                            if row.failures >= TRASH_FAILURE_REPORT_AFTER {
                                report.reported.push(name.clone());
                            }
                        }
                        crate::phase_telemetry::add("checkout_trash_entry_failed", Duration::ZERO);
                        report.failed.push(name.clone());
                    }
                }
            }
        }
    });
    crate::phase_telemetry::add("checkout_trash_drain_time", started.elapsed());
    rows.retain(|_, entries| !entries.is_empty());
    report.entries_pending = rows.values().map(BTreeMap::len).sum();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repository with one commit and two linked checkouts in a pool.
    struct Fixture {
        _dir: tempfile::TempDir,
        main: PathBuf,
        pool: PathBuf,
        repo: crate::git::GitRepo,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let main = dir.path().join("main");
            std::fs::create_dir_all(&main).unwrap();
            git(&main, &["init", "-q", "-b", "main"]);
            std::fs::write(main.join("base.txt"), "base\n").unwrap();
            git(&main, &["add", "base.txt"]);
            git(&main, &["commit", "-qm", "base"]);
            let pool = dir.path().join("pool");
            std::fs::create_dir_all(&pool).unwrap();
            let repo = crate::git::GitRepo::discover(&main).unwrap();
            Self {
                _dir: dir,
                main,
                pool,
                repo,
            }
        }

        fn checkout(&self, name: &str) -> PathBuf {
            let path = self.pool.join(name);
            git(
                &self.main,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    &format!("branch-{name}"),
                    path.to_str().unwrap(),
                ],
            );
            path
        }

        fn common(&self) -> PathBuf {
            self.main.join(".git")
        }

        fn trash(&self, checkout: &Path, session_id: i64) -> Result<TrashedCheckout, String> {
            move_worktree_to_trash(&self.repo, &self.common(), &self.pool, checkout, session_id)
        }
    }

    fn rows_of(trashed: &TrashedCheckout) -> TrashRows {
        let mut rows = TrashRows::new();
        rows.entry(trashed.trash_dir.clone())
            .or_default()
            .insert(trashed.entry.clone(), trashed.row);
        rows
    }

    /// Retiring renames the checkout into the pool's private, marked trash
    /// before anything is unlinked: every file is still there, Git no longer
    /// lists the worktree, the branch is untouched.
    #[test]
    fn a_checkout_is_renamed_into_the_trash_before_any_unlink() {
        let fixture = Fixture::new();
        let checkout = fixture.checkout("one");
        std::fs::create_dir_all(checkout.join("target/debug")).unwrap();
        std::fs::write(checkout.join("target/debug/artifact"), "built\n").unwrap();

        let trashed = fixture.trash(&checkout, 7).unwrap();

        assert!(!checkout.exists(), "the checkout path is free at once");
        assert_eq!(trashed.trash_dir, fixture.pool.join(TRASH_DIR));
        assert!(is_broker_entry_name(&trashed.entry));
        assert_eq!(
            std::fs::read_to_string(trashed.path().join("target/debug/artifact")).unwrap(),
            "built\n",
            "nothing was unlinked by the move"
        );
        assert!(trashed.registration_removed);
        let listing = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&fixture.main)
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&listing.stdout).into_owned();
        assert!(!listing.contains("pool/one"), "{listing}");
        let branch = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "refs/heads/branch-one"])
            .current_dir(&fixture.main)
            .output()
            .unwrap();
        assert!(branch.status.success(), "the branch ref is untouched");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(fixture.pool.join(TRASH_DIR))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    /// A worktree locked by its user is refused and left in place.
    #[test]
    fn a_user_locked_worktree_is_refused() {
        let fixture = Fixture::new();
        let checkout = fixture.checkout("locked");
        git(
            &fixture.main,
            &[
                "worktree",
                "lock",
                "--reason",
                "mine",
                checkout.to_str().unwrap(),
            ],
        );
        let error = fixture.trash(&checkout, 1).unwrap_err();
        assert!(error.contains("locked"), "{error}");
        assert!(checkout.join("base.txt").exists());
    }

    /// A checkout whose `.git` file points at another worktree's
    /// registration is refused: the registration is looked up from the
    /// repository side, and both must agree.
    #[test]
    fn a_dot_git_pointing_at_another_registration_is_refused() {
        let fixture = Fixture::new();
        let victim = fixture.checkout("victim");
        let forger = fixture.checkout("forger");
        let victim_pointer = std::fs::read_to_string(victim.join(".git")).unwrap();
        std::fs::write(forger.join(".git"), victim_pointer).unwrap();
        let error = fixture.trash(&forger, 1).unwrap_err();
        assert!(!error.is_empty());
        assert!(forger.exists());
        assert!(
            victim.join("base.txt").exists(),
            "the other checkout is untouched"
        );
        let admin = fixture.common().join("worktrees/victim");
        assert!(admin.exists(), "the other registration is untouched");
    }

    /// A directory Git does not list is refused.
    #[test]
    fn an_unregistered_directory_is_refused() {
        let fixture = Fixture::new();
        let stray = fixture.pool.join("stray");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join(".git"), "gitdir: /nowhere/at/all\n").unwrap();
        assert!(fixture.trash(&stray, 1).is_err());
        assert!(stray.exists());
    }

    /// A checkout outside the pool it is said to belong to is refused.
    #[test]
    fn a_checkout_outside_the_pool_is_refused() {
        let fixture = Fixture::new();
        let elsewhere = fixture._dir.path().join("elsewhere");
        git(
            &fixture.main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "elsewhere",
                elsewhere.to_str().unwrap(),
            ],
        );
        let error = move_worktree_to_trash(
            &fixture.repo,
            &fixture.common(),
            &fixture.pool,
            &elsewhere,
            1,
        )
        .unwrap_err();
        assert!(error.contains("not directly inside"), "{error}");
        assert!(elsewhere.join("base.txt").exists());
    }

    /// A pre-existing directory of the trash's name without the broker's
    /// marker, and a symlink in its place, are both refused.
    #[test]
    fn a_foreign_or_symlinked_trash_is_refused() {
        let fixture = Fixture::new();
        let checkout = fixture.checkout("one");
        std::fs::create_dir(fixture.pool.join(TRASH_DIR)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            fixture.pool.join(TRASH_DIR),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let error = fixture.trash(&checkout, 1).unwrap_err();
        assert!(error.contains("marker"), "{error}");
        assert!(checkout.join("base.txt").exists());

        std::fs::remove_dir(fixture.pool.join(TRASH_DIR)).unwrap();
        let elsewhere = fixture._dir.path().join("somewhere-else");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, fixture.pool.join(TRASH_DIR)).unwrap();
        let error = fixture.trash(&checkout, 1).unwrap_err();
        assert!(error.contains("not a real directory"), "{error}");
        assert!(checkout.join("base.txt").exists());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    /// Only recorded entries are deleted: a name the broker never recorded,
    /// and one whose identity differs from the record, are left alone and
    /// counted.
    #[test]
    fn unrecorded_entries_are_left_alone() {
        let fixture = Fixture::new();
        let trashed = fixture.trash(&fixture.checkout("one"), 1).unwrap();
        let stranger = trashed.trash_dir.join("0000000000001-9-9-9");
        std::fs::create_dir(&stranger).unwrap();
        std::fs::write(stranger.join("keep.txt"), "not ours\n").unwrap();
        let odd = trashed.trash_dir.join("someone-elses");
        std::fs::create_dir(&odd).unwrap();
        let mut rows = rows_of(&trashed);

        let report = drain_trash(
            &mut rows,
            Instant::now() + Duration::from_secs(600),
            DeleterPace::Unpaced,
        );

        assert_eq!(report.entries_removed, 1, "{report:?}");
        assert!(!trashed.path().exists());
        assert_eq!(report.unrecognized, 2, "{report:?}");
        assert!(stranger.join("keep.txt").exists());
        assert!(odd.exists());
        assert!(rows.is_empty());
    }

    /// A symlink inside a trashed tree is removed as a link: what it points
    /// at, outside the trash, survives.
    #[test]
    fn a_symlink_inside_a_trashed_tree_is_not_followed() {
        let fixture = Fixture::new();
        let checkout = fixture.checkout("one");
        let outside = fixture._dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("precious.txt"), "precious\n").unwrap();
        std::os::unix::fs::symlink(&outside, checkout.join("link")).unwrap();
        let trashed = fixture.trash(&checkout, 1).unwrap();
        let mut rows = rows_of(&trashed);

        let report = drain_trash(
            &mut rows,
            Instant::now() + Duration::from_secs(600),
            DeleterPace::Unpaced,
        );

        assert_eq!(report.entries_removed, 1, "{report:?}");
        assert_eq!(
            std::fs::read_to_string(outside.join("precious.txt")).unwrap(),
            "precious\n"
        );
    }

    /// A run stopped part-way (its deadline, as a killed run would be)
    /// leaves the entry recorded, and the next run finishes it.
    #[test]
    fn the_deleter_resumes_an_entry_stopped_part_way() {
        let fixture = Fixture::new();
        let checkout = fixture.checkout("one");
        std::fs::create_dir_all(checkout.join("a/b")).unwrap();
        for index in 0..50 {
            std::fs::write(checkout.join(format!("a/b/file-{index}")), "x").unwrap();
        }
        let trashed = fixture.trash(&checkout, 1).unwrap();
        let mut rows = rows_of(&trashed);

        let stopped = drain_trash(&mut rows, Instant::now(), DeleterPace::Unpaced);
        assert_eq!(stopped.entries_removed, 0);
        assert!(trashed.path().exists());
        assert_eq!(stopped.entries_pending, 1);

        let resumed = drain_trash(
            &mut rows,
            Instant::now() + Duration::from_secs(600),
            DeleterPace::Unpaced,
        );
        assert_eq!(resumed.entries_removed, 1, "{resumed:?}");
        assert!(!trashed.path().exists());
        assert_eq!(resumed.entries_pending, 0);
    }

    /// The pace follows disk pressure and load, never memory: below the
    /// headroom it is unpaced, under load it keeps the floor.
    #[test]
    fn the_pace_ignores_memory_and_keeps_a_floor() {
        assert_eq!(DeleterPace::for_host(true, true), DeleterPace::Unpaced);
        assert_eq!(DeleterPace::for_host(true, false), DeleterPace::Unpaced);
        assert_eq!(DeleterPace::for_host(false, true), DeleterPace::Floor);
        assert_eq!(DeleterPace::for_host(false, false), DeleterPace::Steady);
        assert_eq!(DeleterPace::Unpaced.unlinks_per_second(), None);
        assert_eq!(
            DeleterPace::Floor.unlinks_per_second(),
            Some(DELETER_FLOOR_UNLINKS_PER_SECOND)
        );
        assert!(DELETER_FLOOR_UNLINKS_PER_SECOND >= 200);
    }

    /// The deleter runs with the worker's pacer set aside, so memory
    /// pressure (which that pacer acts on) cannot stop it.
    #[test]
    fn the_deleter_runs_without_the_memory_pacer() {
        let installed_inside = crate::gc::unattended_pacing::without_pacer(
            crate::gc::unattended_pacing::pacer_installed,
        );
        assert!(!installed_inside);
    }

    /// Entry names: only the broker's own format qualifies.
    #[test]
    fn only_the_broker_entry_name_format_qualifies() {
        assert!(is_broker_entry_name("1790000000000-12-345-6"));
        assert!(!is_broker_entry_name("179000000000-12-345-6"));
        assert!(!is_broker_entry_name("1790000000000-12-345"));
        assert!(!is_broker_entry_name("1790000000000-12-x-6"));
        assert!(!is_broker_entry_name(".aethyme-trash-marker"));
    }

    /// A lock file left by a holder that is gone (its descriptor closed, as a
    /// dead process's is) is taken over; a held lock is not.
    #[test]
    fn a_dead_deleter_lock_owner_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DELETER_LOCK_FILE);
        std::fs::write(&path, "pid 999999 since 1\n").unwrap();
        let first = try_deleter_lock(&path).unwrap();
        assert!(
            first.is_some(),
            "a stale holder line does not keep anyone out"
        );
        assert!(
            try_deleter_lock(&path).unwrap().is_none(),
            "a live holder does"
        );
        drop(first);
        assert!(try_deleter_lock(&path).unwrap().is_some());
        let holder = std::fs::read_to_string(&path).unwrap();
        assert!(
            holder.starts_with(&format!("pid {}", std::process::id())),
            "{holder}"
        );
    }
}
