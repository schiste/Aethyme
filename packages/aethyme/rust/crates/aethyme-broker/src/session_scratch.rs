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
    /// A system call, the trash, its record or the rename failed; nothing
    /// moved. Which step and why are kept ([`MoveFailure`]) and counted.
    Failed(MoveFailure),
}

/// The step of a move that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoveStep {
    /// Opening or inspecting a directory on the recorded path.
    Open,
    /// Inspecting the entry itself (its identity).
    Inspect,
    /// Opening (or creating) the pool's checkout trash.
    TrashOpen,
    /// Recording the pending trash row in the broker's store.
    TrashRecord,
    /// The exclusive rename into the trash.
    Rename,
}

impl MoveStep {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Inspect => "inspect",
            Self::TrashOpen => "trash_open",
            Self::TrashRecord => "trash_record",
            Self::Rename => "rename",
        }
    }
}

/// Where a move failed and why: the step, and the cause as a counter-safe
/// name ([`crate::checkout_trash::errno_cause`], or `store` for the record).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MoveFailure {
    pub(crate) step: MoveStep,
    pub(crate) cause: &'static str,
}

impl MoveFailure {
    pub(crate) fn io(step: MoveStep, error: &std::io::Error) -> Self {
        Self {
            step,
            cause: crate::checkout_trash::errno_cause(error),
        }
    }

    /// `<step>.<cause>`, the key it is counted under.
    pub(crate) fn key(self) -> String {
        format!("{}.{}", self.step.code(), self.cause)
    }

    /// Whether the next move would fail the same way whatever it moves: the
    /// process is out of descriptors, the volume is full or read-only, the
    /// trash cannot be opened, or the store refuses the record. A pass stops
    /// on these instead of failing every entry in turn.
    pub(crate) fn systemic(self) -> bool {
        matches!(self.step, MoveStep::TrashOpen | MoveStep::TrashRecord)
            || matches!(
                self.cause,
                "too_many_open_files" | "no_space" | "read_only" | "unsupported"
            )
    }
}

/// What a systemic move failure ([`MoveFailure::systemic`]) is confined to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureScope {
    /// The whole process (out of descriptors): every move would fail.
    Process,
    /// One pool: its trash cannot be opened, or its trash row refused.
    Pool,
    /// One volume: full, read-only, or without an exclusive rename.
    Volume,
}

impl MoveFailure {
    /// What this failure is confined to, when it is systemic.
    pub(crate) fn scope(self) -> FailureScope {
        if self.cause == "too_many_open_files" {
            FailureScope::Process
        } else if matches!(self.step, MoveStep::TrashOpen | MoveStep::TrashRecord) {
            FailureScope::Pool
        } else {
            FailureScope::Volume
        }
    }
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
            Self::Failed(_) => "move_failed",
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
            Self::Failed(_) => "session_scratch_refused_move_failed",
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
    let mut current = open_dir_nofollow(Path::new("/"))
        .map_err(|error| Refusal::Failed(MoveFailure::io(MoveStep::Open, &error)))?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = name.to_str().ok_or(Refusal::NotARecordedScratch)?;
        current =
            openat_dir_nofollow(current.as_raw_fd(), name).map_err(|error| open_refusal(&error))?;
    }
    Ok(current)
}

/// Why opening a directory on a recorded path without following links
/// failed: gone (`ENOENT`), not a real directory (a link, `ELOOP`, or not a
/// directory, `ENOTDIR`), or a failed call whose cause is counted (out of
/// descriptors, denied, I/O, ...). Only the first two are judgements about
/// the path; the rest say nothing about it and stop or retry like any
/// failed move.
pub(crate) fn open_refusal(error: &std::io::Error) -> Refusal {
    match error.raw_os_error() {
        Some(libc::ENOENT) => Refusal::Missing,
        Some(libc::ELOOP | libc::ENOTDIR) => Refusal::NotARealDirectory,
        _ => Refusal::Failed(MoveFailure::io(MoveStep::Open, error)),
    }
}

/// Whether the pool holding `record` is not there to judge: its root is
/// missing or not a directory (a volume that is not mounted), or it is on
/// another device than recorded without the broker's scratch container (an
/// empty mount-point directory standing in for the volume). A scratch
/// directory that looks missing then is kept, never forgotten.
///
/// A missing pool root alone does not prove the volume is away: when the
/// nearest ancestor that exists (looked at without following a link) is on
/// the recorded device, the volume is mounted and the pool was really
/// removed, so the record may be forgotten. Any other answer (an ancestor
/// on another device, as the empty mount point of a volume that is not
/// mounted is, or an error other than `ENOENT` on the way up) keeps it.
pub(crate) fn pool_unavailable(record: &ScratchRecord) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match std::fs::symlink_metadata(&record.pool) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let unavailable = !nearest_existing_ancestor_on_device(&record.pool, record.device);
            crate::phase_telemetry::add(
                if unavailable {
                    "session_scratch_pool_absent_volume_away"
                } else {
                    "session_scratch_pool_absent_volume_mounted"
                },
                Duration::ZERO,
            );
            unavailable
        }
        Err(_) => true,
        Ok(metadata) if !metadata.is_dir() => true,
        Ok(metadata) => {
            metadata.dev() != record.device && !record.pool.join(SCRATCH_CONTAINER).is_dir()
        }
    }
}

/// Whether the nearest ancestor of `path` that exists is on `device`:
/// ancestors are looked at without following a link (a link's own device
/// is its parent's), climbing only past `ENOENT`. False on any other error
/// or when no ancestor answers.
fn nearest_existing_ancestor_on_device(path: &Path, device: u64) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    for ancestor in path.ancestors().skip(1) {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => return metadata.is_dir() && metadata.dev() == device,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
    }
    false
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
    let container =
        openat_dir_nofollow(pool_fd, SCRATCH_CONTAINER).map_err(|error| open_refusal(&error))?;
    let stat = fstat(container.as_raw_fd())
        .map_err(|error| Refusal::Failed(MoveFailure::io(MoveStep::Open, &error)))?;
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
        .map_err(|error| Refusal::Failed(MoveFailure::io(MoveStep::Open, &error)))?
        .st_dev as u64;
    let container = open_container(pool_fd.as_raw_fd(), pool_device)?;
    let stat = fstatat_nofollow(container.as_raw_fd(), &record.entry).map_err(|error| {
        if error.raw_os_error() == Some(libc::ENOENT) {
            Refusal::Missing
        } else {
            Refusal::Failed(MoveFailure::io(MoveStep::Inspect, &error))
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
        .map_err(|error| Refusal::Failed(MoveFailure::io(MoveStep::Open, &error)))?
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
            Refusal::Failed(MoveFailure::io(MoveStep::Inspect, &error))
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
    let trash = open_trash_for_move(&record.pool).inspect_err(|refusal| {
        crate::phase_telemetry::add(refusal.counter(), Duration::ZERO);
    })?;
    if trash.device != record.device {
        crate::phase_telemetry::add(Refusal::CrossesDevice.counter(), Duration::ZERO);
        return Err(Refusal::CrossesDevice);
    }
    let trash = trash.trash;
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
    record_pending(trash.path(), &entry, row).map_err(|_| {
        let refusal = Refusal::Failed(STORE_FAILURE);
        crate::phase_telemetry::add(refusal.counter(), Duration::ZERO);
        refusal
    })?;
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
    exclusive_rename(
        verified.container.as_raw_fd(),
        &record.entry,
        trash.trash_fd(),
        &entry,
    )
    .inspect_err(|refusal| {
        crate::phase_telemetry::add(refusal.counter(), Duration::ZERO);
    })?;
    row.pending = false;
    remove_container_if_empty(&record.pool, verified.container.as_raw_fd());
    Ok(TrashedScratch {
        trash_dir: trash.path().to_path_buf(),
        entry,
        row,
    })
}

/// The store refused the pending trash row.
const STORE_FAILURE: MoveFailure = MoveFailure {
    step: MoveStep::TrashRecord,
    cause: "store",
};

/// The pool's trash, opened for a move, and its device.
struct TrashForMove {
    trash: crate::checkout_trash::TrashDir,
    device: u64,
}

fn open_trash_for_move(pool: &Path) -> Result<TrashForMove, Refusal> {
    let trash = crate::checkout_trash::open_trash_classified(pool).map_err(|error| {
        Refusal::Failed(MoveFailure {
            step: MoveStep::TrashOpen,
            cause: error.cause,
        })
    })?;
    #[allow(clippy::unnecessary_cast)]
    let device = fstat(trash.trash_fd())
        .map_err(|error| Refusal::Failed(MoveFailure::io(MoveStep::TrashOpen, &error)))?
        .st_dev as u64;
    Ok(TrashForMove { trash, device })
}

#[cfg(test)]
thread_local! {
    /// An errno the next exclusive renames of this thread fail with, as a
    /// filesystem without an exclusive rename would (`ENOTSUP`, `EINVAL`).
    /// With the names of the entries it applies to (empty: every entry).
    static RENAME_ERRNO_FOR_TEST: std::cell::RefCell<Option<(i32, Vec<String>)>> =
        const { std::cell::RefCell::new(None) };
}

/// Make this thread's exclusive renames fail with `errno` (`None` clears).
#[cfg(test)]
pub(crate) fn fail_renames_for_test(errno: Option<i32>) {
    RENAME_ERRNO_FOR_TEST.with(|cell| *cell.borrow_mut() = errno.map(|errno| (errno, Vec::new())));
}

/// Make this thread's exclusive renames of the entries named `names` fail
/// with `errno`; other entries rename normally.
#[cfg(test)]
pub(crate) fn fail_renames_of_for_test(errno: i32, names: Vec<String>) {
    RENAME_ERRNO_FOR_TEST.with(|cell| *cell.borrow_mut() = Some((errno, names)));
}

/// The exclusive rename into the trash, its failure classified. There is no
/// fallback to a plain rename: a filesystem without an exclusive rename
/// (`ENOTSUP`, `EINVAL`) keeps the entry, counted as `rename.unsupported`.
fn exclusive_rename(from_dir: RawFd, from: &str, to_dir: RawFd, to: &str) -> Result<(), Refusal> {
    #[cfg(test)]
    if let Some(errno) = RENAME_ERRNO_FOR_TEST.with(|cell| {
        cell.borrow()
            .as_ref()
            .filter(|(_, names)| names.is_empty() || names.iter().any(|name| name == from))
            .map(|(errno, _)| *errno)
    }) {
        return Err(rename_refusal(&std::io::Error::from_raw_os_error(errno)));
    }
    crate::checkout_trash::rename_noreplace(from_dir, from, to_dir, to)
        .map_err(|error| rename_refusal(&error))
}

fn rename_refusal(error: &std::io::Error) -> Refusal {
    if error.raw_os_error() == Some(libc::EXDEV) {
        Refusal::CrossesDevice
    } else {
        Refusal::Failed(MoveFailure::io(MoveStep::Rename, error))
    }
}

/// One finished command run ([`crate::command_temp`]) of a recorded scratch
/// directory: its marker in the done container and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FinishedRun {
    pub(crate) name: String,
    pub(crate) marker: crate::command_temp::FinishedMarker,
}

impl FinishedRun {
    /// The run's path inside `record`'s directory (it stays in the live
    /// container until it is retired).
    pub(crate) fn path(&self, record: &ScratchRecord) -> PathBuf {
        record
            .path()
            .join(crate::command_temp::LIVE_CONTAINER)
            .join(&self.name)
    }
}

/// The verified sub-container `name` of `record`'s directory: a private
/// directory owned by this user on the pool's device.
fn open_run_container(record: &ScratchRecord, name: &str) -> Result<OwnedFd, Refusal> {
    let verified = verify(record)?;
    let entry = openat_dir_nofollow(verified.container.as_raw_fd(), &record.entry)
        .map_err(|error| open_refusal(&error))?;
    let container =
        openat_dir_nofollow(entry.as_raw_fd(), name).map_err(|error| open_refusal(&error))?;
    let stat = fstat(container.as_raw_fd())
        .map_err(|error| Refusal::Failed(MoveFailure::io(MoveStep::Open, &error)))?;
    if !private_directory(&stat) {
        return Err(Refusal::ForeignContainer);
    }
    #[allow(clippy::unnecessary_cast)]
    let device = stat.st_dev as u64;
    if device != record.device {
        return Err(Refusal::CrossesDevice);
    }
    Ok(container)
}

/// Read the marker `name` in the done container, never following a link.
fn read_marker(done: RawFd, name: &str) -> Option<crate::command_temp::FinishedMarker> {
    let stat = fstatat_nofollow(done, name).ok()?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return None;
    }
    let c_marker = c_name(name).ok()?;
    // SAFETY: `c_marker` is NUL-terminated and `done` is open.
    let fd = checked(unsafe {
        libc::openat(
            done,
            c_marker.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })
    .ok()?;
    // SAFETY: `fd` was just opened and is owned here.
    let file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    let mut text = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(file, crate::command_temp::MARKER_MAX_BYTES as u64),
        &mut text,
    )
    .ok()?;
    serde_json::from_slice(&text).ok()
}

/// Finished runs of `record`'s directory, at most `limit`: markers (regular
/// files with a run name the broker writes) in its verified done container.
/// Anything else there is left alone. Read-only.
pub(crate) fn finished_runs(record: &ScratchRecord, limit: usize) -> Vec<FinishedRun> {
    finished_runs_after(record, None, limit).runs
}

/// Finished runs of one scratch directory, in name order from a cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FinishedRunsListing {
    pub(crate) runs: Vec<FinishedRun>,
    /// Every finished run of the directory is in `runs` (the listing was
    /// not cut at its limit).
    pub(crate) complete: bool,
}

/// [`finished_runs`] in name order, starting with the first run named after
/// `after` and wrapping round, so a caller that stores the last run it
/// judged reaches every run in turn, whatever stays behind (kept or
/// failing) at the front of the order. Read-only.
pub(crate) fn finished_runs_after(
    record: &ScratchRecord,
    after: Option<&str>,
    limit: usize,
) -> FinishedRunsListing {
    let Ok(done) = open_run_container(record, crate::command_temp::DONE_CONTAINER) else {
        return FinishedRunsListing::default();
    };
    let Ok(listing) = std::fs::read_dir(record.path().join(crate::command_temp::DONE_CONTAINER))
    else {
        return FinishedRunsListing::default();
    };
    let mut names = listing
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| crate::command_temp::is_run_name(name))
        .collect::<Vec<_>>();
    names.sort_unstable();
    if let Some(after) = after {
        let split = names.partition_point(|name| name.as_str() <= after);
        names.rotate_left(split);
    }
    let mut runs = Vec::new();
    let mut complete = true;
    for name in names {
        if runs.len() >= limit {
            complete = false;
            break;
        }
        if let Some(marker) = read_marker(done.as_raw_fd(), &name) {
            runs.push(FinishedRun { name, marker });
        }
    }
    FinishedRunsListing { runs, complete }
}

/// Whether `record`'s done container lists a finished-run marker. A hint
/// read by path, never acted on: it only decides whether the verified
/// [`finished_runs`] (a descriptor walk from `/`) is worth its cost for an
/// open session, most of which have no finished run.
pub(crate) fn may_have_finished_runs(record: &ScratchRecord) -> bool {
    std::fs::read_dir(record.path().join(crate::command_temp::DONE_CONTAINER)).is_ok_and(
        |mut listing| {
            listing.any(|entry| {
                entry.ok().is_some_and(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(crate::command_temp::is_run_name)
                })
            })
        },
    )
}

/// Remove `run`'s marker from `record`'s done container (only a regular
/// file: `unlinkat` without flags never removes a directory).
pub(crate) fn forget_finished_run(record: &ScratchRecord, run: &FinishedRun) {
    let Ok(done) = open_run_container(record, crate::command_temp::DONE_CONTAINER) else {
        return;
    };
    if !crate::command_temp::is_run_name(&run.name) {
        return;
    }
    if let Ok(name) = c_name(&run.name) {
        // SAFETY: `name` is NUL-terminated and `done` is an open directory.
        unsafe {
            libc::unlinkat(done.as_raw_fd(), name.as_ptr(), 0);
        }
    }
}

/// Move one finished run from `record`'s live container into the pool's
/// checkout trash with one exclusive same-volume rename, exactly as
/// [`move_to_trash`] moves a whole scratch directory: the scratch directory
/// verifies, the live container is private and on the pool's device, the
/// run is a real directory (never a link, which is not followed) with the
/// identity its marker recorded, the row is recorded pending first, and the
/// identity is checked again right before the rename. Nothing is unlinked.
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
    let live = open_run_container(record, crate::command_temp::LIVE_CONTAINER)?;
    let identity = (run.marker.device, run.marker.inode);
    let check = |live: RawFd| match fstatat_nofollow(live, &run.name) {
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Err(Refusal::Missing),
        Err(error) => Err(Refusal::Failed(MoveFailure::io(MoveStep::Inspect, &error))),
        Ok(stat) if !is_directory(&stat) => Err(Refusal::NotARealDirectory),
        Ok(stat) => {
            #[allow(clippy::unnecessary_cast)]
            let found = (stat.st_dev as u64, stat.st_ino as u64);
            if found == identity {
                Ok(())
            } else {
                Err(Refusal::IdentityChanged)
            }
        }
    };
    check(live.as_raw_fd())?;
    if run.marker.device != record.device {
        return Err(Refusal::CrossesDevice);
    }
    let trash = open_trash_for_move(&record.pool)?;
    if trash.device != record.device {
        return Err(Refusal::CrossesDevice);
    }
    let trash = trash.trash;
    let entry = crate::checkout_trash::new_entry_name(record.session_id);
    let (creator_pid, creator_started_ms) = crate::checkout_trash::this_process_identity();
    let mut row = crate::checkout_trash::TrashRow {
        device: run.marker.device,
        inode: run.marker.inode,
        session_id: record.session_id,
        failures: 0,
        pending: true,
        creator_pid,
        creator_started_ms,
        stripped: true,
        scratch: true,
        verification: false,
    };
    record_pending(trash.path(), &entry, row).map_err(|_| Refusal::Failed(STORE_FAILURE))?;
    check(live.as_raw_fd())?;
    exclusive_rename(live.as_raw_fd(), &run.name, trash.trash_fd(), &entry)?;
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

    /// A missing pool is unavailable only when its nearest existing
    /// ancestor is on another device than recorded (a volume that is not
    /// mounted leaves its mount point on the parent volume); on the
    /// recorded device the volume is mounted and the pool is really gone.
    #[test]
    fn a_missing_pool_is_unavailable_only_off_the_recorded_device() {
        use std::os::unix::fs::MetadataExt as _;
        let (_owner, pool) = pool();
        let mut record = create(&pool, 5).unwrap();
        let device = std::fs::metadata(&pool).unwrap().dev();
        assert_eq!(record.device, device);
        assert!(!pool_unavailable(&record), "a present pool is available");

        std::fs::remove_dir_all(&pool).unwrap();
        assert!(
            !pool_unavailable(&record),
            "removed from a mounted volume: really gone"
        );
        record.pool = pool.join("deeper").join("still");
        assert!(
            !pool_unavailable(&record),
            "the nearest existing ancestor decides, however far up"
        );

        record.device = device.wrapping_add(1000);
        assert!(
            pool_unavailable(&record),
            "an ancestor on another volume: the recorded one is away"
        );
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

    /// A finished run is found by its marker and moved into the trash; a
    /// run swapped for a symbolic link before the move is not followed.
    #[test]
    fn a_finished_run_moves_and_a_swapped_link_is_not_followed() {
        let (_owner, pool) = pool();
        let record = create(&pool, 21).unwrap();
        let bases = [PathBuf::from("/tmp")];
        let mut record_pending =
            |_: &Path, _: &str, _: crate::checkout_trash::TrashRow| Ok::<(), String>(());
        let temp =
            crate::command_temp::CommandTemp::create_in(&record.path(), &bases, false).unwrap();
        std::fs::write(temp.run_dir().join("left.tmp"), b"x").unwrap();
        drop(temp);
        let runs = finished_runs(&record, 10);
        assert_eq!(runs.len(), 1, "{runs:?}");
        let moved = move_finished_run_to_trash(&record, &runs[0], &mut record_pending).unwrap();
        assert!(
            moved
                .trash_dir
                .join(&moved.entry)
                .join("left.tmp")
                .is_file()
        );
        assert!(moved.row.scratch && moved.row.stripped);
        forget_finished_run(&record, &runs[0]);
        assert!(finished_runs(&record, 10).is_empty());

        let temp =
            crate::command_temp::CommandTemp::create_in(&record.path(), &bases, false).unwrap();
        let run = temp.run_dir();
        std::fs::write(run.join("left.tmp"), b"x").unwrap();
        drop(temp);
        let runs = finished_runs(&record, 10);
        assert_eq!(runs.len(), 1);
        let target = pool.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep.txt"), b"not the broker's").unwrap();
        std::fs::remove_file(run.join("left.tmp")).unwrap();
        std::fs::remove_dir(&run).unwrap();
        std::os::unix::fs::symlink(&target, &run).unwrap();
        assert_eq!(
            move_finished_run_to_trash(&record, &runs[0], &mut record_pending).err(),
            Some(Refusal::NotARealDirectory)
        );
        assert!(
            target.join("keep.txt").is_file(),
            "a link is never followed"
        );
        assert!(
            std::fs::symlink_metadata(&run)
                .unwrap()
                .file_type()
                .is_symlink()
        );
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
