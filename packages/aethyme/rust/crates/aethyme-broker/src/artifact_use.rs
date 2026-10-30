//! Per-checkout admission for work that may read or write managed artifacts.
//!
//! A producer holds a shared advisory lock and a durable marker for the
//! lifetime of its managed work. A future collector may proceed only after a
//! nonblocking exclusive try succeeds and the bounded marker inventory is
//! complete and empty. The marker is deliberately not removed by `Drop`: a
//! crashed producer may have left descendants running after its lock descriptor
//! was closed. Old clients that do not use this protocol remain unknown; this
//! module does not grant deletion authority.
//! Synchronous preparation also holds a shared scope lock; each child it
//! dispatches acquires its own durable producer marker before dispatch.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MARKER_SCHEMA_VERSION: u32 = 1;
// B1 exercises the fail-closed reclaimer surface in tests but has no production
// cleanup caller yet. These item-scoped allowances cover only that surface.
#[cfg_attr(not(test), allow(dead_code))]
const MAX_MARKERS_PER_CHECKOUT: usize = 256;
const MAX_MARKER_BYTES: u64 = 16 * 1024;
const ARTIFACT_LAYOUT_LOCK_WAIT: Duration = Duration::from_secs(1);
const ARTIFACT_LAYOUT_LOCK_POLL: Duration = Duration::from_millis(10);
static NEXT_MARKER_NONCE: AtomicU64 = AtomicU64::new(1);

/// Immutable owned-process identity captured at the dispatch boundary. The
/// start token is opaque and comparable only on the host that created the
/// marker.
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
        let (start_time, start_time_clock) =
            process_start_token(pid).unwrap_or((None, "unavailable"));
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
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArtifactUseReclaimBlock {
    /// A producer or another exclusive user currently holds the inode lock.
    Contended,
    /// The advisory lock was available, but durable producer markers remain.
    UnknownProducer { markers: Vec<ArtifactUseMarker> },
    /// This target does not support the protocol; automatic cleanup must stay off.
    ProtocolUnavailable,
}

/// Result of a bounded, nonblocking attempt to reserve a checkout for cleanup.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub(crate) enum ArtifactUseReclaimTry {
    Acquired(ArtifactUseReclaimer),
    Blocked(ArtifactUseReclaimBlock),
}

/// A caller may continue ordinary work without admission only when cleanup
/// remains disabled for every checkout on this unsupported target.
pub(crate) enum ArtifactUseAdmission {
    Admitted(Box<ArtifactUseProducer>),
    ProtocolUnavailable,
}

/// A shared checkout reservation for synchronous managed work that does not
/// dispatch a child. Child processes still need their own durable producer
/// markers, because this lock is released if the broker process exits.
pub(crate) enum ArtifactUseScopeAdmission {
    Admitted(ArtifactUseScope),
    ProtocolUnavailable,
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
    layout_lock_path: PathBuf,
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
            layout_lock_path: runtime_dir.join(format!("{checkout_key}.init.lock")),
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

    /// Initialize the persistent lock and marker layout under a short
    /// per-checkout exclusive lock. Marker storage is created and synced
    /// before the producer lock appears; a pre-existing producer lock with a
    /// missing marker directory is retained as unknown evidence.
    fn initialize_layout(&self, wait: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + wait;
        let layout_lock = loop {
            let file = super::open_lock_file_nofollow(&self.layout_lock_path)?;
            super::verify_named_lock_file(&self.layout_lock_path, &file)?;
            match super::ExclusiveFileLock::try_acquire(file)? {
                Some(lock) => break lock,
                None if Instant::now() >= deadline => return Ok(false),
                None => thread::sleep(
                    ARTIFACT_LAYOUT_LOCK_POLL
                        .min(deadline.saturating_duration_since(Instant::now())),
                ),
            }
        };
        super::verify_named_lock_file(&self.layout_lock_path, &layout_lock.file)?;

        self.require_unchanged()?;
        let lock_preexisted = path_exists(&self.lock_path)?;
        self.ensure_marker_dir_for_producer(lock_preexisted)?;
        let file = super::open_lock_file_nofollow(&self.lock_path)?;
        super::verify_named_lock_file(&self.lock_path, &file)?;
        self.sync_runtime_directory_chain()?;
        self.require_unchanged()?;
        drop(layout_lock);
        Ok(true)
    }

    /// Persist the complete runtime-directory path and its entries before a
    /// producer marker can authorize any managed writes.
    fn sync_runtime_directory_chain(&self) -> io::Result<()> {
        let state_dir = self.primary_root.join(".aethyme");
        let run_dir = state_dir.join("run");
        sync_directory(&self.runtime_dir)?;
        sync_directory(&run_dir)?;
        sync_directory(&state_dir)?;
        sync_directory(&self.primary_root)
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
            || marker.session_id < 0
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
    marker_removed: bool,
    locations: ArtifactUseLocations,
}

/// Shared checkout reservation for one synchronous managed-work scope.
#[derive(Debug)]
pub(crate) struct ArtifactUseScope {
    _lock: super::SharedFileLock,
}

impl ArtifactUseProducer {
    pub(crate) fn is_pre_dispatch(&self) -> bool {
        !self.marker_removed
            && self.marker.phase == ArtifactUsePhase::PreDispatch
            && self.marker.process.is_none()
    }

    pub(crate) fn is_unsettled_running(&self) -> bool {
        !self.marker_removed && self.marker.phase == ArtifactUsePhase::Running
    }

    /// Bind this invocation to the owned child and assigned group immediately
    /// before the supervisor dispatches its command. For inline execution,
    /// the child is the helper whose assigned group the command inherits; a
    /// gate uses the command child directly. If the start token cannot be
    /// read, the durable marker remains unknown to reconciliation after
    /// supervisor death.
    pub(crate) fn mark_process_started(
        &mut self,
        child: &std::process::Child,
        process_group: i32,
    ) -> io::Result<()> {
        if self.marker_removed
            || self.marker.phase != ArtifactUsePhase::PreDispatch
            || self.marker.process.is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-use producer already dispatched or its marker was settled",
            ));
        }
        self.marker.phase = ArtifactUsePhase::Running;
        self.marker.process = None;
        let pid = match i32::try_from(child.id()) {
            Ok(pid) if pid > 1 && process_group > 1 => pid,
            _ => {
                self.persist_marker()?;
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "artifact producer child or process group has an invalid identity",
                ));
            }
        };
        self.marker.process = Some(ProcessStartIdentity::capture(pid, process_group)?);
        self.persist_marker()
    }

    /// Clear a marker only when dispatch is known not to have occurred.
    pub(crate) fn finish_not_started(&mut self) -> io::Result<()> {
        if self.marker.phase != ArtifactUsePhase::PreDispatch || self.marker.process.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "cannot clear an artifact-use marker after command dispatch",
            ));
        }
        self.remove_marker()
    }

    /// Clear a running marker only after this owned child has exited and its
    /// recorded process group is observed absent. The group observation is
    /// performed here so callers cannot clear with a boolean attestation.
    pub(crate) fn finish_verified_quiescence(
        &mut self,
        child: &mut std::process::Child,
    ) -> io::Result<()> {
        if self.marker.phase != ArtifactUsePhase::Running || self.marker_removed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-use marker is not an unsettled running producer",
            ));
        }
        let process = self.marker.process.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact-use marker has no process identity; retain it as unknown",
            )
        })?;
        let child_pid = i32::try_from(child.id()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "supervised child PID cannot be represented in the artifact-use marker",
            )
        })?;
        if child_pid != process.pid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact-use marker PID does not match the owned child",
            ));
        }
        if child.try_wait()?.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "artifact-use command child is still running",
            ));
        }
        #[cfg(unix)]
        {
            if !crate::gates::gate_process_group_absent(process.process_group) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "artifact-use process group is present or could not be confirmed absent",
                ));
            }
            self.remove_marker()
        }
        #[cfg(not(unix))]
        {
            let _ = process.process_group;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "artifact-use process settlement is unavailable on this platform",
            ))
        }
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

    fn remove_marker(&mut self) -> io::Result<()> {
        if self.marker_removed {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "artifact-use marker was already removed",
            ));
        }
        self.locations.require_unchanged()?;
        super::verify_named_lock_file(&self.marker_path, &self.marker_file)?;
        fs::remove_file(&self.marker_path)?;
        self.marker_removed = true;
        sync_directory(&self.locations.marker_dir)
    }
}

/// An exclusive reservation returned only when the full durable marker
/// inventory was readable and empty. It is a synchronization primitive, not
/// cleanup authority.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub(crate) struct ArtifactUseReclaimer {
    _lock: super::ExclusiveFileLock,
}

/// Begin a protocol-aware producer lifetime for one canonical checkout.
/// Session IDs are evidence in the marker, not part of the lock key: distinct
/// broker sessions that name the same checkout contend on the same inode.
///
/// On non-Unix targets, `ProtocolUnavailable` is a permanent target-level
/// lane: callers may continue ordinary work only while every reclaimer remains
/// blocked. On Unix, an error means admission failed and the caller must not
/// dispatch the managed command.
pub(crate) fn begin_artifact_use(
    primary_root: &Path,
    checkout_root: &Path,
    session_id: i64,
    invocation_id: &str,
) -> io::Result<ArtifactUseAdmission> {
    begin_artifact_use_with_layout_wait(
        primary_root,
        checkout_root,
        session_id,
        invocation_id,
        ARTIFACT_LAYOUT_LOCK_WAIT,
    )
}

/// Begin a producer without waiting for another process to initialize the
/// marker layout. Gate admission calls this while holding the brief lifecycle
/// claim, so contention must refuse before dispatch instead of holding that
/// claim while polling.
pub(crate) fn begin_artifact_use_nonblocking(
    primary_root: &Path,
    checkout_root: &Path,
    session_id: i64,
    invocation_id: &str,
) -> io::Result<ArtifactUseAdmission> {
    begin_artifact_use_with_layout_wait(
        primary_root,
        checkout_root,
        session_id,
        invocation_id,
        Duration::ZERO,
    )
}

fn begin_artifact_use_with_layout_wait(
    primary_root: &Path,
    checkout_root: &Path,
    session_id: i64,
    invocation_id: &str,
    layout_wait: Duration,
) -> io::Result<ArtifactUseAdmission> {
    if session_id < 0 || invocation_id.is_empty() || invocation_id.len() > 256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact-use admission requires a nonnegative session ID and a bounded invocation ID",
        ));
    }
    if !cfg!(unix) {
        return Ok(ArtifactUseAdmission::ProtocolUnavailable);
    }
    let locations = ArtifactUseLocations::resolve(primary_root, checkout_root)?;
    if !locations.initialize_layout(layout_wait)? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "artifact-use layout is being initialized; retry producer admission",
        ));
    }
    let file = super::open_lock_file_nofollow(&locations.lock_path)?;
    let lock = super::SharedFileLock::try_acquire(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "checkout artifact use is reserved; retry producer admission",
        )
    })?;
    locations.require_unchanged()?;
    super::verify_named_lock_file(&locations.lock_path, &lock.file)?;

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
    Ok(ArtifactUseAdmission::Admitted(Box::new(
        ArtifactUseProducer {
            _lock: lock,
            marker_file,
            marker_path,
            marker,
            marker_removed: false,
            locations,
        },
    )))
}

/// Build a unique bounded identifier for one caller invocation. Session zero
/// represents a gate run without a broker session; it does not affect the
/// canonical-checkout lock key.
pub(crate) fn invocation_id(kind: &str, session_id: i64) -> String {
    format!("{kind}:{session_id}:{}", next_nonce())
}

/// Reserve a canonical checkout while the current broker process performs
/// synchronous managed work. Any dispatched child must use
/// `begin_artifact_use` separately so its durable marker survives broker
/// process death. The nonblocking shared-lock attempt keeps foreground work
/// bounded when a future reclaimer is active.
pub(crate) fn begin_artifact_use_scope(
    primary_root: &Path,
    checkout_root: &Path,
) -> io::Result<ArtifactUseScopeAdmission> {
    if !cfg!(unix) {
        return Ok(ArtifactUseScopeAdmission::ProtocolUnavailable);
    }
    let locations = ArtifactUseLocations::resolve(primary_root, checkout_root)?;
    if !locations.initialize_layout(ARTIFACT_LAYOUT_LOCK_WAIT)? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "artifact-use layout is being initialized; retry producer admission",
        ));
    }
    let file = super::open_lock_file_nofollow(&locations.lock_path)?;
    let lock = super::SharedFileLock::try_acquire(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "checkout artifact use is reserved; retry producer admission",
        )
    })?;
    locations.require_unchanged()?;
    super::verify_named_lock_file(&locations.lock_path, &lock.file)?;
    Ok(ArtifactUseScopeAdmission::Admitted(ArtifactUseScope {
        _lock: lock,
    }))
}

/// Make a bounded, nonblocking exclusive attempt for a future reclaimer. Any
/// marker, malformed entry, cap overflow, or I/O error must retain the
/// checkout; this function never deletes or repairs markers. Non-Unix
/// targets return `ProtocolUnavailable` permanently, regardless of inventory.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn try_artifact_use_reclaimer(
    primary_root: &Path,
    checkout_root: &Path,
) -> io::Result<ArtifactUseReclaimTry> {
    if !cfg!(unix) {
        return Ok(ArtifactUseReclaimTry::Blocked(
            ArtifactUseReclaimBlock::ProtocolUnavailable,
        ));
    }
    let locations = ArtifactUseLocations::resolve(primary_root, checkout_root)?;
    if !locations.initialize_layout(Duration::ZERO)? {
        return Ok(ArtifactUseReclaimTry::Blocked(
            ArtifactUseReclaimBlock::Contended,
        ));
    }
    let file = super::open_lock_file_nofollow(&locations.lock_path)?;
    let Some(lock) = super::ExclusiveFileLock::try_acquire(file)? else {
        return Ok(ArtifactUseReclaimTry::Blocked(
            ArtifactUseReclaimBlock::Contended,
        ));
    };
    locations.require_unchanged()?;
    super::verify_named_lock_file(&locations.lock_path, &lock.file)?;
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

#[cfg_attr(not(test), allow(dead_code))]
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

#[cfg_attr(not(test), allow(dead_code))]
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

#[cfg_attr(not(test), allow(dead_code))]
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
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "artifact-use admission requires durable directory sync",
        ))
    }
}

fn path_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
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
        match begin_artifact_use(primary, checkout, session, invocation).unwrap() {
            ArtifactUseAdmission::Admitted(producer) => *producer,
            ArtifactUseAdmission::ProtocolUnavailable => {
                panic!("Unix test filesystem should support directory sync")
            }
        }
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
            producer.marker.checkout_key,
            second_session.marker.checkout_key
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

    #[test]
    fn concurrent_first_producers_serialize_layout_initialization() {
        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let workers = (21..23)
            .map(|session_id| {
                let primary_root = primary.path().to_path_buf();
                let checkout_root = checkout.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let mut producer = begin(
                        &primary_root,
                        &checkout_root,
                        session_id,
                        &format!("test:{session_id}:layout-race"),
                    );
                    producer.finish_not_started().unwrap();
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn producer_admission_defers_while_exclusive_reclaimer_reservation_is_held() {
        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let reservation = match try_artifact_use_reclaimer(primary.path(), &checkout).unwrap() {
            ArtifactUseReclaimTry::Acquired(reservation) => reservation,
            blocked => {
                panic!("empty checkout should reserve after complete inventory: {blocked:?}")
            }
        };
        let error = begin_artifact_use(primary.path(), &checkout, 23, "test:23:contended")
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        drop(reservation);

        let mut producer = begin(primary.path(), &checkout, 24, "test:24:resumed");
        producer.finish_not_started().unwrap();
    }

    #[test]
    fn synchronous_scope_holds_checkout_admission_across_child_markers() {
        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let scope = match begin_artifact_use_scope(primary.path(), &checkout).unwrap() {
            ArtifactUseScopeAdmission::Admitted(scope) => scope,
            ArtifactUseScopeAdmission::ProtocolUnavailable => {
                panic!("Unix test filesystem should support directory sync")
            }
        };
        let mut child = begin(primary.path(), &checkout, 29, "prepare-step:29:nested");
        child.finish_not_started().unwrap();
        drop(child);

        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &checkout).unwrap(),
            ArtifactUseReclaimTry::Blocked(ArtifactUseReclaimBlock::Contended)
        ));
        drop(scope);
        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &checkout).unwrap(),
            ArtifactUseReclaimTry::Acquired(_)
        ));
    }

    #[test]
    fn process_binding_is_single_use_and_settlement_is_terminal() {
        use std::os::unix::process::CommandExt;

        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let mut producer = begin(primary.path(), &checkout, 25, "test:25:single-dispatch");

        let mut first_command = Command::new("/bin/cat");
        first_command
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let first_child = first_command.spawn().unwrap();
        let first_group = i32::try_from(first_child.id()).unwrap();
        let mut first = TestProcessGroupChild::new(first_child, first_group);
        assert!(first.child.try_wait().unwrap().is_none());

        producer
            .mark_process_started(&first.child, first_group)
            .unwrap();
        let original_bytes = fs::read(&producer.marker_path).unwrap();
        let original_process = producer.marker.process.clone().unwrap();

        assert!(
            producer
                .mark_process_started(&first.child, first_group)
                .is_err()
        );
        assert_eq!(fs::read(&producer.marker_path).unwrap(), original_bytes);

        let mut second_command = Command::new("/bin/cat");
        second_command
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let second_child = second_command.spawn().unwrap();
        let second_group = i32::try_from(second_child.id()).unwrap();
        let mut second = TestProcessGroupChild::new(second_child, second_group);
        assert!(second.child.try_wait().unwrap().is_none());
        assert!(
            producer
                .mark_process_started(&second.child, second_group)
                .is_err()
        );
        assert_eq!(fs::read(&producer.marker_path).unwrap(), original_bytes);
        assert_eq!(producer.marker.process.as_ref(), Some(&original_process));

        assert!(!second.kill_leader().unwrap().success());
        assert!(!first.kill_leader().unwrap().success());
        producer
            .finish_verified_quiescence(&mut first.child)
            .unwrap();
        assert!(!producer.marker_path.exists());

        assert!(
            producer
                .mark_process_started(&first.child, first_group)
                .is_err()
        );
        assert!(
            producer
                .finish_verified_quiescence(&mut first.child)
                .is_err()
        );
        assert!(producer.finish_not_started().is_err());
        assert!(!producer.marker_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn checkout_aliases_resolve_to_the_same_artifact_use_key() {
        use std::os::unix::fs::symlink;

        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let alias = primary.path().join("checkout-alias");
        symlink(&checkout, &alias).unwrap();

        let mut first = begin(primary.path(), &checkout, 18, "test:18:canonical");
        let mut second = begin(primary.path(), &alias, 19, "test:19:alias");
        assert_eq!(first.marker.checkout_key, second.marker.checkout_key);
        assert_eq!(first.marker.checkout_root, second.marker.checkout_root);
        first.finish_not_started().unwrap();
        second.finish_not_started().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn different_checkout_does_not_wait_and_unbound_fast_exit_clears_with_owned_child_proof() {
        let primary = tempfile::tempdir().unwrap();
        let first_checkout = checkout(primary.path(), "first");
        let second_checkout = checkout(primary.path(), "second");
        let mut producer = begin(primary.path(), &first_checkout, 12, "prepare:12:deps");
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("true");
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let process_group = i32::try_from(child.id()).unwrap();
        producer
            .mark_process_started(&child, process_group)
            .unwrap();
        let process = producer.marker.process.as_mut().unwrap();
        process.start_time = None;
        process.start_time_clock = "unavailable".into();
        producer.persist_marker().unwrap();
        let unrelated = try_artifact_use_reclaimer(primary.path(), &second_checkout).unwrap();
        assert!(matches!(unrelated, ArtifactUseReclaimTry::Acquired(_)));

        child.wait().unwrap();
        producer.finish_verified_quiescence(&mut child).unwrap();
        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &first_checkout).unwrap(),
            ArtifactUseReclaimTry::Blocked(ArtifactUseReclaimBlock::Contended)
        ));
        drop(producer);
        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &first_checkout).unwrap(),
            ArtifactUseReclaimTry::Acquired(_)
        ));
    }

    #[test]
    fn confirmed_not_started_removes_the_marker_without_poisoning_future_admission() {
        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        let mut producer = begin(primary.path(), &checkout, 13, "exec:13:unavailable");
        producer.finish_not_started().unwrap();
        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &checkout).unwrap(),
            ArtifactUseReclaimTry::Blocked(ArtifactUseReclaimBlock::Contended)
        ));
        drop(producer);
        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &checkout).unwrap(),
            ArtifactUseReclaimTry::Acquired(_)
        ));
    }

    #[cfg(unix)]
    struct TestProcessGroupChild {
        child: std::process::Child,
        process_group: i32,
        member: Option<ProcessStartIdentity>,
        leader_reaped: bool,
    }

    #[cfg(unix)]
    impl TestProcessGroupChild {
        fn new(child: std::process::Child, process_group: i32) -> Self {
            Self {
                child,
                process_group,
                member: None,
                leader_reaped: false,
            }
        }

        fn wait_for_ready_member(&mut self, timeout: Duration) -> io::Result<ProcessStartIdentity> {
            let line = read_test_child_handshake(
                self.child.stdout.as_mut().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "test child has no stdout pipe")
                })?,
                timeout,
            )?;
            let identity = parse_test_child_identity(&line)?;
            if identity.process_group != self.process_group {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "producer child handshake named a different process group",
                ));
            }
            if identity.start_time.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "test child process start identity could not be bound",
                ));
            }
            self.member = Some(identity.clone());
            Ok(identity)
        }

        fn kill_leader(&mut self) -> io::Result<std::process::ExitStatus> {
            self.child.kill()?;
            let status = wait_for_test_child(&mut self.child, Duration::from_secs(2))?;
            self.leader_reaped = true;
            Ok(status)
        }
    }

    #[cfg(unix)]
    fn read_test_child_handshake(
        stdout: &mut std::process::ChildStdout,
        timeout: Duration,
    ) -> io::Result<String> {
        use std::os::fd::AsRawFd;

        let deadline = Instant::now() + timeout;
        let mut buffered = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "producer child ready handshake timed out",
                ));
            }
            let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
            let mut descriptor = libc::pollfd {
                fd: stdout.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll reads one valid descriptor and writes readiness into
            // the local `descriptor`; it does not signal or modify the child.
            let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
            if ready == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "producer child ready handshake timed out",
                ));
            }
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(error);
            }

            let mut chunk = [0_u8; 256];
            match stdout.read(&mut chunk) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "producer child exited before its ready handshake",
                    ));
                }
                Ok(read) => {
                    buffered.extend_from_slice(&chunk[..read]);
                    while let Some(end) = buffered.iter().position(|byte| *byte == b'\n') {
                        let line = String::from_utf8_lossy(&buffered[..end]).trim().to_owned();
                        buffered.drain(..=end);
                        if line.starts_with("artifact-use-ready:") {
                            return Ok(line);
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }

    #[cfg(unix)]
    fn parse_test_child_identity(line: &str) -> io::Result<ProcessStartIdentity> {
        let payload = line.strip_prefix("artifact-use-ready:").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid producer child handshake",
            )
        })?;
        let mut parts = payload.split(':');
        let pid = parts
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing child PID"))?
            .parse::<i32>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let process_group = parts
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process group"))?
            .parse::<i32>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if parts.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected fields in producer child handshake",
            ));
        }
        ProcessStartIdentity::capture(pid, process_group)
    }

    #[cfg(unix)]
    impl Drop for TestProcessGroupChild {
        fn drop(&mut self) {
            if !self.leader_reaped {
                // SAFETY: the child is still unreaped, so its process-group ID
                // cannot have been released and reused.
                let _ = unsafe { libc::killpg(self.process_group, libc::SIGKILL) };
                let _ = self.child.kill();
                let _ = wait_for_test_child(&mut self.child, Duration::from_secs(2));
            } else if let Some(member) = self.member.as_ref() {
                let still_same_member =
                    ProcessStartIdentity::capture(member.pid, member.process_group)
                        .is_ok_and(|current| current == *member);
                if still_same_member {
                    // SAFETY: the captured start token still identifies the
                    // test-owned group member, so this group ID cannot name a
                    // replacement group yet.
                    let _ = unsafe { libc::killpg(self.process_group, libc::SIGKILL) };
                }
            }
        }
    }

    #[cfg(unix)]
    fn wait_for_test_child(
        child: &mut std::process::Child,
        timeout: Duration,
    ) -> io::Result<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "test child did not exit within its bounded cleanup window",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::zombie_processes,
        reason = "the bounded nested child intentionally outlives its killed producer; the parent verifies its marker and kills the owned group"
    )]
    fn process_death_leaves_the_durable_marker_for_reconciliation() {
        const CHILD: &str = "AETHYME_ARTIFACT_USE_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let primary =
                PathBuf::from(std::env::var_os("AETHYME_ARTIFACT_USE_TEST_PRIMARY").unwrap());
            let checkout =
                PathBuf::from(std::env::var_os("AETHYME_ARTIFACT_USE_TEST_CHECKOUT").unwrap());
            let mut producer = begin(&primary, &checkout, 15, "test:15:process-death");
            let mut command = Command::new("/bin/sleep");
            command.arg("30");
            let command_child = command.spawn().unwrap();
            // SAFETY: getpgrp takes no pointers and reads this process's group ID.
            let process_group = unsafe { libc::getpgrp() };
            let process =
                ProcessStartIdentity::capture(command_child.id() as i32, process_group).unwrap();
            producer
                .mark_process_started(&command_child, process_group)
                .unwrap();
            println!(
                "artifact-use-ready:{}:{}",
                process.pid, process.process_group
            );
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
        let child = command.spawn().unwrap();
        let process_group = i32::try_from(child.id()).unwrap();
        let mut guarded = TestProcessGroupChild::new(child, process_group);
        let child_member = guarded
            .wait_for_ready_member(Duration::from_secs(5))
            .unwrap();
        let status = guarded.kill_leader().unwrap();
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
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        assert!(process.start_time.is_some());
        assert_eq!(process, &child_member);
        assert_eq!(
            ProcessStartIdentity::capture(process.pid, process.process_group).unwrap(),
            child_member
        );
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
        let mut producer = begin(primary.path(), &checkout, 17, "exec:17:replacement");
        let target = primary.path().join("marker-target");
        fs::write(&target, b"preserve").unwrap();
        let displaced = producer.marker_path.with_extension("displaced");
        fs::rename(&producer.marker_path, &displaced).unwrap();
        symlink(&target, &producer.marker_path).unwrap();

        assert!(producer.finish_not_started().is_err());
        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &checkout).unwrap(),
            ArtifactUseReclaimTry::Blocked(ArtifactUseReclaimBlock::Contended)
        ));
        drop(producer);
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

#[cfg(all(test, not(unix)))]
mod unsupported_tests {
    use super::*;

    #[test]
    fn unsupported_platform_keeps_admission_and_cleanup_disabled() {
        let primary = tempfile::tempdir().unwrap();
        let checkout = primary.path().join("checkout");
        fs::create_dir(&checkout).unwrap();

        assert!(matches!(
            begin_artifact_use(primary.path(), &checkout, 31, "test:31:unsupported").unwrap(),
            ArtifactUseAdmission::ProtocolUnavailable
        ));
        assert!(matches!(
            begin_artifact_use_scope(primary.path(), &checkout).unwrap(),
            ArtifactUseScopeAdmission::ProtocolUnavailable
        ));
        assert!(matches!(
            try_artifact_use_reclaimer(primary.path(), &checkout).unwrap(),
            ArtifactUseReclaimTry::Blocked(ArtifactUseReclaimBlock::ProtocolUnavailable)
        ));
    }
}
