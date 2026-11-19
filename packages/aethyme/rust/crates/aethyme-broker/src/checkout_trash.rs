//! Retired checkouts: renamed into a broker trash directory inside their
//! worktree pool, then deleted later by one host-wide, self-paced deleter.
//!
//! Removing a checkout used to mean `git worktree remove`, which unlinks the
//! whole tree (often hundreds of thousands of build outputs) inside the
//! caller's deadline and under the repository lifecycle claim. Here the
//! caller does two O(1) steps instead: `renameat(2)` the checkout into the
//! pool's trash (same parent directory, so the same volume) and drop its Git
//! registration. The branch ref is never touched. The deleter later unlinks
//! the tree at a steady rate that slows under load but never below a floor,
//! rises to a bounded catch-up rate while the trash is behind
//! ([`TrashBacklog::behind`]), and runs unpaced below the disk headroom.
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
/// The catch-up rate while the trash is behind on a host with no pressure:
/// the deleter's bounded maximum outside disk pressure. A dependency tree of
/// about 100,000 entries goes in about 12 s.
pub(crate) const DELETER_CATCHUP_UNLINKS_PER_SECOND: u32 = 8_000;
/// The catch-up rate while the trash is behind on a loaded host (load above
/// the core count, Linux I/O pressure) or at the memory warning level.
pub(crate) const DELETER_CATCHUP_LOADED_UNLINKS_PER_SECOND: u32 = 4_000;
/// Recorded entries at or above which the trash is behind.
pub(crate) const TRASH_BACKLOG_ENTRIES: usize = 16;
/// Age of the oldest recorded entry at or above which the trash is behind.
pub(crate) const TRASH_BACKLOG_AGE_MS: i64 = 24 * 3_600_000;
/// The longest one continuing catch-up lane (the detached worker's, after
/// its other lanes) drains the trash.
pub(crate) const TRASH_CATCHUP_MAX: Duration = Duration::from_secs(20 * 60);
/// How often a catching-up drain reads the host's pressure again.
const DELETER_PACE_RESAMPLE_EVERY: Duration = Duration::from_secs(2);
/// Unlinks between checks that an entry being deleted is still in the trash.
const TRASH_LOCATION_CHECK_EVERY: u32 = 256;
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
    /// Written before the rename (with the source directory's identity,
    /// which a same-volume rename keeps) and cleared when the mover confirms
    /// it. The deleter never deletes a pending row's entry while its creator
    /// may still run: only once the creator is proven gone and the entry
    /// still matches the recorded identity is it confirmed.
    #[serde(default)]
    pub(crate) pending: bool,
    /// The process that wrote the row, and when it started (epoch ms; 0 when
    /// unknown, which never confirms a pending row on its own).
    #[serde(default)]
    pub(crate) creator_pid: i64,
    #[serde(default)]
    pub(crate) creator_started_ms: i64,
    /// The entry is a regenerable directory stripped out of a checkout that
    /// is still in place, not the checkout itself: it never proves the
    /// session's checkout gone (`checkout_proven_trashed`), and its bytes
    /// count as freed by strip.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) stripped: bool,
    /// The entry is a session scratch directory
    /// ([`crate::session_scratch`]), moved in from the pool's scratch
    /// container. Always recorded with `stripped` set too (it is not the
    /// checkout); its bytes are counted as scratch, not as strip.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) scratch: bool,
}

/// This process's id and start time, as a process-table snapshot reads it.
pub(crate) fn this_process_identity() -> (i64, i64) {
    let pid = i64::from(std::process::id());
    let started = crate::session_holder::ProcessTable::snapshot()
        .and_then(|table| table.process(pid))
        .and_then(|process| crate::session_holder::lstart_epoch_ms(&process.started))
        .unwrap_or(0);
    (pid, started)
}

/// Whether the creator recorded in `row` is proven gone: `table` does not
/// list a process with its pid and start time. Unknown start time or no
/// table proves nothing.
fn creator_gone(row: &TrashRow, table: Option<&crate::session_holder::ProcessTable>) -> bool {
    let Some(table) = table else {
        return false;
    };
    if row.creator_pid <= 0 || row.creator_started_ms <= 0 {
        return false;
    }
    table.process(row.creator_pid).is_none_or(|process| {
        crate::session_holder::lstart_epoch_ms(&process.started) != Some(row.creator_started_ms)
    })
}

#[cfg(test)]
thread_local! {
    /// Test seam: environment destruction fails.
    pub(crate) static FAIL_ENVIRONMENT_DESTROY: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

fn destroy_environments(worktree: &Path) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_ENVIRONMENT_DESTROY.with(std::cell::Cell::get) {
        return Err("simulated environment destruction failure".into());
    }
    crate::environments::lifecycle::destroy_for_worktree(worktree).map(|_| ())
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
    #[cfg(test)]
    pub(crate) fn path(&self) -> PathBuf {
        self.trash_dir.join(&self.entry)
    }
}

pub(crate) fn c_name(name: &str) -> std::io::Result<CString> {
    CString::new(name).map_err(|_| std::io::Error::other("a name holds a NUL byte"))
}

fn c_path(path: &Path) -> std::io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("a path holds a NUL byte"))
}

pub(crate) fn checked(result: libc::c_int) -> std::io::Result<libc::c_int> {
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Open a directory without following a symlink at its last component.
pub(crate) fn open_dir_nofollow(path: &Path) -> std::io::Result<OwnedFd> {
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
pub(crate) fn openat_dir_nofollow(dir: RawFd, name: &str) -> std::io::Result<OwnedFd> {
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

pub(crate) fn fstat(fd: RawFd) -> std::io::Result<libc::stat> {
    // SAFETY: an all-zero stat is a valid output buffer.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open and `stat` is writable.
    checked(unsafe { libc::fstat(fd, &mut stat) })?;
    Ok(stat)
}

pub(crate) fn fstatat_nofollow(dir: RawFd, name: &str) -> std::io::Result<libc::stat> {
    let name = c_name(name)?;
    // SAFETY: an all-zero stat is a valid output buffer.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is NUL-terminated, `dir` open, `stat` writable.
    checked(unsafe { libc::fstatat(dir, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) })?;
    Ok(stat)
}

pub(crate) fn is_directory(stat: &libc::stat) -> bool {
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

    /// The trash directory's own descriptor (opened without following links).
    pub(crate) fn trash_fd(&self) -> RawFd {
        self.trash.as_raw_fd()
    }
}

/// A fresh trash entry name for `session_id`, in the one format the deleter
/// accepts ([`is_broker_entry_name`]).
pub(crate) fn new_entry_name(session_id: i64) -> String {
    format!(
        "{:013}-{session_id}-{}-{}",
        crate::clock::epoch_ms().max(0),
        std::process::id(),
        TRASH_NONCE.fetch_add(1, Ordering::Relaxed)
    )
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

/// Remove a trash that holds nothing but its marker, through its
/// descriptors: an emptied pool is left as it was found. `rmdir` refuses a
/// directory that gained anything meanwhile.
fn remove_trash_if_empty(trash: &TrashDir) {
    let only_marker = std::fs::read_dir(trash.path()).is_ok_and(|listing| {
        listing
            .filter_map(Result::ok)
            .all(|entry| entry.file_name() == TRASH_MARKER)
    });
    if !only_marker {
        return;
    }
    let (Ok(marker), Ok(name)) = (c_name(TRASH_MARKER), c_name(TRASH_DIR)) else {
        return;
    };
    // SAFETY: both names are NUL-terminated and both descriptors are open
    // directories; neither call follows a symlink.
    let removed = unsafe {
        libc::unlinkat(trash.trash.as_raw_fd(), marker.as_ptr(), 0);
        libc::unlinkat(trash.pool.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) == 0
    };
    if !removed {
        // Something was moved in meanwhile: mark the trash again so it stays
        // usable (`O_EXCL` leaves an existing marker alone).
        // SAFETY: as above; the descriptor returned is closed at once.
        unsafe {
            let fd = libc::openat(
                trash.trash.as_raw_fd(),
                marker.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            );
            if fd >= 0 {
                let mut file = std::fs::File::from_raw_fd(fd);
                crate::warn_unrecorded(
                    "mark the checkout trash again",
                    std::io::Write::write_all(&mut file, TRASH_MARKER_CONTENT.as_bytes()),
                );
            }
        }
    }
}

/// After this long, a pending row whose creator's start time was unreadable
/// is reported: nothing confirms it on its own.
pub(crate) const STALE_PENDING_AFTER_MS: i64 = 24 * 3_600_000;

/// When the entry `name` was created (its leading epoch milliseconds).
pub(crate) fn entry_created_ms(name: &str) -> Option<i64> {
    name.split('-').next()?.parse().ok()
}

/// Whether a pending row can never be confirmed by the deleter (its
/// creator's start time is unknown) and is older than a day.
pub(crate) fn stale_unconfirmable(name: &str, row: &TrashRow, now_ms: i64) -> bool {
    row.pending
        && row.creator_started_ms <= 0
        && entry_created_ms(name)
            .is_some_and(|created| now_ms.saturating_sub(created) > STALE_PENDING_AFTER_MS)
}

/// Whether the entry `name` in the trash of `pool` is a real directory with
/// the identity `row` recorded.
pub(crate) fn entry_matches(pool: &Path, name: &str, row: &TrashRow) -> bool {
    let Ok(trash) = open_trash(pool) else {
        return false;
    };
    fstatat_nofollow(trash.trash.as_raw_fd(), name).is_ok_and(|stat| {
        #[allow(clippy::unnecessary_cast)]
        let identity = (stat.st_dev as u64, stat.st_ino as u64);
        is_directory(&stat) && identity == (row.device, row.inode)
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
        .worktree_inventory_for_run()
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

/// `rename` that refuses to replace an existing destination:
/// `renameat2(RENAME_NOREPLACE)` on Linux, `renameatx_np(RENAME_EXCL)` on
/// macOS.
pub(crate) fn rename_noreplace(
    from_dir: RawFd,
    from: &str,
    to_dir: RawFd,
    to: &str,
) -> std::io::Result<()> {
    let from = c_name(from)?;
    let to = c_name(to)?;
    #[cfg(target_os = "linux")]
    {
        // The raw syscall, not `libc::renameat2`: musl has no wrapper for it,
        // so the wrapper fails to link in the static build.
        // SAFETY: `SYS_renameat2` receives the Linux renameat2 ABI arguments;
        // both names are NUL-terminated and both descriptors are open.
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                from_dir,
                from.as_ptr(),
                to_dir,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(target_os = "macos")]
    {
        // <sys/stdio.h>:
        //   int renameatx_np(int fromfd, const char *from, int tofd,
        //                    const char *to, unsigned int flags);
        //   #define RENAME_EXCL 0x00000004
        unsafe extern "C" {
            fn renameatx_np(
                fromfd: libc::c_int,
                from: *const libc::c_char,
                tofd: libc::c_int,
                to: *const libc::c_char,
                flags: libc::c_uint,
            ) -> libc::c_int;
        }
        const RENAME_EXCL: libc::c_uint = 0x0000_0004;
        // SAFETY: both names are NUL-terminated; both descriptors are open.
        checked(unsafe {
            renameatx_np(from_dir, from.as_ptr(), to_dir, to.as_ptr(), RENAME_EXCL)
        })?;
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _unsupported = (from_dir, from, to_dir, to);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "an exclusive rename is unavailable on this platform",
        ))
    }
}

/// Move `worktree`, a checkout directly inside the broker-owned pool `pool`,
/// into the pool's trash and drop its Git registration (looked up from
/// `repo`, whose Git common directory is `common_dir`).
///
/// Order: the registration and any lock are checked first (a lock anyone
/// but the broker placed refuses); environments bound to the checkout are
/// destroyed (a failure keeps everything in place); `record_pending` stores
/// the entry's row (the source directory's identity and this process) before
/// anything moves; the exclusive rename comes next, and a failure there
/// leaves the checkout exactly as it was. After a successful rename nothing
/// moves the entry back. The registration, with the broker's lock, goes last.
/// Nothing in the checkout is unlinked here.
pub(crate) fn move_worktree_to_trash(
    repo: &crate::git::GitRepo,
    common_dir: &Path,
    pool: &Path,
    worktree: &Path,
    session_id: i64,
    record_pending: &mut dyn FnMut(&Path, &str, TrashRow) -> Result<(), String>,
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
    if let Ok(main_root) = repo.main_root() {
        crate::start_reservation::refuse_if_start_in_progress(&main_root, worktree)?;
    }
    let admin = registration_of(repo, common_dir, worktree)?;
    match std::fs::read_to_string(admin.join("locked")) {
        Ok(reason) if reason.trim() == crate::git::BROKER_WORKTREE_LOCK_REASON => {}
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
    // Environments bound to the checkout are destroyed first (their hooks
    // live in it): a failure keeps everything where it is, and once the
    // rename succeeds nothing ever moves the entry back into the pool.
    destroy_environments(worktree).map_err(|reason| {
        format!("environments bound to the checkout could not be destroyed: {reason}")
    })?;
    let entry = new_entry_name(session_id);
    let (creator_pid, creator_started_ms) = this_process_identity();
    #[allow(clippy::unnecessary_cast)]
    let mut row = TrashRow {
        device: source.st_dev as u64,
        inode: source.st_ino as u64,
        session_id,
        failures: 0,
        pending: true,
        creator_pid,
        creator_started_ms,
        stripped: false,
        scratch: false,
    };
    record_pending(&trash.path, &entry, row)?;
    rename_noreplace(
        trash.pool.as_raw_fd(),
        &name,
        trash.trash.as_raw_fd(),
        &entry,
    )
    .map_err(|error| format!("cannot move the checkout into the trash: {error}"))?;
    crate::phase_telemetry::add("checkout_trashed", Duration::ZERO);
    crate::git::forget_cached_worktree_registration(worktree);
    let registration_removed = match std::fs::remove_dir_all(&admin) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => {
            crate::phase_telemetry::add("checkout_trash_registration_left", Duration::ZERO);
            false
        }
    };
    row.pending = false;
    Ok(TrashedCheckout {
        trash_dir: trash.path.clone(),
        entry,
        row,
        registration_removed,
    })
}

/// A regenerable directory renamed out of a checkout into the pool's trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StrippedDirectory {
    pub(crate) trash_dir: PathBuf,
    pub(crate) entry: String,
    pub(crate) row: TrashRow,
}

/// Why a directory was not moved out of its checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StripMoveRefusal {
    /// A component of the path is a symbolic link, not a directory, or the
    /// path is malformed.
    NotARealDirectory(String),
    /// The directory, or a component above it, is on another device than
    /// the checkout (a mount point), or the trash is.
    CrossesDevice,
    /// Anything else (the trash, the rename, the record): the directory
    /// stays where it is.
    Failed(String),
}

impl std::fmt::Display for StripMoveRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARealDirectory(reason) | Self::Failed(reason) => formatter.write_str(reason),
            Self::CrossesDevice => formatter.write_str("the directory is on another device"),
        }
    }
}

/// Move the directory `relative` (slash-separated, inside `worktree`, which
/// sits directly inside the broker-owned pool `pool`) into the pool's trash
/// with one exclusive same-volume rename, without reading its contents.
///
/// Every component is opened relative to its parent's descriptor without
/// following a link, and must be a real directory on the checkout's device
/// (no symlink, no mount point crossed); the trash must be on that device
/// too, so the rename is O(1) and never a copy. The row is recorded pending
/// before the rename, exactly as for a checkout; nothing is unlinked here.
/// The caller has already proved the directory regenerable, ignored, free of
/// tracked files, and the checkout unused.
pub(crate) fn move_directory_to_trash(
    pool: &Path,
    worktree: &Path,
    relative: &str,
    session_id: i64,
    record_pending: &mut dyn FnMut(&Path, &str, TrashRow) -> Result<(), String>,
) -> Result<StrippedDirectory, StripMoveRefusal> {
    use StripMoveRefusal::{CrossesDevice, Failed, NotARealDirectory};
    let _span = crate::phase_telemetry::span("checkout_strip_move");
    if worktree.parent() != Some(pool) {
        return Err(Failed(format!(
            "{} is not directly inside the pool {}",
            worktree.display(),
            pool.display()
        )));
    }
    let checkout_name = worktree
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Failed(format!("{} has no usable name", worktree.display())))?
        .to_owned();
    let components = relative.split('/').collect::<Vec<_>>();
    if components
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == ".." || part.contains('\0'))
    {
        return Err(NotARealDirectory(format!("invalid path {relative:?}")));
    }
    let Some((last, parents)) = components.split_last() else {
        return Err(NotARealDirectory(format!("invalid path {relative:?}")));
    };
    let trash = open_trash(pool).map_err(Failed)?;
    let checkout = openat_dir_nofollow(trash.pool.as_raw_fd(), &checkout_name)
        .map_err(|error| NotARealDirectory(format!("cannot open the checkout: {error}")))?;
    let checkout_device = fstat(checkout.as_raw_fd())
        .map_err(|error| Failed(error.to_string()))?
        .st_dev;
    let trash_device = fstat(trash.trash.as_raw_fd())
        .map_err(|error| Failed(error.to_string()))?
        .st_dev;
    if trash_device != checkout_device {
        return Err(CrossesDevice);
    }
    let mut parent = checkout;
    for part in parents {
        let next = openat_dir_nofollow(parent.as_raw_fd(), part).map_err(|error| {
            NotARealDirectory(format!(
                "{part} in {relative} is not a real directory: {error}"
            ))
        })?;
        if fstat(next.as_raw_fd())
            .map_err(|error| Failed(error.to_string()))?
            .st_dev
            != checkout_device
        {
            return Err(CrossesDevice);
        }
        parent = next;
    }
    let source = fstatat_nofollow(parent.as_raw_fd(), last)
        .map_err(|error| NotARealDirectory(format!("cannot inspect {relative}: {error}")))?;
    if !is_directory(&source) {
        return Err(NotARealDirectory(format!(
            "{relative} is not a real directory"
        )));
    }
    if source.st_dev != checkout_device {
        return Err(CrossesDevice);
    }
    let entry = new_entry_name(session_id);
    let (creator_pid, creator_started_ms) = this_process_identity();
    #[allow(clippy::unnecessary_cast)]
    let mut row = TrashRow {
        device: source.st_dev as u64,
        inode: source.st_ino as u64,
        session_id,
        failures: 0,
        pending: true,
        creator_pid,
        creator_started_ms,
        stripped: true,
        scratch: false,
    };
    record_pending(&trash.path, &entry, row).map_err(Failed)?;
    rename_noreplace(parent.as_raw_fd(), last, trash.trash.as_raw_fd(), &entry).map_err(
        |error| {
            if error.raw_os_error() == Some(libc::EXDEV) {
                CrossesDevice
            } else {
                Failed(format!("cannot move {relative} into the trash: {error}"))
            }
        },
    )?;
    crate::phase_telemetry::add("checkout_strip_directory_trashed", Duration::ZERO);
    row.pending = false;
    Ok(StrippedDirectory {
        trash_dir: trash.path.clone(),
        entry,
        row,
    })
}

/// How fast the deleter may unlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleterPace {
    /// The steady rate.
    Steady,
    /// The host is loaded: the floor rate.
    Floor,
    /// The trash is behind and the host is loaded (or at the memory warning
    /// level): the loaded catch-up rate.
    CatchupLoaded,
    /// The trash is behind and the host shows no pressure: the catch-up
    /// rate, the bounded maximum outside disk pressure.
    Catchup,
    /// The volume is below its headroom: as fast as the disk allows.
    Unpaced,
}

/// Why the deleter needs the pace it runs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainNeed {
    /// The volume is below its headroom.
    DiskPressure,
    /// The trash is behind ([`TrashBacklog::behind`]).
    Backlog,
    /// Neither: the gentle steady pace.
    Gentle,
}

impl DrainNeed {
    pub(crate) const fn of(below_headroom: bool, behind: bool) -> Self {
        if below_headroom {
            Self::DiskPressure
        } else if behind {
            Self::Backlog
        } else {
            Self::Gentle
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DiskPressure => "disk_pressure",
            Self::Backlog => "backlog",
            Self::Gentle => "gentle",
        }
    }
}

/// The host's pressure as the deleter reads it, from the signals the
/// unattended pacer already samples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HostPressure {
    /// One-minute load above the core count, or a pacer load or Linux I/O
    /// pressure signal.
    pub(crate) loaded: bool,
    /// Memory at the warning level (or a level that could not be read).
    pub(crate) memory_warning: bool,
    /// Memory at the critical level.
    pub(crate) memory_critical: bool,
}

impl DeleterPace {
    /// The pace for a volume that is (or is not) below its headroom on a
    /// host that is (or is not) loaded, with the trash not behind. Memory
    /// pressure is deliberately not an input: unlinking frees page cache
    /// rather than using memory.
    pub(crate) const fn for_host(below_headroom: bool, loaded: bool) -> Self {
        if below_headroom {
            Self::Unpaced
        } else if loaded {
            Self::Floor
        } else {
            Self::Steady
        }
    }

    /// The pace for `need` on a host under `host` pressure. Below the
    /// headroom it is unpaced and with no backlog it is the gentle pace of
    /// [`Self::for_host`]. Behind, it rises to the catch-up rate, held to the
    /// loaded catch-up rate under load or at the memory warning level, and
    /// to the floor at critical memory pressure.
    pub(crate) const fn for_need(need: DrainNeed, host: HostPressure) -> Self {
        match need {
            DrainNeed::DiskPressure => Self::Unpaced,
            DrainNeed::Gentle => Self::for_host(false, host.loaded),
            DrainNeed::Backlog => {
                if host.memory_critical {
                    Self::Floor
                } else if host.loaded || host.memory_warning {
                    Self::CatchupLoaded
                } else {
                    Self::Catchup
                }
            }
        }
    }

    pub(crate) const fn unlinks_per_second(self) -> Option<u32> {
        match self {
            Self::Steady => Some(DELETER_STEADY_UNLINKS_PER_SECOND),
            Self::Floor => Some(DELETER_FLOOR_UNLINKS_PER_SECOND),
            Self::CatchupLoaded => Some(DELETER_CATCHUP_LOADED_UNLINKS_PER_SECOND),
            Self::Catchup => Some(DELETER_CATCHUP_UNLINKS_PER_SECOND),
            Self::Unpaced => None,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Steady => "steady",
            Self::Floor => "floor",
            Self::CatchupLoaded => "catchup_loaded",
            Self::Catchup => "catchup",
            Self::Unpaced => "unpaced",
        }
    }
}

/// The recorded entries the deleter still has to delete: how many, and when
/// the oldest was moved in (from its name). Unconfirmed rows and entries
/// already reported as failing do not count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TrashBacklog {
    pub(crate) entries: usize,
    pub(crate) oldest_created_ms: Option<i64>,
}

impl TrashBacklog {
    pub(crate) fn of(rows: &TrashRows) -> Self {
        Self::deletable(rows, &std::collections::BTreeSet::new())
    }

    /// The backlog the deleter can work on: as [`Self::of`], also leaving
    /// out the entries the last run passed over because a process still used
    /// them (`in_use`, entry names). An entry held open for days must not
    /// keep the trash behind with nothing deletable.
    pub(crate) fn deletable(rows: &TrashRows, in_use: &std::collections::BTreeSet<String>) -> Self {
        let mut backlog = Self::default();
        for (name, _) in rows
            .values()
            .flat_map(BTreeMap::iter)
            .filter(|(name, row)| {
                !row.pending
                    && row.failures < TRASH_FAILURE_REPORT_AFTER
                    && is_broker_entry_name(name)
                    && !in_use.contains(*name)
            })
        {
            backlog.entries += 1;
            if let Some(created) = entry_created_ms(name) {
                backlog.oldest_created_ms = Some(
                    backlog
                        .oldest_created_ms
                        .map_or(created, |oldest| oldest.min(created)),
                );
            }
        }
        backlog
    }

    pub(crate) fn oldest_age_ms(&self, now_ms: i64) -> Option<i64> {
        self.oldest_created_ms
            .map(|created| now_ms.saturating_sub(created).max(0))
    }

    /// The trash is behind: at least [`TRASH_BACKLOG_ENTRIES`] entries, or
    /// an entry at least [`TRASH_BACKLOG_AGE_MS`] old.
    pub(crate) fn behind(&self, now_ms: i64) -> bool {
        self.entries >= TRASH_BACKLOG_ENTRIES
            || (self.entries > 0
                && self
                    .oldest_age_ms(now_ms)
                    .is_some_and(|age| age >= TRASH_BACKLOG_AGE_MS))
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

/// One reading of the host's pressure: this deleter's load test plus the
/// unattended pacer's memory, load and Linux I/O signals.
pub(crate) fn sample_host_pressure() -> HostPressure {
    use crate::gc::unattended_pacing::PressureSignal;
    let sample = crate::gc::unattended_pacing::SystemPressure::new().sample();
    HostPressure {
        loaded: host_loaded() || sample.load_elevated == Some(true),
        memory_warning: sample.memory_warning == Some(true),
        memory_critical: sample.memory_elevated == Some(true),
    }
}

/// The pace of one drain: fixed, or following the host's pressure for a
/// need, read again every [`DELETER_PACE_RESAMPLE_EVERY`] while paced.
pub(crate) struct DrainPacer {
    need: Option<DrainNeed>,
    sample: fn() -> HostPressure,
    pace: DeleterPace,
    /// `[cleanup] unattended_unlinks_per_second` when the repository sets
    /// it explicitly: the ceiling of every paced mode (never of unpaced).
    ceiling: Option<u32>,
    /// Deleter threads sharing the rate: each paces itself at its share.
    share: u32,
    /// The rate the unpaced mode (below the headroom) is held to: set in a
    /// process that raised its own disk I/O priority, where the OS throttle
    /// no longer bounds it.
    unpaced_cap: Option<u32>,
    /// Stop the drain, resumably, as soon as a reading shows critical
    /// memory pressure (the continuing catch-up lane), instead of going on
    /// at the floor.
    yield_on_memory_critical: bool,
    /// The last reading showed critical memory pressure.
    memory_critical: bool,
}

impl DrainPacer {
    /// Always `pace`.
    #[cfg(test)]
    pub(crate) fn fixed(pace: DeleterPace) -> Self {
        Self {
            need: None,
            sample: HostPressure::default,
            pace,
            ceiling: None,
            share: 1,
            unpaced_cap: None,
            yield_on_memory_critical: false,
            memory_critical: false,
        }
    }

    /// The pace for `need`, from `sample` now and again during the drain.
    pub(crate) fn for_need(need: DrainNeed, sample: fn() -> HostPressure) -> Self {
        let host = sample();
        Self {
            need: Some(need),
            sample,
            pace: DeleterPace::for_need(need, host),
            ceiling: None,
            share: 1,
            unpaced_cap: PRIORITY_RAISED
                .load(Ordering::Relaxed)
                .then_some(DELETER_CATCHUP_UNLINKS_PER_SECOND),
            yield_on_memory_critical: false,
            memory_critical: host.memory_critical,
        }
    }

    /// Hold the unpaced mode to `cap` unlinks a second (and the configured
    /// ceiling); `None` leaves it unpaced.
    #[cfg(test)]
    pub(crate) fn with_unpaced_cap(mut self, cap: Option<u32>) -> Self {
        self.unpaced_cap = cap;
        self
    }

    /// Pace one of `threads` deleter threads: its share of the rate, so the
    /// threads together stay within it.
    pub(crate) fn with_share(mut self, threads: u32) -> Self {
        self.share = threads.max(1);
        self
    }

    /// Stop at the first reading of critical memory pressure instead of
    /// slowing to the floor.
    pub(crate) fn yielding_on_memory_critical(mut self) -> Self {
        self.yield_on_memory_critical = true;
        self
    }

    /// Whether the drain must stop now for memory pressure.
    fn must_yield(&self) -> bool {
        self.yield_on_memory_critical && self.memory_critical
    }

    /// Hold every paced mode at or below `ceiling` unlinks a second (a
    /// configured rate; `None` leaves the paces as they are).
    pub(crate) fn with_ceiling(mut self, ceiling: Option<u32>) -> Self {
        self.ceiling = ceiling.filter(|rate| *rate > 0);
        self
    }

    pub(crate) fn pace(&self) -> DeleterPace {
        self.pace
    }

    pub(crate) fn need(&self) -> Option<DrainNeed> {
        self.need
    }

    pub(crate) fn ceiling(&self) -> Option<u32> {
        self.ceiling
    }

    /// The rate the current pace allows, held to the configured ceiling;
    /// `None` when unpaced.
    pub(crate) fn unlinks_per_second(&self) -> Option<u32> {
        self.pace
            .unlinks_per_second()
            .or(self.unpaced_cap)
            .map(|rate| self.ceiling.map_or(rate, |ceiling| rate.min(ceiling)))
            .map(|rate| (rate / self.share.max(1)).max(1))
    }

    fn resample(&mut self) -> DeleterPace {
        if let Some(need) = self.need {
            let host = (self.sample)();
            self.memory_critical = host.memory_critical;
            self.pace = DeleterPace::for_need(need, host);
        }
        self.pace
    }
}

/// The most a limiter may fall behind its rate (a stall of the volume, a
/// slow directory) and catch up without sleeping: the rate is a ceiling
/// over any interval longer than this, never an average that a stall
/// banks for a later burst.
const DELETER_MAX_RATE_DEBT: Duration = Duration::from_millis(250);
/// The least a limiter sleeps once ahead, so pacing costs no syscall per
/// unlink.
const DELETER_MIN_SLEEP: Duration = Duration::from_millis(5);

/// The rate window of one limiter, on an explicit clock.
#[derive(Debug, Clone, Copy)]
struct RateWindow {
    per_second: Option<u32>,
    started: Instant,
    unlinks: u64,
}

impl RateWindow {
    fn new(per_second: Option<u32>, now: Instant) -> Self {
        Self {
            per_second,
            started: now,
            unlinks: 0,
        }
    }

    /// Count one unlink at `now` and return how long to sleep before it.
    /// A window more than [`DELETER_MAX_RATE_DEBT`] behind its rate moves
    /// its start forward, so what a stall left undone is never issued in
    /// one burst.
    fn admit(&mut self, now: Instant) -> Option<Duration> {
        self.unlinks = self.unlinks.saturating_add(1);
        let per_second = self.per_second.filter(|rate| *rate > 0)?;
        let due = Duration::from_secs_f64(self.unlinks as f64 / f64::from(per_second));
        let elapsed = now.saturating_duration_since(self.started);
        if let Some(ahead) = due.checked_sub(elapsed) {
            return (ahead >= DELETER_MIN_SLEEP).then_some(ahead);
        }
        let behind = elapsed - due;
        if let Some(forgiven) = behind.checked_sub(DELETER_MAX_RATE_DEBT) {
            self.started += forgiven;
        }
        None
    }
}

/// Holds unlinks to a rate by sleeping only when ahead of it, re-reading
/// the drain's pace every [`DELETER_PACE_RESAMPLE_EVERY`].
struct UnlinkLimiter<'a> {
    pacer: &'a mut DrainPacer,
    window: RateWindow,
    sampled: Instant,
    pace_changes: u64,
}

impl<'a> UnlinkLimiter<'a> {
    fn new(pacer: &'a mut DrainPacer) -> Self {
        let now = Instant::now();
        Self {
            window: RateWindow::new(pacer.unlinks_per_second(), now),
            pacer,
            sampled: now,
            pace_changes: 0,
        }
    }

    /// Start the rate window again (a new entry): time spent between
    /// entries never buys a burst.
    fn restart(&mut self) {
        self.window = RateWindow::new(self.window.per_second, Instant::now());
    }

    /// Pace one unlink; `false` when the drain must stop now for memory
    /// pressure (only a yielding pacer asks that).
    fn before_unlink(&mut self) -> bool {
        if self.sampled.elapsed() >= DELETER_PACE_RESAMPLE_EVERY {
            self.sampled = Instant::now();
            self.pacer.resample();
            let per_second = self.pacer.unlinks_per_second();
            if per_second != self.window.per_second {
                self.window = RateWindow::new(per_second, Instant::now());
                self.pace_changes = self.pace_changes.saturating_add(1);
            }
        }
        if self.pacer.must_yield() {
            return false;
        }
        if let Some(ahead) = self.window.admit(Instant::now()) {
            std::thread::sleep(ahead);
        }
        true
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

/// A catch-up lane lock (the host lane, or a repository's lane-process
/// lock). Its holder line names the process and repository while it is
/// held, and is emptied, still under the lock, when it is dropped: a line
/// that names a live process therefore means the lock is held, and other
/// processes read it without ever taking the lock.
pub(crate) struct CatchupLaneLock {
    lock: crate::file_lock::ExclusiveFileLock,
}

impl Drop for CatchupLaneLock {
    fn drop(&mut self) {
        crate::warn_unrecorded("clear the catch-up lane holder", self.lock.clear_holder());
    }
}

/// Take the catch-up lane lock at `path` if nobody holds it, naming this
/// process and `repository` in its holder line ([`catchup_lane_holder`]).
pub(crate) fn try_catchup_lane_lock(
    path: &Path,
    repository: &Path,
) -> std::io::Result<Option<CatchupLaneLock>> {
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
        "name the checkout trash catch-up lane",
        lock.record_holder(&format!(
            "pid {} since {}\n{CATCHUP_LANE_REPOSITORY_PREFIX}{}\n",
            std::process::id(),
            crate::clock::epoch_ms(),
            repository.display()
        )),
    );
    Ok(Some(CatchupLaneLock { lock }))
}

/// Holder-line prefix naming the repository whose catch-up lane holds the
/// lane lock.
const CATCHUP_LANE_REPOSITORY_PREFIX: &str = "repository ";

/// Who a catch-up lane lock's holder line names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatchupLaneHolder {
    pub(crate) pid: i64,
    /// `None` for a line written by something other than a lane (an older
    /// release's probe wrote `pid <n> since <ms>` alone).
    pub(crate) repository: Option<PathBuf>,
}

impl CatchupLaneHolder {
    /// The named process still exists.
    pub(crate) fn alive(&self) -> bool {
        crate::blockers::process_alive(self.pid)
    }
}

/// The holder line of the lane lock at `path`, read without taking the
/// lock; `None` when it is empty, missing or unreadable.
pub(crate) fn catchup_lane_holder(path: &Path) -> Option<CatchupLaneHolder> {
    let text = std::fs::read_to_string(path).ok()?;
    let pid = text.lines().find_map(|line| {
        line.strip_prefix("pid ")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })?;
    let repository = text.lines().find_map(|line| {
        line.strip_prefix(CATCHUP_LANE_REPOSITORY_PREFIX)
            .map(PathBuf::from)
    });
    Some(CatchupLaneHolder { pid, repository })
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
    /// Of `bytes`, those of entries that were regenerable directories
    /// stripped out of a closed checkout (measured as they were deleted).
    pub(crate) stripped_bytes: u64,
    /// Stripped-directory entries removed this run.
    pub(crate) stripped_entries_removed: usize,
    /// Of `bytes`, those of session scratch directories (measured as they
    /// were deleted); not part of `stripped_bytes`.
    pub(crate) scratch_bytes: u64,
    /// Session scratch entries removed this run.
    pub(crate) scratch_entries_removed: usize,
    /// Entries that failed this run.
    pub(crate) failed: Vec<String>,
    /// Entries that have failed at least [`TRASH_FAILURE_REPORT_AFTER`] times.
    pub(crate) reported: Vec<String>,
    /// Names in a trash the broker did not record (left alone).
    pub(crate) unrecognized: usize,
    /// The first of those names (entry names only).
    pub(crate) unrecognized_names: Vec<String>,
    /// Pending entries older than a day whose creator cannot be identified,
    /// so only an explicit `gc apply` can confirm them (entry names only).
    pub(crate) stale_pending: Vec<String>,
    /// Trash directories that failed verification (left alone).
    pub(crate) invalid_trash_dirs: usize,
    pub(crate) pace: Option<DeleterPace>,
    /// Recorded entries still in the trash after the run.
    pub(crate) entries_pending: usize,
    /// Entries left alone this run because a process had its working
    /// directory or an open file inside (a process that entered the checkout
    /// after the mover's observation keeps a now-trashed directory).
    pub(crate) entries_in_use: usize,
    /// The run's open-file observation could not be taken completely, so it
    /// deleted nothing.
    pub(crate) process_use_unknown: bool,
    /// The run's deadline stopped it with entries left (before an entry or
    /// part-way through one).
    pub(crate) budget_stopped: bool,
    /// Why the run needed its pace (`None` for a fixed pace).
    pub(crate) need: Option<DrainNeed>,
    /// The pace the run ended at (it follows host pressure while paced).
    pub(crate) final_pace: Option<DeleterPace>,
    /// Times the pace changed during the run.
    pub(crate) pace_changes: u64,
    /// When the oldest entry still recorded after the run was moved in.
    pub(crate) oldest_pending_created_ms: Option<i64>,
    /// The rate the run started at, after the configured ceiling (`None`
    /// when unpaced).
    pub(crate) initial_unlinks_per_second: Option<u32>,
    /// The configured ceiling the paced modes were held to, if any.
    pub(crate) configured_ceiling: Option<u32>,
    /// Names of the entries passed over because a process used them (at
    /// most [`TRASH_IN_USE_NAMES_MAX`]).
    pub(crate) in_use_names: Vec<String>,
    /// The run stopped, resumably, at a reading of critical memory pressure
    /// (only a yielding pacer stops for it).
    pub(crate) yielded_to_pressure: bool,
    /// The host-wide deleter lock was held elsewhere: the run deleted
    /// nothing.
    pub(crate) deleter_busy: bool,
    /// Deleter threads the run used (0 or 1: one).
    pub(crate) threads: u32,
    /// A retirement run skipped its drain because a catch-up lane was live
    /// on this host (it drains this trash anyway).
    pub(crate) skipped_for_catchup_lane: bool,
}

impl TrashDrainReport {
    /// Why the run ended: `deleter_busy`, `process_use_unknown`,
    /// `pressure`, `deadline`, or `finished` (every deletable entry was
    /// attempted).
    pub(crate) fn end_reason(&self) -> &'static str {
        if self.skipped_for_catchup_lane {
            "skipped_for_catchup_lane"
        } else if self.deleter_busy {
            "deleter_busy"
        } else if self.process_use_unknown {
            "process_use_unknown"
        } else if self.yielded_to_pressure {
            "pressure"
        } else if self.budget_stopped {
            "deadline"
        } else {
            "finished"
        }
    }
}

/// This process raised its own disk I/O priority (the worker's trash
/// catch-up lane process): its below-headroom drains are held to the
/// catch-up rate instead of running unpaced.
static PRIORITY_RAISED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that this process raised its disk I/O priority out of the
/// worker's throttle.
pub(crate) fn mark_priority_raised() {
    PRIORITY_RAISED.store(true, Ordering::Relaxed);
}

/// Wall time this process has spent in [`drain_trash_paced`], for the
/// drain's share of a worker run.
static DRAIN_NANOS_THIS_PROCESS: AtomicU64 = AtomicU64::new(0);

/// Wall time this process has spent draining the trash.
pub(crate) fn drain_time_this_process() -> Duration {
    Duration::from_nanos(DRAIN_NANOS_THIS_PROCESS.load(Ordering::Relaxed))
}

/// Bytes available to this user on the volume holding `fd`, or `None`
/// when they cannot be read. The deleter measures what an entry freed as
/// the change across its removal: with no per-file `stat` it cannot sum
/// file lengths, and the volume's own figure is what matters. Other writers
/// on the volume make it approximate; it never goes below zero.
fn volume_available_bytes(fd: RawFd) -> Option<u64> {
    // SAFETY: `statvfs` is a plain C struct whose zeroed representation is
    // valid; `fstatvfs` fills it before any field is read.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is a live descriptor and `stat` is writable for the call.
    if unsafe { libc::fstatvfs(fd, &mut stat) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
    let (available, fragment) = (u64::from(stat.f_bavail), stat.f_frsize as u64);
    Some(available.saturating_mul(fragment))
}

/// In-use entry names one run reports and the next run leaves out of the
/// backlog decision, at most.
pub(crate) const TRASH_IN_USE_NAMES_MAX: usize = 256;

/// The longest one deleter run's open-file observation may take.
const DELETER_PROCESS_USE_BUDGET: Duration = Duration::from_secs(10);

/// Apply what one drain did to the stored rows `current`: the drain read
/// `before` and left `after`. An entry it removed or forgot is dropped; for
/// an entry it kept, only the fields it changed (failures, pending) are
/// written, so a confirmation or a new row another process stored during
/// the drain survives.
pub(crate) fn apply_drain_rows(current: &mut TrashRows, before: &TrashRows, after: &TrashRows) {
    for (dir, entries) in before {
        for (name, was) in entries {
            match after.get(dir).and_then(|after| after.get(name)) {
                Some(now) => {
                    if let Some(stored) =
                        current.get_mut(dir).and_then(|stored| stored.get_mut(name))
                    {
                        if now.failures != was.failures {
                            stored.failures = now.failures;
                        }
                        if now.pending != was.pending {
                            stored.pending = now.pending;
                        }
                    }
                }
                None => {
                    if let Some(stored) = current.get_mut(dir) {
                        stored.remove(name);
                    }
                }
            }
        }
    }
    current.retain(|_, entries| !entries.is_empty());
}

/// The order one trash directory's confirmed, broker-named entries are
/// deleted in: fewest failures first (capped at
/// [`TRASH_FAILURE_REPORT_AFTER`]), then oldest first by name. An entry
/// that failed is retried after the others, so it never holds them up.
fn drain_order(entries: &BTreeMap<String, TrashRow>) -> Vec<String> {
    let mut order = entries
        .iter()
        .filter(|(name, row)| is_broker_entry_name(name) && !row.pending)
        .map(|(name, row)| (row.failures.min(TRASH_FAILURE_REPORT_AFTER), name.clone()))
        .collect::<Vec<_>>();
    order.sort();
    order.into_iter().map(|(_, name)| name).collect()
}

/// Delete recorded entries of the trash directories in `rows` oldest first
/// until `deadline`, at `pace` ([`drain_trash_paced`] with a fixed pace).
#[cfg(test)]
pub(crate) fn drain_trash(
    rows: &mut TrashRows,
    deadline: Instant,
    pace: DeleterPace,
) -> TrashDrainReport {
    drain_trash_paced(rows, deadline, &mut DrainPacer::fixed(pace))
}

/// Delete recorded entries of the trash directories in `rows` oldest first
/// (an entry that failed before goes after every entry that has failed
/// fewer times) until `deadline`, at the pace `pacer` sets; `rows` is
/// updated (removed entries dropped, failures counted, entries gone missing
/// forgotten). An entry in use or failing is passed over, never blocking
/// the entries behind it. The caller holds the deleter lock and persists
/// `rows`. Each trash directory is re-verified, each entry must have the
/// broker's name format and its recorded device and inode, re-checked on
/// the pinned directory immediately before deletion.
pub(crate) fn drain_trash_paced(
    rows: &mut TrashRows,
    deadline: Instant,
    pacer: &mut DrainPacer,
) -> TrashDrainReport {
    drain_trash_in(rows, deadline, pacer, DrainContext::default())
}

/// How one drain over (part of) the trash runs.
#[derive(Clone, Copy)]
struct DrainContext<'a> {
    /// Every recorded row, when `rows` holds only this thread's part: names
    /// another part owns are not unrecognized.
    known: Option<&'a TrashRows>,
    /// Count names nobody recorded (only one of several threads does).
    count_unrecognized: bool,
    /// An open-file observation taken once for all threads.
    process_use: Option<&'a crate::reclaim::OpenFileSnapshot>,
    /// A worker thread of a parallel drain: the caller accounts its wall
    /// time once.
    worker_thread: bool,
}

impl Default for DrainContext<'_> {
    fn default() -> Self {
        Self {
            known: None,
            count_unrecognized: true,
            process_use: None,
            worker_thread: false,
        }
    }
}

fn drain_trash_in(
    rows: &mut TrashRows,
    deadline: Instant,
    pacer: &mut DrainPacer,
    context: DrainContext<'_>,
) -> TrashDrainReport {
    let _span = crate::phase_telemetry::span("checkout_trash_drain");
    let pace = pacer.pace();
    let mut report = TrashDrainReport {
        pace: Some(pace),
        need: pacer.need(),
        initial_unlinks_per_second: pacer.unlinks_per_second(),
        configured_ceiling: pacer.ceiling(),
        ..TrashDrainReport::default()
    };
    match pace {
        DeleterPace::Unpaced => {
            crate::phase_telemetry::add("checkout_trash_unpaced_runs", Duration::ZERO);
        }
        DeleterPace::Floor => {
            crate::phase_telemetry::add("checkout_trash_floor_runs", Duration::ZERO);
        }
        DeleterPace::CatchupLoaded => {
            crate::phase_telemetry::add("checkout_trash_catchup_loaded_runs", Duration::ZERO);
        }
        DeleterPace::Catchup => {
            crate::phase_telemetry::add("checkout_trash_catchup_runs", Duration::ZERO);
        }
        DeleterPace::Steady => {}
    }
    let mut limiter = UnlinkLimiter::new(pacer);
    let started = Instant::now();
    // One process table, only when some row is still unconfirmed: it is
    // what proves a pending row's creator gone.
    let any_pending = rows
        .values()
        .any(|entries| entries.values().any(|row| row.pending));
    let table = any_pending
        .then(crate::session_holder::ProcessTable::snapshot)
        .flatten();
    // One open-file observation per run, taken before the first deletion:
    // an entry a live process still has as its working directory, or holds
    // a file open in, is never deleted. An incomplete observation deletes
    // nothing this run.
    let mut process_use: Option<crate::reclaim::OpenFileSnapshot> = context.process_use.cloned();
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
            // A listing that fails says nothing about which entries are
            // gone: the directory is skipped and its rows are kept.
            let Ok(listing) = std::fs::read_dir(trash.path()) else {
                report.invalid_trash_dirs += 1;
                continue;
            };
            let listed = listing
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                .filter(|name| name != TRASH_MARKER)
                .collect::<Vec<_>>();
            let known_here = context.known.and_then(|known| known.get(trash_path));
            for name in listed.iter().filter(|_| context.count_unrecognized) {
                let recorded = known_here.map_or_else(
                    || entries.contains_key(name),
                    |known| known.contains_key(name),
                );
                if !is_broker_entry_name(name) || !recorded {
                    report.unrecognized += 1;
                    if report.unrecognized_names.len() < 20 {
                        report.unrecognized_names.push(name.clone());
                    }
                    crate::phase_telemetry::add("checkout_trash_unrecognized", Duration::ZERO);
                }
            }
            // A pending row is never deleted while its creator may still
            // run (the move may be mid-way). Once the creator is proven
            // gone, an entry that still matches the recorded identity is
            // confirmed; a mismatch, or no entry, drops the row.
            let mut dropped = Vec::new();
            for (name, row) in entries.iter_mut() {
                let present = listed.contains(name);
                if !row.pending {
                    if !present {
                        dropped.push(name.clone());
                    }
                    continue;
                }
                if stale_unconfirmable(name, row, crate::clock::epoch_ms())
                    && report.stale_pending.len() < 20
                {
                    report.stale_pending.push(name.clone());
                }
                if !creator_gone(row, table.as_ref()) {
                    continue;
                }
                let matches = present
                    && fstatat_nofollow(trash.trash.as_raw_fd(), name).is_ok_and(|stat| {
                        #[allow(clippy::unnecessary_cast)]
                        let identity = (stat.st_dev as u64, stat.st_ino as u64);
                        is_directory(&stat) && identity == (row.device, row.inode)
                    });
                if matches {
                    row.pending = false;
                    crate::phase_telemetry::add("checkout_trash_pending_confirmed", Duration::ZERO);
                } else {
                    dropped.push(name.clone());
                }
            }
            for name in dropped {
                entries.remove(&name);
            }
            for name in drain_order(entries) {
                if Instant::now() >= deadline {
                    report.budget_stopped = true;
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
                // The pinned directory must still be this trash's entry, at
                // the start and every batch: anything that moved it away
                // stops its deletion.
                let trash_fd = trash.trash.as_raw_fd();
                let still_in_trash = || {
                    fstatat_nofollow(trash_fd, &name).is_ok_and(|stat| {
                        #[allow(clippy::unnecessary_cast)]
                        let identity = (stat.st_dev as u64, stat.st_ino as u64);
                        identity == (row.device, row.inode)
                    })
                };
                if !still_in_trash() {
                    report.unrecognized += 1;
                    continue;
                }
                let observation = process_use.get_or_insert_with(|| {
                    let budget = deadline
                        .saturating_duration_since(Instant::now())
                        .min(DELETER_PROCESS_USE_BUDGET);
                    let started = Instant::now();
                    let observed =
                        crate::session_holder::trash_deleter_process_use_snapshot(budget);
                    crate::phase_telemetry::add(
                        "checkout_trash_process_snapshot",
                        started.elapsed(),
                    );
                    observed
                });
                if let crate::reclaim::OpenFileSnapshot::Incomplete { .. } = observation {
                    report.process_use_unknown = true;
                    crate::phase_telemetry::add(
                        "checkout_trash_process_use_unknown",
                        Duration::ZERO,
                    );
                    break 'dirs;
                }
                if crate::reclaim::open_file_snapshot_blocker_for_worktree(observation, &path)
                    .is_some()
                {
                    report.entries_in_use += 1;
                    if report.in_use_names.len() < TRASH_IN_USE_NAMES_MAX {
                        report.in_use_names.push(name.clone());
                    }
                    crate::phase_telemetry::add("checkout_trash_entry_in_use", Duration::ZERO);
                    continue;
                }
                let left_trash = std::cell::Cell::new(false);
                let yielded = std::cell::Cell::new(false);
                limiter.restart();
                let mut since_check = 0_u32;
                let available_before = volume_available_bytes(trash_fd);
                // Listing types, no per-file `stat`: the trash is private
                // to the broker and nothing else writes into an entry.
                let removed =
                    pinned.remove_trusting_listing_types_with_guard(Some(deadline), || {
                        since_check += 1;
                        if since_check >= TRASH_LOCATION_CHECK_EVERY {
                            since_check = 0;
                            if !still_in_trash() {
                                left_trash.set(true);
                                return Err(std::io::Error::other("the entry left the trash"));
                            }
                        }
                        if !limiter.before_unlink() {
                            yielded.set(true);
                            return Err(std::io::Error::other("critical memory pressure"));
                        }
                        Ok(())
                    });
                let bytes = available_before
                    .zip(volume_available_bytes(trash_fd))
                    .map_or(0, |(before, after)| after.saturating_sub(before));
                match removed.map(|(outcome, unlinks)| (outcome, unlinks, bytes)) {
                    Ok((crate::gc::TreeRemoval::Complete, unlinks, bytes)) => {
                        report.entries_removed += 1;
                        report.unlinks = report.unlinks.saturating_add(unlinks);
                        report.bytes = report.bytes.saturating_add(bytes);
                        if row.scratch {
                            report.scratch_entries_removed += 1;
                            report.scratch_bytes = report.scratch_bytes.saturating_add(bytes);
                            crate::phase_telemetry::add(
                                "session_scratch_trash_entries_removed",
                                Duration::ZERO,
                            );
                        } else if row.stripped {
                            report.stripped_entries_removed += 1;
                            report.stripped_bytes = report.stripped_bytes.saturating_add(bytes);
                        }
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
                        if row.scratch {
                            report.scratch_bytes = report.scratch_bytes.saturating_add(bytes);
                        } else if row.stripped {
                            report.stripped_bytes = report.stripped_bytes.saturating_add(bytes);
                        }
                        report.budget_stopped = true;
                        crate::phase_telemetry::add(
                            "checkout_trash_entry_interrupted",
                            Duration::ZERO,
                        );
                        break 'dirs;
                    }
                    Err(error) if yielded.get() => {
                        report.unlinks = report.unlinks.saturating_add(error.entries_reclaimed);
                        report.bytes = report.bytes.saturating_add(bytes);
                        report.yielded_to_pressure = true;
                        crate::phase_telemetry::add(
                            "checkout_trash_yielded_to_pressure",
                            Duration::ZERO,
                        );
                        break 'dirs;
                    }
                    Err(error) if left_trash.get() => {
                        report.unlinks = report.unlinks.saturating_add(error.entries_reclaimed);
                        report.unrecognized += 1;
                        entries.remove(&name);
                        crate::phase_telemetry::add(
                            "checkout_trash_entry_moved_away",
                            Duration::ZERO,
                        );
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
            if entries.is_empty() {
                remove_trash_if_empty(&trash);
            }
        }
    });
    crate::phase_telemetry::add("checkout_trash_drain_time", started.elapsed());
    if !context.worker_thread {
        DRAIN_NANOS_THIS_PROCESS.fetch_add(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
    if report.budget_stopped {
        crate::phase_telemetry::add("checkout_trash_budget_stops", Duration::ZERO);
    }
    report.pace_changes = limiter.pace_changes;
    report.final_pace = Some(limiter.pacer.pace());
    rows.retain(|_, entries| !entries.is_empty());
    report.entries_pending = rows.values().map(BTreeMap::len).sum();
    report
}

/// Deleter threads a catching-up drain uses, each over its own entries.
/// The volume, not the CPU, bounds unlinks (the worker sat blocked in the
/// kernel on an external USB volume); several outstanding unlinks on
/// different directories let the device queue them.
pub(crate) const TRASH_CATCHUP_THREADS: u32 = 3;

/// Split `rows` into `parts` row sets: each directory's confirmed entries
/// dealt round-robin in drain order (so every part starts with the oldest
/// it can), and every other row (pending, foreign names) in the first part,
/// which alone confirms pending rows.
fn partition_rows(rows: &TrashRows, parts: usize) -> Vec<TrashRows> {
    let parts = parts.max(1);
    let mut split = vec![TrashRows::new(); parts];
    for (dir, entries) in rows {
        let order = drain_order(entries);
        for (index, name) in order.iter().enumerate() {
            if let Some(row) = entries.get(name) {
                split[index % parts]
                    .entry(dir.clone())
                    .or_default()
                    .insert(name.clone(), *row);
            }
        }
        for (name, row) in entries {
            if !order.contains(name) {
                split[0]
                    .entry(dir.clone())
                    .or_default()
                    .insert(name.clone(), *row);
            }
        }
    }
    split
}

/// One report for a parallel drain's parts.
fn merge_drain_reports(parts: Vec<TrashDrainReport>, threads: u32) -> TrashDrainReport {
    let mut merged = TrashDrainReport::default();
    for (index, part) in parts.into_iter().enumerate() {
        if index == 0 {
            merged.pace = part.pace;
            merged.need = part.need;
            merged.final_pace = part.final_pace;
            merged.configured_ceiling = part.configured_ceiling;
            merged.initial_unlinks_per_second = part
                .initial_unlinks_per_second
                .map(|rate| rate.saturating_mul(threads));
        }
        merged.entries_removed += part.entries_removed;
        merged.entries_interrupted += part.entries_interrupted;
        merged.unlinks = merged.unlinks.saturating_add(part.unlinks);
        // Each thread's per-entry bytes are the volume's change while the
        // other threads delete too: summing them counts the same space up
        // to `threads` times. The caller measures the slice once instead,
        // and stripped and scratch bytes stay out of the ledger.
        merged.stripped_entries_removed += part.stripped_entries_removed;
        merged.scratch_entries_removed += part.scratch_entries_removed;
        merged.failed.extend(part.failed);
        merged.reported.extend(part.reported);
        merged.unrecognized += part.unrecognized;
        merged.unrecognized_names.extend(part.unrecognized_names);
        merged.stale_pending.extend(part.stale_pending);
        merged.invalid_trash_dirs = merged.invalid_trash_dirs.max(part.invalid_trash_dirs);
        merged.entries_in_use += part.entries_in_use;
        merged.in_use_names.extend(part.in_use_names);
        merged.process_use_unknown |= part.process_use_unknown;
        merged.budget_stopped |= part.budget_stopped;
        merged.yielded_to_pressure |= part.yielded_to_pressure;
        merged.pace_changes = merged.pace_changes.saturating_add(part.pace_changes);
    }
    merged.unrecognized_names.truncate(20);
    merged.in_use_names.truncate(TRASH_IN_USE_NAMES_MAX);
    merged.threads = threads;
    merged
}

/// Bytes available to this user, summed over the distinct volumes holding
/// the trash directories `dirs`; `None` when any cannot be read.
pub(crate) fn trash_volumes_available(dirs: &[PathBuf]) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let mut seen = std::collections::BTreeSet::new();
    let mut total = 0_u64;
    for dir in dirs {
        let metadata = std::fs::metadata(dir).ok()?;
        if !seen.insert(metadata.dev()) {
            continue;
        }
        let trash = open_dir_nofollow(dir).ok()?;
        total = total.saturating_add(volume_available_bytes(trash.as_raw_fd())?);
    }
    Some(total)
}

/// Drain `rows` with `threads` deleter threads, each over its own part
/// ([`partition_rows`]) with a pacer from `pacer` (give each its share of
/// the rate, [`DrainPacer::with_share`]). One open-file observation is
/// taken for all of them (`process_use`, or a fresh one); each thread
/// records into this thread's telemetry. A thread that panics leaves its
/// part's rows as they were. `rows` is updated as [`drain_trash_paced`]
/// updates it. `bytes` is the change in `available` (bytes free on the
/// trash volumes) from before the threads start to after they join,
/// measured once for the slice; `stripped_bytes` and `scratch_bytes` stay
/// zero.
pub(crate) fn drain_trash_parallel(
    rows: &mut TrashRows,
    deadline: Instant,
    pacer: impl Fn() -> DrainPacer,
    threads: u32,
    process_use: Option<crate::reclaim::OpenFileSnapshot>,
    available: impl Fn() -> Option<u64>,
) -> TrashDrainReport {
    let started = Instant::now();
    let available_before = available();
    let process_use = process_use.unwrap_or_else(|| {
        let budget = deadline
            .saturating_duration_since(Instant::now())
            .min(DELETER_PROCESS_USE_BUDGET);
        let observed = crate::session_holder::trash_deleter_process_use_snapshot(budget);
        crate::phase_telemetry::add("checkout_trash_process_snapshot", started.elapsed());
        observed
    });
    let known = rows.clone();
    let parts = partition_rows(rows, threads as usize);
    let recorder = crate::phase_telemetry::active();
    let results = std::thread::scope(|scope| {
        let handles = parts
            .into_iter()
            .enumerate()
            .map(|(index, mut part)| {
                let original = part.clone();
                let mut pacer = pacer();
                let known = &known;
                let process_use = &process_use;
                let recorder = recorder.clone();
                let handle = scope.spawn(move || {
                    let _telemetry = recorder.map(crate::phase_telemetry::install);
                    let report = drain_trash_in(
                        &mut part,
                        deadline,
                        &mut pacer,
                        DrainContext {
                            known: Some(known),
                            count_unrecognized: index == 0,
                            process_use: Some(process_use),
                            worker_thread: true,
                        },
                    );
                    (part, report)
                });
                (original, handle)
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|(original, handle)| {
                handle.join().unwrap_or_else(|_| {
                    crate::phase_telemetry::add("checkout_trash_thread_panicked", Duration::ZERO);
                    (original, TrashDrainReport::default())
                })
            })
            .collect::<Vec<_>>()
    });
    let mut merged_rows = TrashRows::new();
    let mut reports = Vec::with_capacity(results.len());
    for (part, report) in results {
        for (dir, entries) in part {
            merged_rows.entry(dir).or_default().extend(entries);
        }
        reports.push(report);
    }
    merged_rows.retain(|_, entries| !entries.is_empty());
    *rows = merged_rows;
    let mut report = merge_drain_reports(reports, threads);
    report.bytes = available_before
        .zip(available())
        .map_or(0, |(before, after)| after.saturating_sub(before));
    report.entries_pending = rows.values().map(BTreeMap::len).sum();
    DRAIN_NANOS_THIS_PROCESS.fetch_add(
        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
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
            move_worktree_to_trash(
                &self.repo,
                &self.common(),
                &self.pool,
                checkout,
                session_id,
                &mut |_, _, _| Ok(()),
            )
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
            &mut |_, _, _| Ok(()),
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
        assert!(stopped.budget_stopped, "the budget stop is reported");

        let resumed = drain_trash(
            &mut rows,
            Instant::now() + Duration::from_secs(600),
            DeleterPace::Unpaced,
        );
        assert_eq!(resumed.entries_removed, 1, "{resumed:?}");
        assert!(!trashed.path().exists());
        assert_eq!(resumed.entries_pending, 0);
        assert!(!resumed.budget_stopped, "{resumed:?}");
    }

    thread_local! {
        static SCRIPTED_PRESSURE: std::cell::Cell<HostPressure> =
            const { std::cell::Cell::new(HostPressure { loaded: false, memory_warning: false, memory_critical: false }) };
    }

    fn scripted_pressure() -> HostPressure {
        SCRIPTED_PRESSURE.with(std::cell::Cell::get)
    }

    fn set_scripted_pressure(pressure: HostPressure) {
        SCRIPTED_PRESSURE.with(|cell| cell.set(pressure));
    }

    fn row_for_backlog(failures: u32, pending: bool) -> TrashRow {
        TrashRow {
            device: 1,
            inode: 1,
            session_id: 1,
            failures,
            pending,
            creator_pid: 0,
            creator_started_ms: 0,
            stripped: true,
            scratch: false,
        }
    }

    /// A drain writes back only what it changed: a confirmation stored by
    /// another process during the drain is not overwritten with the
    /// drain's stale `pending`, a new row survives, a failure the drain
    /// counted and a confirmation it made are applied, and an entry it
    /// removed is dropped.
    #[test]
    fn a_drain_writes_back_only_the_fields_it_changed() {
        let dir = PathBuf::from("/pool").join(TRASH_DIR);
        let name = |nonce: u32| format!("1790000000000-1-1-{nonce}");
        let mut before = TrashRows::new();
        for (nonce, pending) in [(1, true), (2, false), (3, true), (4, false)] {
            before
                .entry(dir.clone())
                .or_default()
                .insert(name(nonce), row_for_backlog(0, pending));
        }
        // The drain: 1 still pending (its creator lives), 2 failed once,
        // 3 confirmed, 4 removed.
        let mut after = before.clone();
        let entries = after.get_mut(&dir).unwrap();
        entries.get_mut(&name(2)).unwrap().failures = 1;
        entries.get_mut(&name(3)).unwrap().pending = false;
        entries.remove(&name(4));
        // Meanwhile another process confirmed 1 and recorded 5.
        let mut current = before.clone();
        let stored = current.get_mut(&dir).unwrap();
        stored.get_mut(&name(1)).unwrap().pending = false;
        stored.insert(name(5), row_for_backlog(0, true));

        apply_drain_rows(&mut current, &before, &after);

        let stored = &current[&dir];
        assert!(
            !stored[&name(1)].pending,
            "the confirmation made meanwhile survives"
        );
        assert_eq!(stored[&name(2)].failures, 1);
        assert!(!stored[&name(3)].pending);
        assert!(!stored.contains_key(&name(4)));
        assert!(
            stored[&name(5)].pending,
            "a row recorded meanwhile survives"
        );
    }

    /// The trash is behind once it holds [`TRASH_BACKLOG_ENTRIES`] entries
    /// or one a day old; unconfirmed rows and entries already reported as
    /// failing do not count.
    #[test]
    fn a_growing_or_old_backlog_puts_the_trash_behind() {
        let now = 1_800_000_000_000_i64;
        let dir = PathBuf::from("/pool").join(TRASH_DIR);
        let name = |created: i64, nonce: usize| format!("{created:013}-1-1-{nonce}");
        let mut rows = TrashRows::new();
        for nonce in 0..TRASH_BACKLOG_ENTRIES - 1 {
            rows.entry(dir.clone())
                .or_default()
                .insert(name(now - 60_000, nonce), row_for_backlog(0, false));
        }
        rows.entry(dir.clone())
            .or_default()
            .insert(name(now - 120_000, 900), row_for_backlog(0, true));
        rows.entry(dir.clone()).or_default().insert(
            name(now - 120_000, 901),
            row_for_backlog(TRASH_FAILURE_REPORT_AFTER, false),
        );
        let small = TrashBacklog::of(&rows);
        assert_eq!(small.entries, TRASH_BACKLOG_ENTRIES - 1, "{small:?}");
        assert_eq!(small.oldest_age_ms(now), Some(60_000));
        assert!(!small.behind(now), "a small, recent backlog stays gentle");

        rows.entry(dir.clone())
            .or_default()
            .insert(name(now - 30_000, 999), row_for_backlog(0, false));
        let grown = TrashBacklog::of(&rows);
        assert_eq!(grown.entries, TRASH_BACKLOG_ENTRIES);
        assert!(grown.behind(now), "growth past the threshold is behind");

        let mut old = TrashRows::new();
        old.entry(dir).or_default().insert(
            name(now - TRASH_BACKLOG_AGE_MS, 0),
            row_for_backlog(0, false),
        );
        let old = TrashBacklog::of(&old);
        assert_eq!(old.oldest_age_ms(now), Some(TRASH_BACKLOG_AGE_MS));
        assert!(old.behind(now), "one entry a day old is behind");
        assert!(!TrashBacklog::default().behind(now));
    }

    /// Behind, the pace rises above the steady rate, held lower under load
    /// or memory warning and to the floor at critical memory pressure, and
    /// never above the catch-up maximum outside disk pressure; with no
    /// backlog it stays the gentle pace.
    #[test]
    fn a_backlog_raises_the_pace_within_its_maximum() {
        let clear = HostPressure::default();
        let loaded = HostPressure {
            loaded: true,
            ..HostPressure::default()
        };
        let warning = HostPressure {
            memory_warning: true,
            ..HostPressure::default()
        };
        let critical = HostPressure {
            memory_critical: true,
            loaded: true,
            ..HostPressure::default()
        };
        assert_eq!(
            DeleterPace::for_need(DrainNeed::Gentle, clear),
            DeleterPace::Steady
        );
        assert_eq!(
            DeleterPace::for_need(DrainNeed::Gentle, loaded),
            DeleterPace::Floor
        );
        assert_eq!(
            DeleterPace::for_need(DrainNeed::Gentle, warning),
            DeleterPace::Steady,
            "the gentle pace ignores memory, as before"
        );
        assert_eq!(
            DeleterPace::for_need(DrainNeed::Backlog, clear),
            DeleterPace::Catchup
        );
        assert_eq!(
            DeleterPace::for_need(DrainNeed::Backlog, loaded),
            DeleterPace::CatchupLoaded
        );
        assert_eq!(
            DeleterPace::for_need(DrainNeed::Backlog, warning),
            DeleterPace::CatchupLoaded
        );
        assert_eq!(
            DeleterPace::for_need(DrainNeed::Backlog, critical),
            DeleterPace::Floor
        );
        assert_eq!(
            DeleterPace::for_need(DrainNeed::DiskPressure, critical),
            DeleterPace::Unpaced
        );
        for pace in [
            DeleterPace::Steady,
            DeleterPace::Floor,
            DeleterPace::CatchupLoaded,
            DeleterPace::Catchup,
        ] {
            let rate = pace.unlinks_per_second().unwrap();
            assert!(rate <= DELETER_CATCHUP_UNLINKS_PER_SECOND, "{pace:?}");
            assert!(rate >= DELETER_FLOOR_UNLINKS_PER_SECOND, "{pace:?}");
        }
        const {
            assert!(DELETER_CATCHUP_LOADED_UNLINKS_PER_SECOND > DELETER_STEADY_UNLINKS_PER_SECOND);
            assert!(DELETER_CATCHUP_UNLINKS_PER_SECOND > DELETER_CATCHUP_LOADED_UNLINKS_PER_SECOND);
        };
        assert_eq!(DrainNeed::of(true, true), DrainNeed::DiskPressure);
        assert_eq!(DrainNeed::of(false, true), DrainNeed::Backlog);
        assert_eq!(DrainNeed::of(false, false), DrainNeed::Gentle);
    }

    /// A catching-up drain reads the host's pressure again and follows it;
    /// a fixed pace never changes.
    #[test]
    fn a_catching_up_pacer_follows_host_pressure() {
        set_scripted_pressure(HostPressure::default());
        let mut pacer = DrainPacer::for_need(DrainNeed::Backlog, scripted_pressure);
        assert_eq!(pacer.pace(), DeleterPace::Catchup);
        set_scripted_pressure(HostPressure {
            loaded: true,
            ..HostPressure::default()
        });
        assert_eq!(pacer.resample(), DeleterPace::CatchupLoaded);
        set_scripted_pressure(HostPressure {
            memory_critical: true,
            ..HostPressure::default()
        });
        assert_eq!(pacer.resample(), DeleterPace::Floor);
        set_scripted_pressure(HostPressure::default());
        assert_eq!(pacer.resample(), DeleterPace::Catchup);
        let mut fixed = DrainPacer::fixed(DeleterPace::Steady);
        assert_eq!(fixed.resample(), DeleterPace::Steady);
        assert_eq!(fixed.need(), None);
    }

    /// A stall inside one entry (the volume blocking unlinks for 10 s) is
    /// not banked: once it ends the limiter lets through at most the bounded
    /// debt before sleeping again, and about the rate over the next second,
    /// instead of the ~80,000 unlinks the stall left undone in one burst.
    #[test]
    fn a_stall_never_buys_an_unpaced_burst() {
        let rate = DELETER_CATCHUP_UNLINKS_PER_SECOND;
        let per_interval =
            |interval: Duration| (f64::from(rate) * interval.as_secs_f64()).ceil() as u64;
        let start = Instant::now();
        let mut now = start;
        let mut window = RateWindow::new(Some(rate), now);
        // A second at the rate (unlinks themselves take no time here).
        while now < start + Duration::from_secs(1) {
            if let Some(sleep) = window.admit(now) {
                now += sleep;
            }
        }
        // The volume stalls for 10 s.
        now += Duration::from_secs(10);
        let stall_end = now;
        let mut burst = 0_u64;
        loop {
            match window.admit(now) {
                None => burst += 1,
                Some(sleep) => {
                    now += sleep;
                    break;
                }
            }
            assert!(burst < 1_000_000, "the limiter never slept again");
        }
        assert!(
            burst <= per_interval(DELETER_MAX_RATE_DEBT) + per_interval(DELETER_MIN_SLEEP) + 8,
            "{burst} unlinks issued without a sleep after the stall"
        );
        let mut next_second = burst + 1;
        while now < stall_end + Duration::from_secs(1) {
            if let Some(sleep) = window.admit(now) {
                now += sleep;
            }
            next_second += 1;
        }
        assert!(
            next_second
                <= per_interval(Duration::from_secs(1))
                    + per_interval(DELETER_MAX_RATE_DEBT)
                    + per_interval(DELETER_MIN_SLEEP)
                    + 8,
            "{next_second} unlinks in the second after the stall"
        );
        // Unpaced never sleeps.
        let mut unpaced = RateWindow::new(None, start);
        assert_eq!(unpaced.admit(start), None);
    }

    /// In a process that raised its disk I/O priority, the below-headroom
    /// mode is held to the catch-up rate (and the configured ceiling)
    /// instead of running unpaced; elsewhere it stays unpaced.
    #[test]
    fn a_raised_process_caps_the_unpaced_mode() {
        set_scripted_pressure(HostPressure::default());
        let raised = DrainPacer::for_need(DrainNeed::DiskPressure, scripted_pressure)
            .with_unpaced_cap(Some(DELETER_CATCHUP_UNLINKS_PER_SECOND));
        assert_eq!(raised.pace(), DeleterPace::Unpaced);
        assert_eq!(
            raised.unlinks_per_second(),
            Some(DELETER_CATCHUP_UNLINKS_PER_SECOND)
        );
        let ceiling = DrainPacer::for_need(DrainNeed::DiskPressure, scripted_pressure)
            .with_unpaced_cap(Some(DELETER_CATCHUP_UNLINKS_PER_SECOND))
            .with_ceiling(Some(500));
        assert_eq!(ceiling.unlinks_per_second(), Some(500));
        let worker =
            DrainPacer::for_need(DrainNeed::DiskPressure, scripted_pressure).with_unpaced_cap(None);
        assert_eq!(worker.unlinks_per_second(), None);
        let paced = DrainPacer::for_need(DrainNeed::Gentle, scripted_pressure)
            .with_unpaced_cap(Some(DELETER_CATCHUP_UNLINKS_PER_SECOND));
        assert_eq!(
            paced.unlinks_per_second(),
            Some(DELETER_STEADY_UNLINKS_PER_SECOND)
        );
    }

    /// An explicitly configured rate holds every paced mode at or below it;
    /// a higher one changes nothing, and below the headroom it stays
    /// unpaced.
    #[test]
    fn a_configured_ceiling_holds_every_paced_mode() {
        set_scripted_pressure(HostPressure::default());
        let capped =
            DrainPacer::for_need(DrainNeed::Backlog, scripted_pressure).with_ceiling(Some(500));
        assert_eq!(capped.pace(), DeleterPace::Catchup);
        assert_eq!(capped.unlinks_per_second(), Some(500));
        assert_eq!(capped.ceiling(), Some(500));
        let gentle =
            DrainPacer::for_need(DrainNeed::Gentle, scripted_pressure).with_ceiling(Some(500));
        assert_eq!(gentle.unlinks_per_second(), Some(500));
        let higher = DrainPacer::for_need(DrainNeed::Backlog, scripted_pressure)
            .with_ceiling(Some(1_000_000));
        assert_eq!(
            higher.unlinks_per_second(),
            Some(DELETER_CATCHUP_UNLINKS_PER_SECOND)
        );
        let unpaced = DrainPacer::for_need(DrainNeed::DiskPressure, scripted_pressure)
            .with_ceiling(Some(500));
        assert_eq!(unpaced.unlinks_per_second(), None);
        let none = DrainPacer::for_need(DrainNeed::Backlog, scripted_pressure).with_ceiling(None);
        assert_eq!(
            none.unlinks_per_second(),
            Some(DELETER_CATCHUP_UNLINKS_PER_SECOND)
        );
    }

    /// An entry the last run passed over as in use, however old, does not
    /// keep the trash behind; the rest of the backlog still counts.
    #[test]
    fn an_entry_in_use_does_not_keep_the_trash_behind() {
        let now = 1_800_000_000_000_i64;
        let dir = PathBuf::from("/pool").join(TRASH_DIR);
        let held = format!("{:013}-1-1-0", now - 2 * TRASH_BACKLOG_AGE_MS);
        let fresh = format!("{:013}-1-1-1", now - 60_000);
        let mut rows = TrashRows::new();
        for name in [&held, &fresh] {
            rows.entry(dir.clone())
                .or_default()
                .insert(name.clone(), row_for_backlog(0, false));
        }
        assert!(TrashBacklog::of(&rows).behind(now), "counted, it is behind");
        let in_use = std::collections::BTreeSet::from([held.clone()]);
        let deletable = TrashBacklog::deletable(&rows, &in_use);
        assert_eq!(deletable.entries, 1, "{deletable:?}");
        assert_eq!(deletable.oldest_age_ms(now), Some(60_000));
        assert!(!deletable.behind(now));
    }

    /// Several deleter threads each take their own entries: every recorded
    /// entry goes, the one a process uses stays (and is retried), an
    /// unrecorded name is counted once and left alone, no symlink is
    /// followed, and each thread paces itself at its share of the rate.
    #[test]
    fn a_parallel_drain_splits_the_entries_and_keeps_every_guard() {
        let fixture = Fixture::new();
        let outside = fixture._dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("precious.txt"), "precious\n").unwrap();
        let mut rows = TrashRows::new();
        let mut trashed = Vec::new();
        for index in 0..5 {
            let checkout = fixture.checkout(&format!("parallel-{index}"));
            std::os::unix::fs::symlink(&outside, checkout.join("link")).unwrap();
            std::fs::create_dir_all(checkout.join("node_modules/dep")).unwrap();
            std::fs::write(checkout.join("node_modules/dep/index.js"), "x").unwrap();
            let moved = fixture.trash(&checkout, index + 1).unwrap();
            rows.entry(moved.trash_dir.clone())
                .or_default()
                .insert(moved.entry.clone(), moved.row);
            trashed.push(moved);
        }
        let stranger = trashed[0].trash_dir.join("0000000000001-9-9-9");
        std::fs::create_dir(&stranger).unwrap();
        let held = std::fs::canonicalize(trashed[2].path()).unwrap();
        let in_use =
            crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                pid: std::process::id(),
                command: "shell".into(),
                descriptor: "cwd".into(),
                file_type: Some("DIR".into()),
                path: held.clone(),
            }]);
        let parts = partition_rows(&rows, 3);
        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts
                .iter()
                .map(|part| part.values().map(BTreeMap::len).sum::<usize>())
                .sum::<usize>(),
            5,
            "every row in exactly one part"
        );
        assert_eq!(
            DrainPacer::fixed(DeleterPace::Catchup)
                .with_share(3)
                .unlinks_per_second(),
            Some(DELETER_CATCHUP_UNLINKS_PER_SECOND / 3)
        );

        // The slice's free-space change is measured once, before the
        // threads start and after they join: here a scripted 1,000 then
        // 5,000 bytes, whatever each thread's per-entry deltas were.
        let readings = std::cell::Cell::new(0_u64);
        let report = drain_trash_parallel(
            &mut rows,
            Instant::now() + Duration::from_secs(600),
            || DrainPacer::fixed(DeleterPace::Unpaced),
            3,
            Some(in_use),
            || {
                readings.set(readings.get() + 1);
                Some(if readings.get() == 1 { 1_000 } else { 5_000 })
            },
        );

        assert_eq!(readings.get(), 2, "measured once before and once after");
        assert_eq!(report.bytes, 4_000, "{report:?}");
        assert_eq!(report.stripped_bytes, 0, "{report:?}");
        assert_eq!(report.scratch_bytes, 0, "{report:?}");
        assert_eq!(report.threads, 3);
        assert_eq!(report.entries_removed, 4, "{report:?}");
        assert_eq!(report.entries_in_use, 1, "{report:?}");
        assert_eq!(report.unrecognized, 1, "{report:?}");
        assert_eq!(report.entries_pending, 1, "{report:?}");
        assert!(held.exists());
        assert!(stranger.exists());
        for (index, moved) in trashed.iter().enumerate() {
            assert_eq!(moved.path().exists(), index == 2, "{index}");
        }
        assert_eq!(
            std::fs::read_to_string(outside.join("precious.txt")).unwrap(),
            "precious\n"
        );
        let left = rows
            .values()
            .flat_map(|entries| entries.keys())
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(left, vec![trashed[2].entry.clone()]);
    }

    /// A drain catching up still deletes only recorded entries and never
    /// follows a symlink out of the trash.
    #[test]
    fn a_catching_up_drain_stays_inside_the_trash() {
        let fixture = Fixture::new();
        let checkout = fixture.checkout("one");
        let outside = fixture._dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("precious.txt"), "precious\n").unwrap();
        std::os::unix::fs::symlink(&outside, checkout.join("link")).unwrap();
        std::fs::create_dir_all(checkout.join("node_modules/dep")).unwrap();
        for index in 0..20 {
            std::fs::write(
                checkout.join(format!("node_modules/dep/file-{index}.js")),
                "x",
            )
            .unwrap();
        }
        let trashed = fixture.trash(&checkout, 1).unwrap();
        let stranger = trashed.trash_dir.join("0000000000001-9-9-9");
        std::fs::create_dir(&stranger).unwrap();
        std::fs::write(stranger.join("keep.txt"), "not ours\n").unwrap();
        let mut rows = rows_of(&trashed);
        set_scripted_pressure(HostPressure::default());
        let mut pacer = DrainPacer::for_need(DrainNeed::Backlog, scripted_pressure);

        let report = drain_trash_paced(
            &mut rows,
            Instant::now() + Duration::from_secs(600),
            &mut pacer,
        );

        assert_eq!(report.entries_removed, 1, "{report:?}");
        assert_eq!(report.pace, Some(DeleterPace::Catchup));
        assert_eq!(report.need, Some(DrainNeed::Backlog));
        assert!(!trashed.path().exists());
        assert_eq!(
            std::fs::read_to_string(outside.join("precious.txt")).unwrap(),
            "precious\n"
        );
        assert!(stranger.join("keep.txt").exists(), "{report:?}");
        assert_eq!(report.unrecognized, 1, "{report:?}");
    }

    /// An entry that failed before goes after the entries that failed
    /// fewer times, whatever its age, so it never holds them up.
    #[test]
    fn a_failing_entry_goes_after_the_others() {
        let mut entries = BTreeMap::new();
        entries.insert("1790000000001-1-1-1".to_string(), row_for_backlog(1, false));
        entries.insert("1790000000002-1-1-2".to_string(), row_for_backlog(0, false));
        entries.insert("1790000000003-1-1-3".to_string(), row_for_backlog(5, false));
        entries.insert("1790000000004-1-1-4".to_string(), row_for_backlog(0, false));
        entries.insert("1790000000000-1-1-0".to_string(), row_for_backlog(0, true));
        entries.insert("not-ours".to_string(), row_for_backlog(0, false));
        assert_eq!(
            drain_order(&entries),
            vec![
                "1790000000002-1-1-2".to_string(),
                "1790000000004-1-1-4".to_string(),
                "1790000000001-1-1-1".to_string(),
                "1790000000003-1-1-3".to_string(),
            ],
            "oldest first among equals; failed entries later; pending and foreign names never"
        );
    }

    /// A move whose environments cannot be destroyed changes nothing: no
    /// row is written and nothing enters the trash.
    #[test]
    fn a_failed_environment_destruction_moves_nothing() {
        let fixture = Fixture::new();
        let checkout = fixture.checkout("one");
        FAIL_ENVIRONMENT_DESTROY.with(|flag| flag.set(true));
        let mut recorded = 0;
        let result = move_worktree_to_trash(
            &fixture.repo,
            &fixture.common(),
            &fixture.pool,
            &checkout,
            3,
            &mut |_, _, _| {
                recorded += 1;
                Ok(())
            },
        );
        FAIL_ENVIRONMENT_DESTROY.with(|flag| flag.set(false));
        assert!(result.is_err());
        assert_eq!(recorded, 0, "no pending row");
        assert!(checkout.join("base.txt").exists());
        let entries = std::fs::read_dir(fixture.pool.join(TRASH_DIR))
            .map(|listing| {
                listing
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_name() != TRASH_MARKER)
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(entries, 0, "nothing entered the trash");
    }

    /// Move one checkout, capturing its pending row instead of confirming.
    fn move_unconfirmed(fixture: &Fixture, name: &str) -> (TrashedCheckout, TrashRow) {
        let checkout = fixture.checkout(name);
        let mut pending = None;
        let trashed = move_worktree_to_trash(
            &fixture.repo,
            &fixture.common(),
            &fixture.pool,
            &checkout,
            3,
            &mut |_, _, row| {
                pending = Some(row);
                Ok(())
            },
        )
        .unwrap();
        (trashed, pending.expect("a pending row was written first"))
    }

    fn rows_with(trashed: &TrashedCheckout, row: TrashRow) -> TrashRows {
        let mut rows = TrashRows::new();
        rows.entry(trashed.trash_dir.clone())
            .or_default()
            .insert(trashed.entry.clone(), row);
        rows
    }

    /// A drain running while a move is unconfirmed (its creator, this
    /// process, still runs) deletes nothing and keeps the row.
    #[test]
    fn an_unconfirmed_move_is_never_deleted() {
        let fixture = Fixture::new();
        let (trashed, pending) = move_unconfirmed(&fixture, "one");
        assert!(pending.pending);
        assert_eq!(
            (pending.device, pending.inode),
            (trashed.row.device, trashed.row.inode),
            "the source identity survives the rename"
        );
        let mut rows = rows_with(&trashed, pending);
        let report = drain_trash(
            &mut rows,
            Instant::now() + Duration::from_secs(600),
            DeleterPace::Unpaced,
        );
        assert_eq!(report.entries_removed, 0, "{report:?}");
        assert!(trashed.path().join("base.txt").exists());
        assert_eq!(rows.values().map(BTreeMap::len).sum::<usize>(), 1);
    }

    /// A pending row whose creator is gone is confirmed only when its entry
    /// still has the recorded identity; otherwise the row is dropped and the
    /// entry left alone.
    #[test]
    fn a_dead_creators_pending_row_is_confirmed_only_on_identity() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        let dead = i64::from(child.id());
        let fixture = Fixture::new();

        let (matching, mut row) = move_unconfirmed(&fixture, "one");
        row.creator_pid = dead;
        row.creator_started_ms = 1;
        let mut rows = rows_with(&matching, row);
        let report = drain_trash(
            &mut rows,
            Instant::now() + Duration::from_secs(600),
            DeleterPace::Unpaced,
        );
        assert_eq!(report.entries_removed, 1, "{report:?}");
        assert!(!matching.path().exists());

        let (mismatched, mut row) = move_unconfirmed(&fixture, "two");
        row.creator_pid = dead;
        row.creator_started_ms = 1;
        row.inode = row.inode.wrapping_add(1);
        let mut rows = rows_with(&mismatched, row);
        let report = drain_trash(
            &mut rows,
            Instant::now() + Duration::from_secs(600),
            DeleterPace::Unpaced,
        );
        assert_eq!(report.entries_removed, 0, "{report:?}");
        assert!(mismatched.path().join("base.txt").exists());
        assert!(rows.is_empty(), "the unmatched row is dropped");
    }

    /// A pending row whose creator cannot be identified is reported by name
    /// once it is a day old, and left alone.
    #[test]
    fn an_unconfirmable_pending_row_is_reported_after_a_day() {
        let fixture = Fixture::new();
        let (trashed, mut row) = move_unconfirmed(&fixture, "one");
        row.creator_started_ms = 0;
        let old = "0000000000001-3-1-1".to_string();
        std::fs::rename(trashed.path(), trashed.trash_dir.join(&old)).unwrap();
        let mut rows = TrashRows::new();
        rows.entry(trashed.trash_dir.clone())
            .or_default()
            .insert(old.clone(), row);
        let report = drain_trash(
            &mut rows,
            Instant::now() + Duration::from_secs(600),
            DeleterPace::Unpaced,
        );
        assert_eq!(report.stale_pending, vec![old.clone()]);
        assert_eq!(report.entries_removed, 0);
        assert!(trashed.trash_dir.join(&old).join("base.txt").exists());
        assert!(entry_matches(&fixture.pool, &old, &row));
    }

    /// The rename never replaces an existing destination, so a failed move
    /// leaves the checkout where it was.
    #[test]
    fn the_rename_never_replaces_a_destination() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("source")).unwrap();
        std::fs::write(dir.path().join("source/file"), "kept\n").unwrap();
        std::fs::create_dir(dir.path().join("taken")).unwrap();
        let fd = open_dir_nofollow(dir.path()).unwrap();
        assert!(rename_noreplace(fd.as_raw_fd(), "source", fd.as_raw_fd(), "taken").is_err());
        assert!(dir.path().join("source/file").exists());
        rename_noreplace(fd.as_raw_fd(), "source", fd.as_raw_fd(), "moved").unwrap();
        assert!(dir.path().join("moved/file").exists());
    }

    /// A trash whose listing fails is skipped with its rows kept, not
    /// treated as empty.
    #[test]
    fn an_unreadable_trash_keeps_its_rows() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let trashed = fixture.trash(&fixture.checkout("one"), 1).unwrap();
        let mut rows = rows_of(&trashed);
        std::fs::set_permissions(&trashed.trash_dir, std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let readable_anyway = std::fs::read_dir(&trashed.trash_dir).is_ok();
        let report = drain_trash(
            &mut rows,
            Instant::now() + Duration::from_secs(600),
            DeleterPace::Unpaced,
        );
        std::fs::set_permissions(&trashed.trash_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        if readable_anyway {
            // A privileged runner reads it regardless; nothing to show.
            return;
        }
        assert_eq!(report.entries_removed, 0);
        assert_eq!(report.invalid_trash_dirs, 1, "{report:?}");
        assert_eq!(rows.values().map(BTreeMap::len).sum::<usize>(), 1);
        assert!(trashed.path().exists());
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
        const { assert!(DELETER_FLOOR_UNLINKS_PER_SECOND >= 200) };
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
