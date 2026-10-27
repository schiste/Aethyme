//! Per-checkout admission for work that may read or write managed artifacts.
//!
//! A producer holds a shared advisory lock and a durable marker for the
//! lifetime of its managed work. A future collector may proceed only after a
//! nonblocking exclusive try succeeds and the bounded marker inventory is
//! complete and empty. The marker is deliberately not removed by `Drop`: a
//! crashed producer may have left descendants running after its lock descriptor
//! was closed. Old clients that do not use this protocol remain unknown; this
//! module does not grant deletion authority.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MARKER_SCHEMA_VERSION: u32 = 1;
const MAX_MARKERS_PER_CHECKOUT: usize = 256;
const MAX_MARKER_BYTES: u64 = 16 * 1024;
static NEXT_MARKER_NONCE: AtomicU64 = AtomicU64::new(1);

/// Immutable process identity captured after a command starts. The start token
/// is opaque and comparable only on the host that created the marker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ProcessStartIdentity {
    pub(crate) pid: i32,
    pub(crate) process_group: i32,
    pub(crate) start_time: Option<u64>,
    pub(crate) start_time_clock: String,
}

impl ProcessStartIdentity {
    /// Capture the process start token when the current platform exposes one.
    /// An absent token is recorded as unknown and must not be treated as proof
    /// that a later process with the same PID is the producer.
    pub(crate) fn capture(pid: i32, process_group: i32) -> io::Result<Self> {
        if pid <= 1 || process_group <= 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "invalid artifact producer process identity pid={pid} pgid={process_group}"
                ),
            ));
        }
        let (start_time, start_time_clock) = process_start_token(pid)?;
        Ok(Self {
            pid,
            process_group,
            start_time,
            start_time_clock: start_time_clock.to_owned(),
        })
    }
}

/// Durable evidence for an unsettled or in-flight producer invocation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ArtifactUseMarker {
    pub(crate) schema_version: u32,
    pub(crate) nonce: String,
    pub(crate) primary_root: String,
    pub(crate) primary_device: u64,
    pub(crate) primary_inode: u64,
    pub(crate) checkout_root: String,
    pub(crate) checkout_device: u64,
    pub(crate) checkout_inode: u64,
    pub(crate) checkout_key: String,
    pub(crate) session_id: i64,
    pub(crate) invocation_id: String,
    pub(crate) phase: ArtifactUsePhase,
    pub(crate) process: Option<ProcessStartIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactUsePhase {
    PreDispatch,
    Running,
}

/// Why a future artifact reclamation attempt could not prove the checkout idle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArtifactUseReclaimBlock {
    /// A producer or another exclusive user currently holds the inode lock.
    Contended,
    /// The advisory lock was available, but durable producer markers remain.
    UnknownProducer { markers: Vec<ArtifactUseMarker> },
}

/// Result of a bounded, nonblocking attempt to reserve a checkout for cleanup.
#[derive(Debug)]
pub(crate) enum ArtifactUseReclaimTry {
    Acquired(ArtifactUseReclaimer),
    Blocked(ArtifactUseReclaimBlock),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ArtifactUseLocations {
    primary_root: PathBuf,
    primary_identity: DirectoryIdentity,
    checkout_root: PathBuf,
    checkout_identity: DirectoryIdentity,
    checkout_key: String,
    runtime_dir: PathBuf,
    lock_path: PathBuf,
    marker_dir: PathBuf,
}

impl ArtifactUseLocations {
    fn resolve(primary_root: &Path, checkout_root: &Path) -> io::Result<Self> {
        let primary_root = primary_root.canonicalize()?;
        let checkout_root = checkout_root.canonicalize()?;
        let primary_identity = directory_identity(&primary_root)?;
        let checkout_identity = directory_identity(&checkout_root)?;
        let checkout_key = checkout_key(&checkout_root)?;

        let run_dir = super::repository_runtime_directory(&primary_root)?;
        let runtime_dir = run_dir.join("artifact-use");
        super::ensure_real_directory(&runtime_dir)?;
        let canonical_runtime = runtime_dir.canonicalize()?;
        if canonical_runtime != runtime_dir || !canonical_runtime.starts_with(&run_dir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "artifact-use runtime directory is not a real directory under the primary repository: {}",
                    runtime_dir.display()
                ),
            ));
        }

        Ok(Self {
            primary_root,
            primary_identity,
            checkout_root,
            checkout_identity,
            lock_path: runtime_dir.join(format!("{checkout_key}.lock")),
            marker_dir: runtime_dir.join(format!("{checkout_key}.markers")),
            checkout_key,
            runtime_dir,
        })
    }

    fn require_unchanged(&self) -> io::Result<()> {
        let current = Self::resolve(&self.primary_root, &self.checkout_root)?;
        if current != *self {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact-use primary or checkout root changed while its lock was acquired",
            ));
        }
        Ok(())
    }

    fn ensure_marker_dir(&self) -> io::Result<()> {
        super::ensure_real_directory(&self.marker_dir)?;
        let canonical = self.marker_dir.canonicalize()?;
        if canonical != self.marker_dir || !canonical.starts_with(&self.runtime_dir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "artifact-use marker directory is not a real child of the runtime directory: {}",
                    self.marker_dir.display()
                ),
            ));
        }
        Ok(())
    }

    fn ensure_marker_dir_for_producer(&self, lock_preexisted: bool) -> io::Result<()> {
        match fs::symlink_metadata(&self.marker_dir) {
            Ok(metadata) if metadata.file_type().is_dir() => self.ensure_marker_dir(),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact-use marker path is not a real directory",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound && !lock_preexisted => {
                self.ensure_marker_dir()
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-use marker directory is missing for an existing lock; retain the checkout",
            )),
            Err(error) => Err(error),
        }
    }

    fn ensure_marker_dir_for_reclaimer(&self, lock_preexisted: bool) -> io::Result<()> {
        self.ensure_marker_dir_for_producer(lock_preexisted)
    }

    fn marker_for(&self, marker: &ArtifactUseMarker) -> io::Result<()> {
        if marker.schema_version != MARKER_SCHEMA_VERSION
            || marker.primary_root != path_text(&self.primary_root)?
            || marker.primary_device != self.primary_identity.device
            || marker.primary_inode != self.primary_identity.inode
            || marker.checkout_root != path_text(&self.checkout_root)?
            || marker.checkout_device != self.checkout_identity.device
            || marker.checkout_inode != self.checkout_identity.inode
            || marker.checkout_key != self.checkout_key
            || marker.nonce.is_empty()
            || marker.session_id <= 0
            || marker.invocation_id.is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-use marker does not match its primary and checkout identities",
            ));
        }
        Ok(())
    }
}

/// A shared producer claim. Dropping this value releases only the advisory
/// lock; it never clears the durable marker. Callers must explicitly finish a
/// confirmed-not-started invocation or one whose process tree is quiescent.
#[derive(Debug)]
pub(crate) struct ArtifactUseProducer {
    _lock: super::SharedFileLock,
    marker_file: File,
    marker_path: PathBuf,
    marker: ArtifactUseMarker,
    locations: ArtifactUseLocations,
}

impl ArtifactUseProducer {
    pub(crate) fn marker(&self) -> &ArtifactUseMarker {
        &self.marker
    }

    /// Record the real command process after dispatch. `start_time` may be
    /// absent on platforms that cannot distinguish PID reuse; such a marker
    /// remains unknown to automated cleanup until stronger evidence exists.
    pub(crate) fn mark_process_started(&mut self, process: ProcessStartIdentity) -> io::Result<()> {
        if process.pid <= 1 || process.process_group <= 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact producer process identity has a reserved PID or process group",
            ));
        }
        self.marker.phase = ArtifactUsePhase::Running;
        self.marker.process = Some(process);
        self.persist_marker()
    }

    /// Clear a marker only when dispatch is known not to have occurred.
    pub(crate) fn finish_not_started(self) -> io::Result<()> {
        if self.marker.phase != ArtifactUsePhase::PreDispatch || self.marker.process.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "cannot clear an artifact-use marker after command dispatch",
            ));
        }
        self.remove_marker()
    }

    /// Clear a marker only after the caller has independently verified process
    /// tree quiescence. This method is intentionally explicit; `Drop` is not
    /// evidence that a child or descendant stopped.
    pub(crate) fn finish_verified_quiescence(self) -> io::Result<()> {
        self.remove_marker()
    }

    fn persist_marker(&mut self) -> io::Result<()> {
        self.locations.require_unchanged()?;
        super::verify_named_lock_file(&self.marker_path, &self.marker_file)?;
        let bytes = serde_json::to_vec(&self.marker)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if bytes.len() as u64 > MAX_MARKER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-use marker exceeds its bounded record size",
            ));
        }
        self.marker_file.set_len(0)?;
        self.marker_file.seek(SeekFrom::Start(0))?;
        self.marker_file.write_all(&bytes)?;
        self.marker_file.sync_all()?;
        sync_directory(&self.locations.marker_dir)
    }

    fn remove_marker(self) -> io::Result<()> {
        self.locations.require_unchanged()?;
        super::verify_named_lock_file(&self.marker_path, &self.marker_file)?;
        fs::remove_file(&self.marker_path)?;
        sync_directory(&self.locations.marker_dir)
    }
}

/// An exclusive reservation returned only when the full durable marker
/// inventory was readable and empty. It is a synchronization primitive, not
/// cleanup authority.
#[derive(Debug)]
pub(crate) struct ArtifactUseReclaimer {
    _lock: super::ExclusiveFileLock,
}

/// Begin a protocol-aware producer lifetime for one canonical checkout.
/// Session IDs are evidence in the marker, not part of the lock key: distinct
/// broker sessions that name the same checkout contend on the same inode.
pub(crate) fn begin_artifact_use(
    primary_root: &Path,
    checkout_root: &Path,
    session_id: i64,
    invocation_id: &str,
) -> io::Result<ArtifactUseProducer> {
    if session_id <= 0 || invocation_id.is_empty() || invocation_id.len() > 256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact-use admission requires a positive session ID and a bounded invocation ID",
        ));
    }
    let locations = ArtifactUseLocations::resolve(primary_root, checkout_root)?;
    let lock_preexisted = match fs::symlink_metadata(&locations.lock_path) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    let file = super::open_lock_file_nofollow(&locations.lock_path)?;
    let lock = super::SharedFileLock::acquire(file)?;
    locations.require_unchanged()?;
    super::verify_named_lock_file(&locations.lock_path, &lock.file)?;
    if !lock_preexisted {
        sync_directory(&locations.runtime_dir)?;
    }
    locations.ensure_marker_dir_for_producer(lock_preexisted)?;

    let nonce = next_nonce();
    let marker_path = locations.marker_dir.join(format!("{nonce}.json"));
    let marker = ArtifactUseMarker {
        schema_version: MARKER_SCHEMA_VERSION,
        nonce,
        primary_root: path_text(&locations.primary_root)?,
        primary_device: locations.primary_identity.device,
        primary_inode: locations.primary_identity.inode,
        checkout_root: path_text(&locations.checkout_root)?,
        checkout_device: locations.checkout_identity.device,
        checkout_inode: locations.checkout_identity.inode,
        checkout_key: locations.checkout_key.clone(),
        session_id,
        invocation_id: invocation_id.to_owned(),
        phase: ArtifactUsePhase::PreDispatch,
        process: None,
    };
    locations.marker_for(&marker)?;
    let marker_file = create_marker_file(&marker_path)?;
    if let Err(error) = write_new_marker(&marker_path, &marker_file, &marker, &locations.marker_dir)
    {
        // No command has been dispatched yet, so removal is safe. If cleanup
        // or its directory sync fails, the remaining marker is conservatively
        // retained for future reconciliation.
        let cleanup = remove_new_marker(&marker_path, &marker_file, &locations.marker_dir);
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(io::Error::new(
                error.kind(),
                format!("{error}; pre-dispatch marker cleanup also failed: {cleanup_error}"),
            )),
        };
    }
    if let Err(error) = locations.require_unchanged() {
        let cleanup = remove_new_marker(&marker_path, &marker_file, &locations.marker_dir);
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(io::Error::new(
                error.kind(),
                format!("{error}; pre-dispatch marker cleanup also failed: {cleanup_error}"),
            )),
        };
    }
    Ok(ArtifactUseProducer {
        _lock: lock,
        marker_file,
        marker_path,
        marker,
        locations,
    })
}

/// Make a bounded, nonblocking exclusive attempt for a future reclaimer. Any
/// marker, malformed entry, cap overflow, or I/O error must retain the
/// checkout; this function never deletes or repairs markers.
pub(crate) fn try_artifact_use_reclaimer(
    primary_root: &Path,
    checkout_root: &Path,
) -> io::Result<ArtifactUseReclaimTry> {
    let locations = ArtifactUseLocations::resolve(primary_root, checkout_root)?;
    let lock_preexisted = match fs::symlink_metadata(&locations.lock_path) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    let file = super::open_lock_file_nofollow(&locations.lock_path)?;
    let Some(lock) = super::ExclusiveFileLock::try_acquire(file)? else {
        return Ok(ArtifactUseReclaimTry::Blocked(
            ArtifactUseReclaimBlock::Contended,
        ));
    };
    locations.require_unchanged()?;
    super::verify_named_lock_file(&locations.lock_path, &lock.file)?;
    if !lock_preexisted {
        sync_directory(&locations.runtime_dir)?;
    }

    locations.ensure_marker_dir_for_reclaimer(lock_preexisted)?;
    let markers = read_marker_inventory(&locations)?;
    locations.require_unchanged()?;
    if markers.is_empty() {
        Ok(ArtifactUseReclaimTry::Acquired(ArtifactUseReclaimer {
            _lock: lock,
        }))
    } else {
        Ok(ArtifactUseReclaimTry::Blocked(
            ArtifactUseReclaimBlock::UnknownProducer { markers },
        ))
    }
}

fn read_marker_inventory(locations: &ArtifactUseLocations) -> io::Result<Vec<ArtifactUseMarker>> {
    let directory_metadata = match fs::symlink_metadata(&locations.marker_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-use marker directory disappeared during inventory; retain the checkout",
            ));
        }
        Err(error) => return Err(error),
    };
    if !directory_metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact-use marker path is not a real directory",
        ));
    }
    locations.ensure_marker_dir()?;

    let entries = fs::read_dir(&locations.marker_dir)?;
    let mut markers = Vec::new();
    let mut count = 0_usize;
    for entry in entries {
        let entry = entry?;
        count += 1;
        if count > MAX_MARKERS_PER_CHECKOUT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-use marker inventory exceeded its cap; retain the checkout",
            ));
        }
        let name = entry.file_name().into_string().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-use marker inventory contains a non-UTF-8 name",
            )
        })?;
        let Some(nonce) = name.strip_suffix(".json") else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected artifact-use marker entry {name:?}"),
            ));
        };
        if nonce.is_empty() || nonce.contains('/') || nonce.contains('\\') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-use marker has an invalid nonce filename",
            ));
        }
        let path = entry.path();
        let marker = read_marker_file(&path)?;
        locations.marker_for(&marker)?;
        if marker.nonce != nonce {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-use marker nonce does not match its filename",
            ));
        }
        markers.push(marker);
    }
    Ok(markers)
}

fn read_marker_file(path: &Path) -> io::Result<ArtifactUseMarker> {
    let mut file = open_existing_nofollow(path)?;
    super::verify_named_lock_file(path, &file)?;
    let before = file.metadata()?;
    if before.len() > MAX_MARKER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact-use marker exceeds its bounded record size",
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    super::verify_named_lock_file(path, &file)?;
    if before.len() != after.len() || bytes.len() as u64 != after.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact-use marker changed while it was read",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn create_marker_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    configure_nofollow(&mut options);
    let file = options.open(path)?;
    super::verify_named_lock_file(path, &file)?;
    Ok(file)
}

fn open_existing_nofollow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options);
    options.open(path)
}

fn configure_nofollow(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open a reparse point itself; metadata verification rejects it.
        options.custom_flags(0x0020_0000);
    }
}

fn write_new_marker(
    path: &Path,
    file: &File,
    marker: &ArtifactUseMarker,
    marker_dir: &Path,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(marker)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if bytes.len() as u64 > MAX_MARKER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact-use marker exceeds its bounded record size",
        ));
    }
    let mut file = file;
    file.write_all(&bytes)?;
    file.sync_all()?;
    super::verify_named_lock_file(path, file)?;
    sync_directory(marker_dir)
}

fn remove_new_marker(path: &Path, file: &File, marker_dir: &Path) -> io::Result<()> {
    super::verify_named_lock_file(path, file)?;
    fs::remove_file(path)?;
    sync_directory(marker_dir)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn path_text(path: &Path) -> io::Result<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact-use root path is not representable as UTF-8",
        )
    })
}

fn checkout_key(checkout_root: &Path) -> io::Result<String> {
    let root = checkout_root.as_os_str().as_encoded_bytes();
    let mut digest = Sha256::new();
    digest.update(b"aethyme-artifact-use-v1\0");
    digest.update(root);
    Ok(format!("{:x}", digest.finalize()))
}

fn next_nonce() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_MARKER_NONCE.fetch_add(1, Ordering::Relaxed);
    format!("{:08x}-{now:032x}-{sequence:016x}", std::process::id())
}

fn directory_identity(path: &Path) -> io::Result<DirectoryIdentity> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("artifact-use root is not a directory: {}", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let device = metadata.volume_serial_number().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "artifact-use root volume identity is unavailable",
            )
        })?;
        let inode = metadata.file_index().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "artifact-use root file identity is unavailable",
            )
        })?;
        Ok(DirectoryIdentity {
            device: u64::from(device),
            inode,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "artifact-use root identity is unavailable on this platform",
        ))
    }
}

fn process_start_token(pid: i32) -> io::Result<(Option<u64>, &'static str)> {
    #[cfg(target_os = "macos")]
    {
        if pid <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process ID must be positive",
            ));
        }
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: `info` is a writable buffer of exactly `size` bytes, and
        // proc_pidinfo writes at most `size` bytes into it.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if written != size {
            return Ok((None, "macos_epoch_microseconds"));
        }
        // SAFETY: the buffer was zero-initialized and then fully written, and
        // proc_bsdinfo is plain integers and byte arrays, valid for any bits.
        let info = unsafe { info.assume_init() };
        let start_time = info
            .pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec);
        Ok((Some(start_time), "macos_epoch_microseconds"))
    }
    #[cfg(target_os = "linux")]
    {
        if pid <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process ID must be positive",
            ));
        }
        let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((None, "linux_clock_ticks"));
            }
            Err(error) => return Err(error),
        };
        let tail = stat.rsplit_once(')').map(|(_, tail)| tail).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed Linux process stat")
        })?;
        let start_time = tail
            .split_whitespace()
            .nth(19)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "missing Linux process start time",
                )
            })?;
        Ok((Some(start_time), "linux_clock_ticks"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        Ok((None, "unavailable"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    fn checkout(primary: &Path, name: &str) -> PathBuf {
        let path = primary.join(name);
        fs::create_dir(&path).unwrap();
        path
    }

    fn begin(
        primary: &Path,
        checkout: &Path,
        session: i64,
        invocation: &str,
    ) -> ArtifactUseProducer {
        begin_artifact_use(primary, checkout, session, invocation).unwrap()
    }

    fn marker(locations: &ArtifactUseLocations, nonce: String) -> ArtifactUseMarker {
        ArtifactUseMarker {
            schema_version: MARKER_SCHEMA_VERSION,
            nonce,
            primary_root: path_text(&locations.primary_root).unwrap(),
            primary_device: locations.primary_identity.device,
            primary_inode: locations.primary_identity.inode,
            checkout_root: path_text(&locations.checkout_root).unwrap(),
            checkout_device: locations.checkout_identity.device,
            checkout_inode: locations.checkout_identity.inode,
            checkout_key: locations.checkout_key.clone(),
            session_id: 99,
            invocation_id: "test:99:invocation".into(),
            phase: ArtifactUsePhase::PreDispatch,
            process: None,
        }
    }

    #[test]
    fn producer_marker_is_durable_before_dispatch_and_shared_per_checkout() {
        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let producer = begin(primary.path(), &checkout, 11, "gate:11:test");
        let second_session = begin(primary.path(), &checkout, 12, "exec:12:same-root");
        let marker_path = producer.marker_path.clone();
        let marker = read_marker_file(&marker_path).unwrap();
        assert_eq!(marker.phase, ArtifactUsePhase::PreDispatch);
        assert_eq!(marker.session_id, 11);
        assert_eq!(marker.invocation_id, "gate:11:test");
        assert_eq!(
            producer.marker().checkout_key,
            second_session.marker().checkout_key
        );
        assert_eq!(
            marker.checkout_root,
            checkout.canonicalize().unwrap().display().to_string()
        );

        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &checkout).unwrap(),
            ArtifactUseReclaimTry::Blocked(ArtifactUseReclaimBlock::Contended)
        ));
        drop(producer);
        drop(second_session);
        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &checkout).unwrap(),
            ArtifactUseReclaimTry::Blocked(ArtifactUseReclaimBlock::UnknownProducer { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn checkout_aliases_resolve_to_the_same_artifact_use_key() {
        use std::os::unix::fs::symlink;

        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let alias = primary.path().join("checkout-alias");
        symlink(&checkout, &alias).unwrap();

        let first = begin(primary.path(), &checkout, 18, "test:18:canonical");
        let second = begin(primary.path(), &alias, 19, "test:19:alias");
        assert_eq!(first.marker().checkout_key, second.marker().checkout_key);
        assert_eq!(first.marker().checkout_root, second.marker().checkout_root);
        first.finish_not_started().unwrap();
        second.finish_not_started().unwrap();
    }

    #[test]
    fn different_checkout_does_not_wait_and_verified_settlement_clears_its_marker() {
        let primary = tempfile::tempdir().unwrap();
        let first_checkout = checkout(primary.path(), "first");
        let second_checkout = checkout(primary.path(), "second");
        let mut producer = begin(primary.path(), &first_checkout, 12, "prepare:12:deps");
        producer
            .mark_process_started(ProcessStartIdentity {
                pid: 1234,
                process_group: 1234,
                start_time: Some(5678),
                start_time_clock: "test_clock".into(),
            })
            .unwrap();
        let unrelated = try_artifact_use_reclaimer(primary.path(), &second_checkout).unwrap();
        assert!(matches!(unrelated, ArtifactUseReclaimTry::Acquired(_)));

        producer.finish_verified_quiescence().unwrap();
        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &first_checkout).unwrap(),
            ArtifactUseReclaimTry::Acquired(_)
        ));
    }

    #[test]
    fn confirmed_not_started_removes_the_marker_without_poisoning_future_admission() {
        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let producer = begin(primary.path(), &checkout, 13, "exec:13:unavailable");
        producer.finish_not_started().unwrap();
        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &checkout).unwrap(),
            ArtifactUseReclaimTry::Acquired(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn process_death_leaves_the_durable_marker_for_reconciliation() {
        const CHILD: &str = "AETHYME_ARTIFACT_USE_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let primary =
                PathBuf::from(std::env::var_os("AETHYME_ARTIFACT_USE_TEST_PRIMARY").unwrap());
            let checkout =
                PathBuf::from(std::env::var_os("AETHYME_ARTIFACT_USE_TEST_CHECKOUT").unwrap());
            let mut producer =
                begin_artifact_use(&primary, &checkout, 15, "test:15:process-death").unwrap();
            use std::os::unix::process::CommandExt;
            let mut command = Command::new("/bin/sleep");
            command.arg("60").process_group(0);
            let _command_child = command.spawn().unwrap();
            let command_pid = _command_child.id() as i32;
            let process = ProcessStartIdentity::capture(command_pid, command_pid).unwrap();
            producer.mark_process_started(process).unwrap();
            println!("artifact-use-ready");
            io::stdout().flush().unwrap();
            let mut byte = [0_u8; 1];
            let _ = io::stdin().read(&mut byte);
            return;
        }

        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        use std::os::unix::process::CommandExt;
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("process_death_leaves_the_durable_marker_for_reconciliation")
            .arg("--nocapture")
            .env(CHILD, "1")
            .env("AETHYME_ARTIFACT_USE_TEST_PRIMARY", primary.path())
            .env("AETHYME_ARTIFACT_USE_TEST_CHECKOUT", &checkout)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0);
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut output = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "producer child exited before publishing its ready handshake"
            );
            if line.trim() == "artifact-use-ready" {
                break;
            }
        }
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success(), "the producer process should be killed");

        let markers = match try_artifact_use_reclaimer(primary.path(), &checkout).unwrap() {
            ArtifactUseReclaimTry::Blocked(ArtifactUseReclaimBlock::UnknownProducer {
                markers,
            }) => markers,
            other => panic!("producer death should leave an unknown marker, got {other:?}"),
        };
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].phase, ArtifactUsePhase::Running);
        let process = markers[0].process.as_ref().unwrap();
        assert_ne!(process.pid as u32, std::process::id());
        assert!(process.start_time.is_some());
        assert_eq!(
            ProcessStartIdentity::capture(process.pid, process.process_group)
                .unwrap()
                .start_time,
            process.start_time
        );
        // SAFETY: the marker recorded this process group and its start token
        // still matches, so the group belongs to this test's command child.
        let killed = unsafe { libc::kill(-process.process_group, libc::SIGKILL) };
        assert!(killed == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH));
    }

    #[test]
    fn marker_inventory_fails_closed_on_malformed_and_over_limit_entries() {
        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let locations = ArtifactUseLocations::resolve(primary.path(), &checkout).unwrap();
        locations.ensure_marker_dir().unwrap();
        fs::write(locations.marker_dir.join("bad.json"), b"not-json").unwrap();
        assert!(try_artifact_use_reclaimer(primary.path(), &checkout).is_err());

        fs::remove_file(locations.marker_dir.join("bad.json")).unwrap();
        for index in 0..=MAX_MARKERS_PER_CHECKOUT {
            let marker = marker(&locations, format!("bounded-{index}"));
            let path = locations.marker_dir.join(format!("{}.json", marker.nonce));
            fs::write(path, serde_json::to_vec(&marker).unwrap()).unwrap();
        }
        let error = try_artifact_use_reclaimer(primary.path(), &checkout).unwrap_err();
        assert!(error.to_string().contains("exceeded its cap"));
    }

    #[cfg(unix)]
    #[test]
    fn lock_path_symlink_is_refused_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let locations = ArtifactUseLocations::resolve(primary.path(), &checkout).unwrap();
        let target = primary.path().join("lock-target");
        fs::write(&target, b"preserve").unwrap();
        symlink(&target, &locations.lock_path).unwrap();

        assert!(begin_artifact_use(primary.path(), &checkout, 14, "exec:14:link").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"preserve");
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_held_lock_inode_cannot_hide_its_live_marker() {
        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let locations = ArtifactUseLocations::resolve(primary.path(), &checkout).unwrap();
        let producer = begin(primary.path(), &checkout, 16, "gate:16:replacement");
        let displaced = locations.runtime_dir.join("displaced.lock");
        fs::rename(&locations.lock_path, &displaced).unwrap();
        fs::write(&locations.lock_path, b"replacement inode").unwrap();

        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &checkout).unwrap(),
            ArtifactUseReclaimTry::Blocked(ArtifactUseReclaimBlock::UnknownProducer { .. })
        ));
        drop(producer);
        assert!(displaced.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_marker_name_never_unlinks_its_new_target() {
        use std::os::unix::fs::symlink;

        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let producer = begin(primary.path(), &checkout, 17, "exec:17:replacement");
        let target = primary.path().join("marker-target");
        fs::write(&target, b"preserve").unwrap();
        let displaced = producer.marker_path.with_extension("displaced");
        fs::rename(&producer.marker_path, &displaced).unwrap();
        symlink(&target, &producer.marker_path).unwrap();

        assert!(producer.finish_verified_quiescence().is_err());
        assert_eq!(fs::read(&target).unwrap(), b"preserve");
        assert!(try_artifact_use_reclaimer(primary.path(), &checkout).is_err());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn process_start_identity_names_the_current_process() {
        // SAFETY: getpgrp takes no pointers and reads this process's group ID.
        let process_group = unsafe { libc::getpgrp() };
        let identity =
            ProcessStartIdentity::capture(std::process::id() as i32, process_group).unwrap();
        assert!(identity.start_time.is_some());
        assert!(!identity.start_time_clock.is_empty());
    }
}
