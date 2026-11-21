//! Per-session scratch directories: a place the broker owns, creates and
//! removes, where a session's build output and temporary files go instead of
//! the system temp folder.
//!
//! Layout: `<pool>/.aethyme-session-scratch/s<session>-<hex ms>`, where
//! `<pool>` is the broker-owned worktree pool directory that holds the
//! session's checkout. That puts the directory
//!
//! - on the same volume as the checkout (and as the pool's checkout trash,
//!   so removal is one O(1) rename into the trash, never a copy);
//! - outside every checkout, so it never shows up in `git status`;
//! - under a dot-named container, which pool scanners skip like the trash
//!   (`is_worktree_root_infrastructure`).
//!
//! Removal is a deletion path and is written like one. Only a directory the
//! broker created and recorded (pool, entry name, device and inode) is ever
//! moved, and only after [`verify`] proves, through descriptors opened
//! without following links from `/` down: every component of the recorded
//! path is a real directory, the container is a private directory owned by
//! this user carrying the broker's marker, the entry still has the recorded
//! identity, and nothing crosses a device. The move is a rename into the
//! pool's checkout trash ([`crate::checkout_trash`]); the host-wide paced
//! deleter unlinks it later, after its own open-file observation. Nothing
//! here unlinks anything.
//!
//! Commands the broker runs for a session get the path as
//! [`SESSION_SCRATCH_ENV`]. `TMPDIR` (and `TMP`, `TEMP`) are not pointed at
//! the scratch directory itself: a pool path on the busiest machine is 87
//! bytes and holds a space, so a scratch path is about 130 bytes, past the
//! Unix socket path limit (104 bytes on macOS, 108 on Linux) before any
//! socket name is added. Each command instead gets its own run directory
//! inside it, handed out through a short alias when its path is too long
//! ([`crate::command_temp`]).

use std::collections::BTreeMap;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::checkout_trash::{
    c_name, checked, fstat, fstatat_nofollow, is_directory, open_dir_nofollow, openat_dir_nofollow,
};

/// The container directory inside a worktree pool.
pub(crate) const SCRATCH_CONTAINER: &str = ".aethyme-session-scratch";
/// Marker file proving the container is the broker's.
const SCRATCH_MARKER: &str = ".aethyme-scratch-marker";
const SCRATCH_MARKER_CONTENT: &str = "aethyme session scratch v1\n";
/// The variable commands the broker runs for a session receive.
pub const SESSION_SCRATCH_ENV: &str = "AETHYME_SESSION_SCRATCH_DIR";
/// Longest accepted entry name (`s<i64>-<hex ms>-<n>` fits easily).
const ENTRY_NAME_MAX: usize = 64;
/// Attempts at a fresh name when one is taken.
const CREATE_ATTEMPTS: u32 = 8;

/// What the broker recorded about one scratch directory it created.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ScratchRecord {
    pub(crate) session_id: i64,
    /// The worktree pool (canonical when created) holding the container.
    pub(crate) pool: PathBuf,
    /// The directory's name inside the container.
    pub(crate) entry: String,
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) created_ms: i64,
    /// Replaced by a fresh directory (`start --reuse`): retired even while
    /// the session stays open.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) superseded: bool,
    /// Why the last retirement attempt kept it, and how many attempts kept it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) keep: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) attempts: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl ScratchRecord {
    /// The directory's path.
    pub(crate) fn path(&self) -> PathBuf {
        self.pool.join(SCRATCH_CONTAINER).join(&self.entry)
    }

    /// The key records are stored under: the path, as text.
    pub(crate) fn key(&self) -> String {
        self.path().to_string_lossy().into_owned()
    }
}

/// Every recorded scratch directory of a repository, by [`ScratchRecord::key`].
pub(crate) type ScratchRecords = BTreeMap<String, ScratchRecord>;

/// Why a recorded directory is not moved. Each is a phase counter
/// `session_scratch_refused_<code>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The record itself is malformed: a relative or non-normal pool path, or
    /// an entry name the broker does not write for that session.
    NotARecordedScratch,
    /// The directory is gone (nothing to remove; the record is dropped).
    Missing,
    /// A component of the path is a symbolic link or not a directory.
    NotARealDirectory,
    /// The container is not a private, marked directory owned by this user.
    ForeignContainer,
    /// The entry is another directory than the one recorded.
    IdentityChanged,
    /// The container or the entry is on another device than the pool.
    CrossesDevice,
    /// The trash, the record or the rename failed; nothing moved.
    Failed,
}

impl Refusal {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::NotARecordedScratch => "not_a_recorded_scratch",
            Self::Missing => "missing",
            Self::NotARealDirectory => "not_a_real_directory",
            Self::ForeignContainer => "foreign_container",
            Self::IdentityChanged => "identity_changed",
            Self::CrossesDevice => "crosses_device",
            Self::Failed => "move_failed",
        }
    }

    fn counter(self) -> &'static str {
        match self {
            Self::NotARecordedScratch => "session_scratch_refused_not_a_recorded_scratch",
            Self::Missing => "session_scratch_refused_missing",
            Self::NotARealDirectory => "session_scratch_refused_not_a_real_directory",
            Self::ForeignContainer => "session_scratch_refused_foreign_container",
            Self::IdentityChanged => "session_scratch_refused_identity_changed",
            Self::CrossesDevice => "session_scratch_refused_crosses_device",
            Self::Failed => "session_scratch_refused_move_failed",
        }
    }
}

/// The entry name for `session_id` created at `created_ms`, with an optional
/// collision suffix.
fn entry_name(session_id: i64, created_ms: i64, attempt: u32) -> String {
    let created = u64::try_from(created_ms.max(0)).unwrap_or(0);
    if attempt == 0 {
        format!("s{session_id}-{created:x}")
    } else {
        format!("s{session_id}-{created:x}-{attempt}")
    }
}

/// Whether `name` is an entry name the broker writes for `session_id`:
/// `s<session>-<lowercase hex>` or `s<session>-<lowercase hex>-<decimal>`.
pub(crate) fn is_entry_name_for(name: &str, session_id: i64) -> bool {
    if name.len() > ENTRY_NAME_MAX {
        return false;
    }
    let Some(rest) = name.strip_prefix(&format!("s{session_id}-")) else {
        return false;
    };
    let mut parts = rest.split('-');
    let hex = parts.next().unwrap_or_default();
    let suffix = parts.next();
    parts.next().is_none()
        && !hex.is_empty()
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        && suffix
            .is_none_or(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `name` has the shape of any broker scratch entry (any session).
pub(crate) fn is_entry_name(name: &str) -> bool {
    name.strip_prefix('s')
        .and_then(|rest| rest.split_once('-'))
        .and_then(|(session, _)| session.parse::<i64>().ok())
        .is_some_and(|session| is_entry_name_for(name, session))
}

/// Whether `path` is absolute and made of normal components only (no `.`,
/// `..` or prefix), so walking it component by component means what it says.
fn plain_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

/// Open `path` as a directory by walking from `/`, every component opened
/// without following a symbolic link: a link anywhere on the path refuses.
fn open_path_without_links(path: &Path) -> Result<OwnedFd, Refusal> {
    if !plain_absolute(path) {
        return Err(Refusal::NotARecordedScratch);
    }
    let mut current = open_dir_nofollow(Path::new("/")).map_err(|_| Refusal::Failed)?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = name.to_str().ok_or(Refusal::NotARecordedScratch)?;
        current = openat_dir_nofollow(current.as_raw_fd(), name).map_err(|error| {
            if error.raw_os_error() == Some(libc::ENOENT) {
                Refusal::Missing
            } else {
                Refusal::NotARealDirectory
            }
        })?;
    }
    Ok(current)
}

fn uid() -> libc::uid_t {
    // SAFETY: `geteuid` has no preconditions.
    unsafe { libc::geteuid() }
}

/// Read the marker inside `container`, without following a link.
fn marker_matches(container: RawFd) -> bool {
    let Ok(marker) = c_name(SCRATCH_MARKER) else {
        return false;
    };
    // SAFETY: `marker` is NUL-terminated and `container` is open.
    let Ok(fd) = checked(unsafe {
        libc::openat(
            container,
            marker.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    }) else {
        return false;
    };
    // SAFETY: `fd` was just opened and is owned here.
    let mut file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    let mut content = String::new();
    std::io::Read::read_to_string(&mut file, &mut content).is_ok()
        && content == SCRATCH_MARKER_CONTENT
}

/// Write the marker into a container this call just created.
fn write_marker(container: RawFd) -> Result<(), String> {
    let marker = c_name(SCRATCH_MARKER).map_err(|error| error.to_string())?;
    // SAFETY: `marker` is NUL-terminated and `container` is open.
    let fd = checked(unsafe {
        libc::openat(
            container,
            marker.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    })
    .map_err(|error| format!("cannot mark the scratch container: {error}"))?;
    // SAFETY: `fd` was just opened and is owned here.
    let mut file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    std::io::Write::write_all(&mut file, SCRATCH_MARKER_CONTENT.as_bytes())
        .map_err(|error| format!("cannot mark the scratch container: {error}"))
}

/// Whether `stat` is a private directory owned by this user.
fn private_directory(stat: &libc::stat) -> bool {
    is_directory(stat) && stat.st_uid == uid() && stat.st_mode & 0o077 == 0
}

/// The container inside the pool `pool_fd`, verified: a real, private,
/// marked directory owned by this user, on the pool's device.
fn open_container(pool_fd: RawFd, pool_device: u64) -> Result<OwnedFd, Refusal> {
    let container = openat_dir_nofollow(pool_fd, SCRATCH_CONTAINER).map_err(|error| {
        if error.raw_os_error() == Some(libc::ENOENT) {
            Refusal::Missing
        } else {
            Refusal::NotARealDirectory
        }
    })?;
    let stat = fstat(container.as_raw_fd()).map_err(|_| Refusal::Failed)?;
    if !private_directory(&stat) || !marker_matches(container.as_raw_fd()) {
        return Err(Refusal::ForeignContainer);
    }
    #[allow(clippy::unnecessary_cast)]
    let container_device = stat.st_dev as u64;
    if container_device != pool_device {
        return Err(Refusal::CrossesDevice);
    }
    Ok(container)
}

/// Create a fresh scratch directory for `session_id` in the pool `pool`
/// (mode 0700), creating the marked container when it is absent, and return
/// what to record. Refuses a pool path with a link in it, a foreign
/// container, or an entry on another device.
pub(crate) fn create(pool: &Path, session_id: i64) -> Result<ScratchRecord, String> {
    let _span = crate::phase_telemetry::span("session_scratch_create");
    let pool = std::fs::canonicalize(pool)
        .map_err(|error| format!("cannot resolve the pool {}: {error}", pool.display()))?;
    let pool_fd = open_path_without_links(&pool).map_err(|refusal| {
        format!(
            "the pool {} is refused ({})",
            pool.display(),
            refusal.code()
        )
    })?;
    #[allow(clippy::unnecessary_cast)]
    let pool_device = fstat(pool_fd.as_raw_fd())
        .map_err(|error| error.to_string())?
        .st_dev as u64;
    let name = c_name(SCRATCH_CONTAINER).map_err(|error| error.to_string())?;
    // SAFETY: `name` is NUL-terminated and `pool_fd` is open.
    let created_container =
        match checked(unsafe { libc::mkdirat(pool_fd.as_raw_fd(), name.as_ptr(), 0o700) }) {
            Ok(_) => true,
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => false,
            Err(error) => return Err(format!("cannot create the scratch container: {error}")),
        };
    if created_container {
        let container = openat_dir_nofollow(pool_fd.as_raw_fd(), SCRATCH_CONTAINER)
            .map_err(|error| format!("the scratch container is not a real directory: {error}"))?;
        let stat = fstat(container.as_raw_fd()).map_err(|error| error.to_string())?;
        if !private_directory(&stat) {
            return Err(
                "the scratch container is not a private directory owned by this user".into(),
            );
        }
        write_marker(container.as_raw_fd())?;
    }
    let container = open_container(pool_fd.as_raw_fd(), pool_device)
        .map_err(|refusal| format!("the scratch container is refused ({})", refusal.code()))?;
    let created_ms = crate::clock::epoch_ms();
    for attempt in 0..CREATE_ATTEMPTS {
        let entry = entry_name(session_id, created_ms, attempt);
        let c_entry = c_name(&entry).map_err(|error| error.to_string())?;
        // SAFETY: `c_entry` is NUL-terminated and `container` is open.
        match checked(unsafe { libc::mkdirat(container.as_raw_fd(), c_entry.as_ptr(), 0o700) }) {
            Ok(_) => {}
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => continue,
            Err(error) => return Err(format!("cannot create the scratch directory: {error}")),
        }
        let stat = fstatat_nofollow(container.as_raw_fd(), &entry)
            .map_err(|error| format!("cannot inspect the new scratch directory: {error}"))?;
        #[allow(clippy::unnecessary_cast)]
        let (device, inode) = (stat.st_dev as u64, stat.st_ino as u64);
        if !is_directory(&stat) || device != pool_device {
            return Err(
                "the new scratch directory is not a real directory on the pool's device".into(),
            );
        }
        crate::phase_telemetry::add("session_scratch_created", Duration::ZERO);
        return Ok(ScratchRecord {
            session_id,
            pool,
            entry,
            device,
            inode,
            created_ms,
            superseded: false,
            keep: None,
            attempts: 0,
        });
    }
    Err("no free scratch directory name".into())
}

/// A recorded directory proven to be the one the broker created.
pub(crate) struct Verified {
    container: OwnedFd,
}

/// Prove `record` names the directory the broker created: see the module
/// documentation. Read-only.
pub(crate) fn verify(record: &ScratchRecord) -> Result<Verified, Refusal> {
    if !is_entry_name_for(&record.entry, record.session_id) || !plain_absolute(&record.pool) {
        return Err(Refusal::NotARecordedScratch);
    }
    let pool_fd = open_path_without_links(&record.pool)?;
    #[allow(clippy::unnecessary_cast)]
    let pool_device = fstat(pool_fd.as_raw_fd())
        .map_err(|_| Refusal::Failed)?
        .st_dev as u64;
    let container = open_container(pool_fd.as_raw_fd(), pool_device)?;
    let stat = fstatat_nofollow(container.as_raw_fd(), &record.entry).map_err(|error| {
        if error.raw_os_error() == Some(libc::ENOENT) {
            Refusal::Missing
        } else {
            Refusal::Failed
        }
    })?;
    if !is_directory(&stat) {
        return Err(Refusal::NotARealDirectory);
    }
    #[allow(clippy::unnecessary_cast)]
    let (device, inode) = (stat.st_dev as u64, stat.st_ino as u64);
    if device != pool_device {
        return Err(Refusal::CrossesDevice);
    }
    if (device, inode) != (record.device, record.inode) {
        return Err(Refusal::IdentityChanged);
    }
    Ok(Verified { container })
}

/// Re-identify `record` after its volume was remounted, which gives every
/// directory on it a new device number while inode numbers stay: the record
/// is updated only when the pool's current device differs from the
/// recorded one and, on that current device, the container is the broker's
/// (private, owned by this user, carrying its marker), the entry name is the
/// one bound to the record's session, and the entry is a real directory
/// with the recorded inode. Anything else refuses, and the caller keeps the
/// directory. Read-only; the caller stores the returned record.
pub(crate) fn reidentify(record: &ScratchRecord) -> Result<ScratchRecord, Refusal> {
    if !is_entry_name_for(&record.entry, record.session_id) || !plain_absolute(&record.pool) {
        return Err(Refusal::NotARecordedScratch);
    }
    let pool_fd = open_path_without_links(&record.pool)?;
    #[allow(clippy::unnecessary_cast)]
    let pool_device = fstat(pool_fd.as_raw_fd())
        .map_err(|_| Refusal::Failed)?
        .st_dev as u64;
    if pool_device == record.device {
        // Same device: a different inode is another directory, not a remount.
        return Err(Refusal::IdentityChanged);
    }
    let container = open_container(pool_fd.as_raw_fd(), pool_device)?;
    let stat = fstatat_nofollow(container.as_raw_fd(), &record.entry).map_err(|error| {
        if error.raw_os_error() == Some(libc::ENOENT) {
            Refusal::Missing
        } else {
            Refusal::Failed
        }
    })?;
    if !is_directory(&stat) {
        return Err(Refusal::NotARealDirectory);
    }
    #[allow(clippy::unnecessary_cast)]
    let (device, inode) = (stat.st_dev as u64, stat.st_ino as u64);
    if device != pool_device {
        return Err(Refusal::CrossesDevice);
    }
    if inode != record.inode {
        return Err(Refusal::IdentityChanged);
    }
    crate::phase_telemetry::add("session_scratch_reidentified", Duration::ZERO);
    Ok(ScratchRecord {
        device,
        ..record.clone()
    })
}

/// A scratch directory moved into the pool's trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrashedScratch {
    pub(crate) trash_dir: PathBuf,
    pub(crate) entry: String,
    pub(crate) row: crate::checkout_trash::TrashRow,
}

/// Move the recorded directory into its pool's checkout trash with one
/// exclusive same-volume rename, after [`verify`]. The trash row is recorded
/// pending (through `record_pending`) before the rename, exactly as for a
/// checkout, and the identity is checked again right before it. Nothing is
/// unlinked here; the caller has proved no process uses the directory.
pub(crate) fn move_to_trash(
    record: &ScratchRecord,
    record_pending: &mut dyn FnMut(
        &Path,
        &str,
        crate::checkout_trash::TrashRow,
    ) -> Result<(), String>,
) -> Result<TrashedScratch, Refusal> {
    let _span = crate::phase_telemetry::span("session_scratch_move");
    let verified = verify(record).inspect_err(|refusal| {
        crate::phase_telemetry::add(refusal.counter(), Duration::ZERO);
    })?;
    let trash = crate::checkout_trash::open_trash(&record.pool).map_err(|_| Refusal::Failed)?;
    #[allow(clippy::unnecessary_cast)]
    let trash_device = fstat(trash.trash_fd()).map_err(|_| Refusal::Failed)?.st_dev as u64;
    if trash_device != record.device {
        crate::phase_telemetry::add(Refusal::CrossesDevice.counter(), Duration::ZERO);
        return Err(Refusal::CrossesDevice);
    }
    let entry = crate::checkout_trash::new_entry_name(record.session_id);
    let (creator_pid, creator_started_ms) = crate::checkout_trash::this_process_identity();
    let mut row = crate::checkout_trash::TrashRow {
        device: record.device,
        inode: record.inode,
        session_id: record.session_id,
        failures: 0,
        pending: true,
        creator_pid,
        creator_started_ms,
        stripped: true,
        scratch: true,
        verification: false,
    };
    record_pending(trash.path(), &entry, row).map_err(|_| Refusal::Failed)?;
    // The identity once more, on the pinned container, right before the
    // rename: a directory swapped in since `verify` is never moved. A swap
    // between this check and the rename (microseconds, by a process of this
    // same user inside a private 0700 container) is not excluded; the
    // rename then moves the swapped-in directory into the trash, where the
    // deleter's own identity check against this row (device and inode)
    // refuses to delete it.
    let still = fstatat_nofollow(verified.container.as_raw_fd(), &record.entry).is_ok_and(|stat| {
        #[allow(clippy::unnecessary_cast)]
        let identity = (stat.st_dev as u64, stat.st_ino as u64);
        is_directory(&stat) && identity == (record.device, record.inode)
    });
    if !still {
        crate::phase_telemetry::add(Refusal::IdentityChanged.counter(), Duration::ZERO);
        return Err(Refusal::IdentityChanged);
    }
    crate::checkout_trash::rename_noreplace(
        verified.container.as_raw_fd(),
        &record.entry,
        trash.trash_fd(),
        &entry,
    )
    .map_err(|error| {
        let refusal = if error.raw_os_error() == Some(libc::EXDEV) {
            Refusal::CrossesDevice
        } else {
            Refusal::Failed
        };
        crate::phase_telemetry::add(refusal.counter(), Duration::ZERO);
        refusal
    })?;
    row.pending = false;
    remove_container_if_empty(&record.pool, verified.container.as_raw_fd());
    Ok(TrashedScratch {
        trash_dir: trash.path().to_path_buf(),
        entry,
        row,
    })
}

/// One finished command run ([`crate::command_temp`]) waiting in the done
/// container of a recorded scratch directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FinishedRun {
    pub(crate) name: String,
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

impl FinishedRun {
    /// The run's path inside `record`'s directory.
    pub(crate) fn path(&self, record: &ScratchRecord) -> PathBuf {
        record
            .path()
            .join(crate::command_temp::DONE_CONTAINER)
            .join(&self.name)
    }
}

/// The verified done container of `record`'s directory.
fn open_done_container(record: &ScratchRecord) -> Result<(OwnedFd, u64), Refusal> {
    let verified = verify(record)?;
    let entry = openat_dir_nofollow(verified.container.as_raw_fd(), &record.entry)
        .map_err(|_| Refusal::NotARealDirectory)?;
    let done = openat_dir_nofollow(entry.as_raw_fd(), crate::command_temp::DONE_CONTAINER)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ENOENT) {
                Refusal::Missing
            } else {
                Refusal::NotARealDirectory
            }
        })?;
    let stat = fstat(done.as_raw_fd()).map_err(|_| Refusal::Failed)?;
    #[allow(clippy::unnecessary_cast)]
    let device = stat.st_dev as u64;
    if !private_directory(&stat) {
        return Err(Refusal::ForeignContainer);
    }
    if device != record.device {
        return Err(Refusal::CrossesDevice);
    }
    Ok((done, device))
}

/// Finished runs in `record`'s done container, at most `limit`: real
/// directories with a run name the broker writes, on the record's device.
/// Anything else there is left alone. Read-only.
pub(crate) fn finished_runs(record: &ScratchRecord, limit: usize) -> Vec<FinishedRun> {
    let Ok((done, device)) = open_done_container(record) else {
        return Vec::new();
    };
    let Ok(listing) = std::fs::read_dir(record.path().join(crate::command_temp::DONE_CONTAINER))
    else {
        return Vec::new();
    };
    listing
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| crate::command_temp::is_run_name(name))
        .filter_map(|name| {
            let stat = fstatat_nofollow(done.as_raw_fd(), &name).ok()?;
            #[allow(clippy::unnecessary_cast)]
            let (run_device, inode) = (stat.st_dev as u64, stat.st_ino as u64);
            (is_directory(&stat) && run_device == device).then_some(FinishedRun {
                name,
                device: run_device,
                inode,
            })
        })
        .take(limit)
        .collect()
}

/// Move one finished run from `record`'s done container into the pool's
/// checkout trash with one exclusive same-volume rename, exactly as
/// [`move_to_trash`] moves a whole scratch directory: the scratch directory
/// verifies, the run is a real directory with the listed identity on the
/// pool's device, the row is recorded pending first, and the identity is
/// checked again right before the rename. Nothing is unlinked here.
pub(crate) fn move_finished_run_to_trash(
    record: &ScratchRecord,
    run: &FinishedRun,
    record_pending: &mut dyn FnMut(
        &Path,
        &str,
        crate::checkout_trash::TrashRow,
    ) -> Result<(), String>,
) -> Result<TrashedScratch, Refusal> {
    let _span = crate::phase_telemetry::span("command_temp_move");
    if !crate::command_temp::is_run_name(&run.name) {
        return Err(Refusal::NotARecordedScratch);
    }
    let (done, device) = open_done_container(record)?;
    let same = |done: RawFd| {
        fstatat_nofollow(done, &run.name).is_ok_and(|stat| {
            #[allow(clippy::unnecessary_cast)]
            let identity = (stat.st_dev as u64, stat.st_ino as u64);
            is_directory(&stat) && identity == (run.device, run.inode)
        })
    };
    if run.device != device || !same(done.as_raw_fd()) {
        return Err(Refusal::IdentityChanged);
    }
    let trash = crate::checkout_trash::open_trash(&record.pool).map_err(|_| Refusal::Failed)?;
    #[allow(clippy::unnecessary_cast)]
    let trash_device = fstat(trash.trash_fd()).map_err(|_| Refusal::Failed)?.st_dev as u64;
    if trash_device != device {
        return Err(Refusal::CrossesDevice);
    }
    let entry = crate::checkout_trash::new_entry_name(record.session_id);
    let (creator_pid, creator_started_ms) = crate::checkout_trash::this_process_identity();
    let mut row = crate::checkout_trash::TrashRow {
        device: run.device,
        inode: run.inode,
        session_id: record.session_id,
        failures: 0,
        pending: true,
        creator_pid,
        creator_started_ms,
        stripped: true,
        scratch: true,
        verification: false,
    };
    record_pending(trash.path(), &entry, row).map_err(|_| Refusal::Failed)?;
    if !same(done.as_raw_fd()) {
        return Err(Refusal::IdentityChanged);
    }
    crate::checkout_trash::rename_noreplace(done.as_raw_fd(), &run.name, trash.trash_fd(), &entry)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::EXDEV) {
                Refusal::CrossesDevice
            } else {
                Refusal::Failed
            }
        })?;
    row.pending = false;
    Ok(TrashedScratch {
        trash_dir: trash.path().to_path_buf(),
        entry,
        row,
    })
}

/// Remove a container that holds nothing but its marker, so an emptied pool
/// is left as it was found. `rmdir` refuses a container that gained an entry
/// meanwhile (and a link in its place); the marker is then written again.
fn remove_container_if_empty(pool: &Path, container: RawFd) {
    let path = pool.join(SCRATCH_CONTAINER);
    let only_marker = std::fs::read_dir(&path).is_ok_and(|listing| {
        listing
            .filter_map(Result::ok)
            .all(|entry| entry.file_name() == SCRATCH_MARKER)
    });
    if !only_marker {
        return;
    }
    let Ok(marker) = c_name(SCRATCH_MARKER) else {
        return;
    };
    // SAFETY: `marker` is NUL-terminated and `container` is an open
    // directory; `unlinkat` without flags removes only a non-directory.
    unsafe {
        libc::unlinkat(container, marker.as_ptr(), 0);
    }
    if std::fs::remove_dir(&path).is_err() {
        crate::warn_unrecorded("mark the scratch container again", write_marker(container));
    }
}

/// Whether `container` (a pool's scratch container, not followed if a link)
/// holds nothing but the broker's marker and scratch-named real
/// directories: the broker's own state, removed with its sessions.
pub(crate) fn container_holds_only_scratch(container: &Path) -> bool {
    let Ok(listing) = std::fs::read_dir(container) else {
        return false;
    };
    std::fs::symlink_metadata(container).is_ok_and(|metadata| metadata.is_dir())
        && listing.flatten().all(|entry| {
            let kind = entry.file_type().ok();
            match entry.file_name().to_str() {
                Some(SCRATCH_MARKER) => kind.is_some_and(|kind| kind.is_file()),
                Some(name) => is_entry_name(name) && kind.is_some_and(|kind| kind.is_dir()),
                None => false,
            }
        })
}

/// Whether `container` holds any entry besides the broker's marker.
pub(crate) fn container_has_entries(container: &Path) -> bool {
    // An unreadable container counts as holding entries: nothing is removed.
    match std::fs::read_dir(container) {
        Ok(listing) => listing
            .flatten()
            .any(|entry| entry.file_name() != SCRATCH_MARKER),
        Err(_) => true,
    }
}

/// Names in the container of `pool` that are not recorded (`recorded` holds
/// the recorded entry names), at most `limit`. Left alone, only counted.
pub(crate) fn unrecorded_entries(
    pool: &Path,
    recorded: &std::collections::BTreeSet<String>,
    limit: usize,
) -> Vec<String> {
    let Ok(listing) = std::fs::read_dir(pool.join(SCRATCH_CONTAINER)) else {
        return Vec::new();
    };
    listing
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| name != SCRATCH_MARKER && !recorded.contains(name))
        .take(limit)
        .collect()
}

/// Allocated bytes under `path` (never following a link or crossing a
/// device), walking until `deadline`; `false` when it stopped short.
pub(crate) fn measure(path: &Path, deadline: Instant) -> (u64, bool) {
    let Some(device) = crate::storage_ledger::device_of(path) else {
        return (0, true);
    };
    let mut pending = vec![path.to_path_buf()];
    let mut usage = crate::storage_ledger::Usage::default();
    let complete = crate::storage_ledger::walk_until(
        &mut pending,
        &mut usage,
        device,
        deadline,
        crate::storage_ledger::BEAT_EVERY,
        &mut || {},
    );
    (usage.bytes, complete)
}

/// Scratch directories resolved in this process, by session, for commands
/// the broker spawns where only the session id is at hand (gate commands).
/// Process-wide on purpose: gate workers run on other threads.
static RESOLVED_FOR_COMMANDS: Mutex<BTreeMap<i64, PathBuf>> = Mutex::new(BTreeMap::new());

/// Remember `path` as `session_id`'s scratch directory for commands this
/// process spawns.
pub(crate) fn remember_for_commands(session_id: i64, path: &Path) {
    RESOLVED_FOR_COMMANDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session_id, path.to_path_buf());
}

/// The scratch directory remembered for `session_id`, when it still exists.
pub(crate) fn remembered_for_commands(session_id: i64) -> Option<PathBuf> {
    RESOLVED_FOR_COMMANDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&session_id)
        .filter(|path| path.is_dir())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> (tempfile::TempDir, PathBuf) {
        let owner = tempfile::tempdir().unwrap();
        let pool = std::fs::canonicalize(owner.path()).unwrap().join("pool");
        std::fs::create_dir_all(&pool).unwrap();
        (owner, pool)
    }

    #[test]
    fn entry_names_are_bound_to_their_session() {
        assert!(is_entry_name_for("s12-19a2b3c4d5e", 12));
        assert!(is_entry_name_for("s12-19a2b3c4d5e-3", 12));
        assert!(!is_entry_name_for("s12-19a2b3c4d5e", 13));
        assert!(!is_entry_name_for("s12-", 12));
        assert!(!is_entry_name_for("s12-XYZ", 12));
        assert!(!is_entry_name_for("s12-1a/..", 12));
        assert!(!is_entry_name_for("s12-1a-2-3", 12));
        assert!(is_entry_name("s7-ff"));
        assert!(!is_entry_name(".aethyme-scratch-marker"));
    }

    #[test]
    fn a_created_directory_is_private_marked_and_verifies() {
        let (_owner, pool) = pool();
        let record = create(&pool, 4).unwrap();
        let path = record.path();
        assert!(path.is_dir());
        assert!(path.starts_with(pool.join(SCRATCH_CONTAINER)));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
        assert!(pool.join(SCRATCH_CONTAINER).join(SCRATCH_MARKER).is_file());
        assert!(verify(&record).is_ok());
        let second = create(&pool, 4).unwrap();
        assert_ne!(
            second.entry, record.entry,
            "a fresh one never reuses a name"
        );
    }

    #[test]
    fn a_path_that_is_not_the_recorded_scratch_is_refused() {
        let (_owner, pool) = pool();
        let record = create(&pool, 9).unwrap();
        // Another session's name.
        let mut other = record.clone();
        other.session_id = 10;
        assert_eq!(verify(&other).err(), Some(Refusal::NotARecordedScratch));
        // A path escaping the container.
        let mut escape = record.clone();
        escape.entry = "../elsewhere".into();
        assert_eq!(verify(&escape).err(), Some(Refusal::NotARecordedScratch));
        // A relative pool.
        let mut relative = record.clone();
        relative.pool = PathBuf::from("pool");
        assert_eq!(verify(&relative).err(), Some(Refusal::NotARecordedScratch));
        // Same name, another directory.
        // (made before the original goes, so it never reuses its inode).
        let replacement = pool.join("replacement");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::remove_dir(record.path()).unwrap();
        std::fs::rename(&replacement, record.path()).unwrap();
        assert_eq!(verify(&record).err(), Some(Refusal::IdentityChanged));
        // A symlink in place of the entry.
        let target = pool.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::remove_dir(record.path()).unwrap();
        std::os::unix::fs::symlink(&target, record.path()).unwrap();
        assert_eq!(verify(&record).err(), Some(Refusal::NotARealDirectory));
        let mut moved = |_: &Path,
                         _: &str,
                         _: crate::checkout_trash::TrashRow|
         -> Result<(), String> { Ok(()) };
        assert!(move_to_trash(&record, &mut moved).is_err());
        assert!(target.is_dir(), "a link's target is never moved");
    }

    #[test]
    fn a_link_above_the_pool_or_a_foreign_container_is_refused() {
        let (owner, pool) = pool();
        let record = create(&pool, 3).unwrap();
        let alias = std::fs::canonicalize(owner.path()).unwrap().join("alias");
        std::os::unix::fs::symlink(&pool, &alias).unwrap();
        let mut through_link = record.clone();
        through_link.pool = alias;
        assert_eq!(
            verify(&through_link).err(),
            Some(Refusal::NotARealDirectory)
        );
        std::fs::remove_file(pool.join(SCRATCH_CONTAINER).join(SCRATCH_MARKER)).unwrap();
        assert_eq!(verify(&record).err(), Some(Refusal::ForeignContainer));
    }

    #[test]
    fn a_verified_directory_moves_into_the_trash_without_unlinking() {
        let (_owner, pool) = pool();
        let record = create(&pool, 5).unwrap();
        std::fs::write(record.path().join("build.o"), b"output").unwrap();
        let mut recorded = Vec::new();
        let mut record_pending =
            |dir: &Path, entry: &str, row: crate::checkout_trash::TrashRow| -> Result<(), String> {
                recorded.push((dir.to_path_buf(), entry.to_string(), row));
                Ok(())
            };
        let moved = move_to_trash(&record, &mut record_pending).unwrap();
        assert!(!record.path().exists());
        assert!(moved.trash_dir.join(&moved.entry).join("build.o").is_file());
        assert!(moved.row.scratch && moved.row.stripped && !moved.row.pending);
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].2.pending, "recorded pending before the rename");
        assert!(crate::checkout_trash::is_broker_entry_name(&moved.entry));
        assert_eq!(verify(&record).err(), Some(Refusal::Missing));
    }

    /// A remount renumbers the device: the record is re-identified by its
    /// marked container, its session-bound name and its inode, and only
    /// then; another directory under the same name is still refused.
    #[test]
    fn a_remounted_directory_is_reidentified_by_marker_name_and_inode() {
        let (_owner, pool) = pool();
        let record = create(&pool, 11).unwrap();
        let mut remounted = record.clone();
        remounted.device = remounted.device.wrapping_add(1000);
        assert_eq!(verify(&remounted).err(), Some(Refusal::IdentityChanged));
        let updated = reidentify(&remounted).unwrap();
        assert_eq!(updated.device, record.device);
        assert_eq!(updated.inode, record.inode);
        assert!(verify(&updated).is_ok());
        // Same device: no remount, nothing to re-identify.
        assert_eq!(reidentify(&record).err(), Some(Refusal::IdentityChanged));
        // A remount and a swapped directory: refused.
        let replacement = pool.join("replacement");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::remove_dir(record.path()).unwrap();
        std::fs::rename(&replacement, record.path()).unwrap();
        assert_eq!(reidentify(&remounted).err(), Some(Refusal::IdentityChanged));
        // A remount with the marker gone: refused.
        let fresh = create(&pool, 12).unwrap();
        let mut fresh_remounted = fresh.clone();
        fresh_remounted.device = fresh_remounted.device.wrapping_add(1000);
        std::fs::remove_file(pool.join(SCRATCH_CONTAINER).join(SCRATCH_MARKER)).unwrap();
        assert_eq!(
            reidentify(&fresh_remounted).err(),
            Some(Refusal::ForeignContainer)
        );
    }

    /// A container of scratch directories is broker state (a root holding
    /// it is still empty of content); anything else in it is not.
    #[test]
    fn a_container_of_scratch_directories_is_broker_state() {
        let (_owner, pool) = pool();
        let record = create(&pool, 8).unwrap();
        let container = pool.join(SCRATCH_CONTAINER);
        assert!(container_holds_only_scratch(&container));
        assert!(container_has_entries(&container));
        std::fs::remove_dir(record.path()).unwrap();
        assert!(container_holds_only_scratch(&container));
        assert!(
            !container_has_entries(&container),
            "only the marker is left"
        );
        std::fs::write(container.join("notes.txt"), b"mine").unwrap();
        assert!(!container_holds_only_scratch(&container));
    }

    #[test]
    fn unrecorded_entries_are_listed_not_touched() {
        let (_owner, pool) = pool();
        let record = create(&pool, 6).unwrap();
        std::fs::create_dir(pool.join(SCRATCH_CONTAINER).join("someone-else")).unwrap();
        let recorded = std::collections::BTreeSet::from([record.entry.clone()]);
        assert_eq!(
            unrecorded_entries(&pool, &recorded, 10),
            vec!["someone-else".to_string()]
        );
        assert!(pool.join(SCRATCH_CONTAINER).join("someone-else").is_dir());
    }
}
