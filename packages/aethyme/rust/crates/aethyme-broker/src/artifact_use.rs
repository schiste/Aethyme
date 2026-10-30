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
// B1's fail-closed reclaimer allowances below stay item-scoped. The same
// inventory bound also caps explicit marker reconciliation.
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

/// One exact marker entry in an explicit, checkout-scoped reconciliation plan.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ArtifactUseReconcileMarker {
    pub(crate) filename: String,
    pub(crate) session_id: i64,
    pub(crate) invocation_id: String,
    pub(crate) bytes_sha256: String,
    pub(crate) file_device: u64,
    pub(crate) file_inode: u64,
    pub(crate) disposition: ArtifactUseReconcileDisposition,
}

/// A marker is removable only when both its PID identity and its recorded
/// process group were observed absent during the current plan/apply pass.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactUseReconcileDisposition {
    RemoveIfConfirmed,
    Retain { reason: String },
    Unknown { reason: String },
}

/// Complete reviewed evidence for one closed session's canonical checkout
/// cohort. This grants marker-removal authority only.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ArtifactUseReconcilePlan {
    pub(crate) schema_version: u32,
    pub(crate) selected_session_id: i64,
    pub(crate) cohort: crate::auto_cleanup::ClosedArtifactCheckoutCohort,
    pub(crate) primary_root: PathBuf,
    pub(crate) checkout_root: PathBuf,
    checkout_key: String,
    primary_identity: DirectoryIdentity,
    checkout_identity: DirectoryIdentity,
    marker_directory_identity: DirectoryIdentity,
    registry_witness: crate::worktree_registry::ArtifactUsePrimaryRootWitness,
    pub(crate) markers: Vec<ArtifactUseReconcileMarker>,
    pub(crate) digest: String,
}

/// Per-marker result from a digest-confirmed apply. An unconfirmed directory
/// sync means the unlink returned success but its crash durability is unknown.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ArtifactUseReconcileMarkerOutcome {
    pub(crate) filename: String,
    pub(crate) bytes_sha256: String,
    pub(crate) outcome: ArtifactUseReconcileOutcomeKind,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub(crate) enum ArtifactUseReconcileOutcomeKind {
    Removed {
        directory_sync: ArtifactUseReconcileDirectorySync,
    },
    Retained {
        reason: String,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactUseReconcileDirectorySync {
    Synced,
    Unconfirmed,
}

/// A stopped apply still returns every marker's outcome. The stop kind says
/// why later candidates were retained as unknown and whether an unlink had
/// already succeeded before the failure.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ArtifactUseReconcileApplyStop {
    pub(crate) kind: ArtifactUseReconcileApplyStopKind,
    pub(crate) marker_filename: String,
    pub(crate) reason: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactUseReconcileApplyStopKind {
    InventoryChanged,
    MarkerChanged,
    ProducerStillActive,
    ProcessProofUnknown,
    RegistryChanged,
    LayoutChanged,
    MarkerRemovalFailed,
    MarkerDirectorySyncFailed,
}

/// Result of applying one exact marker plan. A stopped result records safe
/// per-marker progress instead of hiding earlier unlinks behind an error.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ArtifactUseReconcileReport {
    pub(crate) plan_digest: String,
    pub(crate) outcomes: Vec<ArtifactUseReconcileMarkerOutcome>,
    pub(crate) stopped: Option<ArtifactUseReconcileApplyStop>,
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

/// Read-only plan for explicit reconciliation of one closed checkout cohort.
/// The normal reclaimer remains strict and continues to block on every marker.
pub(crate) fn plan_artifact_use_marker_reconciliation(
    primary_root: &Path,
    checkout_root: &Path,
    selected_session_id: i64,
    cohort: &crate::auto_cleanup::ClosedArtifactCheckoutCohort,
    registry: &crate::worktree_registry::ArtifactUsePrimaryRootWitnessGuard,
) -> io::Result<ArtifactUseReconcilePlan> {
    let claim = ArtifactUseReconcileClaim::acquire(primary_root, checkout_root)?;
    let (plan, _) = claim.plan(selected_session_id, cohort, registry)?;
    Ok(plan)
}

/// Apply one explicit plan only after rebuilding its complete digest under the
/// lifecycle, registry, layout, and checkout claims. Each candidate is
/// re-read and its PID/start token and group absence are rechecked immediately
/// before unlink. Later evidence or sync failures return typed per-marker
/// progress; no process is signalled and no checkout is removed.
pub(crate) fn apply_artifact_use_marker_reconciliation(
    primary_root: &Path,
    checkout_root: &Path,
    selected_session_id: i64,
    cohort: &crate::auto_cleanup::ClosedArtifactCheckoutCohort,
    registry: &crate::worktree_registry::ArtifactUsePrimaryRootWitnessGuard,
    confirmed_digest: &str,
) -> io::Result<ArtifactUseReconcileReport> {
    apply_artifact_use_marker_reconciliation_impl(
        primary_root,
        checkout_root,
        selected_session_id,
        cohort,
        registry,
        confirmed_digest,
        |_, _| {},
    )
}

#[cfg(test)]
pub(crate) fn apply_artifact_use_marker_reconciliation_with_after_unlink<F>(
    primary_root: &Path,
    checkout_root: &Path,
    selected_session_id: i64,
    cohort: &crate::auto_cleanup::ClosedArtifactCheckoutCohort,
    registry: &crate::worktree_registry::ArtifactUsePrimaryRootWitnessGuard,
    confirmed_digest: &str,
    after_unlink: F,
) -> io::Result<ArtifactUseReconcileReport>
where
    F: FnMut(&Path, &str),
{
    apply_artifact_use_marker_reconciliation_impl(
        primary_root,
        checkout_root,
        selected_session_id,
        cohort,
        registry,
        confirmed_digest,
        after_unlink,
    )
}

fn apply_artifact_use_marker_reconciliation_impl<F>(
    primary_root: &Path,
    checkout_root: &Path,
    selected_session_id: i64,
    cohort: &crate::auto_cleanup::ClosedArtifactCheckoutCohort,
    registry: &crate::worktree_registry::ArtifactUsePrimaryRootWitnessGuard,
    confirmed_digest: &str,
    mut after_unlink: F,
) -> io::Result<ArtifactUseReconcileReport>
where
    F: FnMut(&Path, &str),
{
    let claim = ArtifactUseReconcileClaim::acquire(primary_root, checkout_root)?;
    let (plan, records) = claim.plan(selected_session_id, cohort, registry)?;
    if plan.digest != confirmed_digest {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "artifact-use reconciliation plan changed; review digest {} before applying",
                plan.digest
            ),
        ));
    }

    let mut expected_remaining = records;
    let mut outcomes = plan
        .markers
        .iter()
        .map(initial_reconcile_outcome)
        .collect::<Vec<_>>();
    for (planned_index, planned) in plan.markers.iter().enumerate() {
        if !matches!(
            &planned.disposition,
            ArtifactUseReconcileDisposition::RemoveIfConfirmed
        ) {
            continue;
        }
        match attempt_reconcile_marker_removal(
            &claim,
            selected_session_id,
            cohort,
            registry,
            &expected_remaining,
            &planned.filename,
            &mut after_unlink,
        ) {
            Ok(()) => {
                if let Some(index) = expected_remaining
                    .iter()
                    .position(|record| record.filename == planned.filename)
                {
                    expected_remaining.remove(index);
                }
                outcomes[planned_index] = Some(ArtifactUseReconcileMarkerOutcome {
                    filename: planned.filename.clone(),
                    bytes_sha256: planned.bytes_sha256.clone(),
                    outcome: ArtifactUseReconcileOutcomeKind::Removed {
                        directory_sync: ArtifactUseReconcileDirectorySync::Synced,
                    },
                });
            }
            Err(ArtifactUseReconcileAttemptError::MarkerDirectorySyncFailed(reason)) => {
                outcomes[planned_index] = Some(ArtifactUseReconcileMarkerOutcome {
                    filename: planned.filename.clone(),
                    bytes_sha256: planned.bytes_sha256.clone(),
                    outcome: ArtifactUseReconcileOutcomeKind::Removed {
                        directory_sync: ArtifactUseReconcileDirectorySync::Unconfirmed,
                    },
                });
                mark_remaining_candidates_unknown(
                    &plan.markers,
                    &mut outcomes,
                    planned_index + 1,
                    "apply stopped after a marker-directory sync failure",
                );
                return Ok(ArtifactUseReconcileReport {
                    plan_digest: plan.digest,
                    outcomes: finish_reconcile_outcomes(outcomes),
                    stopped: Some(ArtifactUseReconcileApplyStop {
                        kind: ArtifactUseReconcileApplyStopKind::MarkerDirectorySyncFailed,
                        marker_filename: planned.filename.clone(),
                        reason,
                    }),
                });
            }
            Err(error) => {
                let (kind, outcome, reason) = error.into_stop_and_outcome();
                outcomes[planned_index] = Some(ArtifactUseReconcileMarkerOutcome {
                    filename: planned.filename.clone(),
                    bytes_sha256: planned.bytes_sha256.clone(),
                    outcome,
                });
                mark_remaining_candidates_unknown(
                    &plan.markers,
                    &mut outcomes,
                    planned_index + 1,
                    "apply stopped before this candidate was re-proven",
                );
                return Ok(ArtifactUseReconcileReport {
                    plan_digest: plan.digest,
                    outcomes: finish_reconcile_outcomes(outcomes),
                    stopped: Some(ArtifactUseReconcileApplyStop {
                        kind,
                        marker_filename: planned.filename.clone(),
                        reason,
                    }),
                });
            }
        }
    }

    Ok(ArtifactUseReconcileReport {
        plan_digest: plan.digest,
        outcomes: finish_reconcile_outcomes(outcomes),
        stopped: None,
    })
}

fn initial_reconcile_outcome(
    marker: &ArtifactUseReconcileMarker,
) -> Option<ArtifactUseReconcileMarkerOutcome> {
    let outcome = match &marker.disposition {
        ArtifactUseReconcileDisposition::RemoveIfConfirmed => return None,
        ArtifactUseReconcileDisposition::Retain { reason } => {
            ArtifactUseReconcileOutcomeKind::Retained {
                reason: reason.clone(),
            }
        }
        ArtifactUseReconcileDisposition::Unknown { reason } => {
            ArtifactUseReconcileOutcomeKind::Unknown {
                reason: reason.clone(),
            }
        }
    };
    Some(ArtifactUseReconcileMarkerOutcome {
        filename: marker.filename.clone(),
        bytes_sha256: marker.bytes_sha256.clone(),
        outcome,
    })
}

fn finish_reconcile_outcomes(
    outcomes: Vec<Option<ArtifactUseReconcileMarkerOutcome>>,
) -> Vec<ArtifactUseReconcileMarkerOutcome> {
    outcomes
        .into_iter()
        .map(|outcome| outcome.expect("every planned marker receives an apply outcome"))
        .collect()
}

fn mark_remaining_candidates_unknown(
    markers: &[ArtifactUseReconcileMarker],
    outcomes: &mut [Option<ArtifactUseReconcileMarkerOutcome>],
    start: usize,
    reason: &str,
) {
    for (marker, outcome) in markers.iter().zip(outcomes).skip(start) {
        if outcome.is_none() {
            *outcome = Some(ArtifactUseReconcileMarkerOutcome {
                filename: marker.filename.clone(),
                bytes_sha256: marker.bytes_sha256.clone(),
                outcome: ArtifactUseReconcileOutcomeKind::Unknown {
                    reason: reason.to_owned(),
                },
            });
        }
    }
}

#[derive(Debug)]
enum ArtifactUseReconcileAttemptError {
    InventoryChanged(String),
    MarkerChanged(String),
    ProducerStillActive(String),
    ProcessProofUnknown(String),
    RegistryChanged(String),
    LayoutChanged(String),
    MarkerRemovalFailed(String),
    MarkerDirectorySyncFailed(String),
}

impl ArtifactUseReconcileAttemptError {
    fn into_stop_and_outcome(
        self,
    ) -> (
        ArtifactUseReconcileApplyStopKind,
        ArtifactUseReconcileOutcomeKind,
        String,
    ) {
        match self {
            Self::InventoryChanged(reason) => (
                ArtifactUseReconcileApplyStopKind::InventoryChanged,
                ArtifactUseReconcileOutcomeKind::Unknown {
                    reason: reason.clone(),
                },
                reason,
            ),
            Self::MarkerChanged(reason) => (
                ArtifactUseReconcileApplyStopKind::MarkerChanged,
                ArtifactUseReconcileOutcomeKind::Unknown {
                    reason: reason.clone(),
                },
                reason,
            ),
            Self::ProducerStillActive(reason) => (
                ArtifactUseReconcileApplyStopKind::ProducerStillActive,
                ArtifactUseReconcileOutcomeKind::Retained {
                    reason: reason.clone(),
                },
                reason,
            ),
            Self::ProcessProofUnknown(reason) => (
                ArtifactUseReconcileApplyStopKind::ProcessProofUnknown,
                ArtifactUseReconcileOutcomeKind::Unknown {
                    reason: reason.clone(),
                },
                reason,
            ),
            Self::RegistryChanged(reason) => (
                ArtifactUseReconcileApplyStopKind::RegistryChanged,
                ArtifactUseReconcileOutcomeKind::Unknown {
                    reason: reason.clone(),
                },
                reason,
            ),
            Self::LayoutChanged(reason) => (
                ArtifactUseReconcileApplyStopKind::LayoutChanged,
                ArtifactUseReconcileOutcomeKind::Unknown {
                    reason: reason.clone(),
                },
                reason,
            ),
            Self::MarkerRemovalFailed(reason) => (
                ArtifactUseReconcileApplyStopKind::MarkerRemovalFailed,
                ArtifactUseReconcileOutcomeKind::Unknown {
                    reason: reason.clone(),
                },
                reason,
            ),
            Self::MarkerDirectorySyncFailed(reason) => (
                ArtifactUseReconcileApplyStopKind::MarkerDirectorySyncFailed,
                ArtifactUseReconcileOutcomeKind::Unknown {
                    reason: reason.clone(),
                },
                reason,
            ),
        }
    }
}

fn attempt_reconcile_marker_removal<F>(
    claim: &ArtifactUseReconcileClaim,
    selected_session_id: i64,
    cohort: &crate::auto_cleanup::ClosedArtifactCheckoutCohort,
    registry: &crate::worktree_registry::ArtifactUsePrimaryRootWitnessGuard,
    expected_remaining: &[ReconcileMarkerRecord],
    filename: &str,
    after_unlink: &mut F,
) -> Result<(), ArtifactUseReconcileAttemptError>
where
    F: FnMut(&Path, &str),
{
    claim
        .verify_inventory(expected_remaining)
        .map_err(|error| ArtifactUseReconcileAttemptError::InventoryChanged(error.to_string()))?;
    let record = expected_remaining
        .iter()
        .find(|record| record.filename == filename)
        .ok_or_else(|| {
            ArtifactUseReconcileAttemptError::InventoryChanged(
                "planned marker disappeared before removal".into(),
            )
        })?;
    let (current, mut current_file) =
        read_reconcile_marker_file_with_handle(&claim.locations.marker_dir, filename)
            .map_err(|error| ArtifactUseReconcileAttemptError::MarkerChanged(error.to_string()))?;
    if &current != record {
        return Err(ArtifactUseReconcileAttemptError::MarkerChanged(
            "artifact-use marker bytes or file identity changed before removal".into(),
        ));
    }
    match reconciliation_disposition(
        &claim.locations,
        selected_session_id,
        cohort,
        registry,
        &current,
    ) {
        ArtifactUseReconcileDisposition::RemoveIfConfirmed => {}
        ArtifactUseReconcileDisposition::Retain { reason } => {
            return Err(ArtifactUseReconcileAttemptError::ProducerStillActive(
                reason,
            ));
        }
        ArtifactUseReconcileDisposition::Unknown { reason } => {
            return Err(ArtifactUseReconcileAttemptError::ProcessProofUnknown(
                reason,
            ));
        }
    }
    registry
        .revalidate()
        .map_err(|error| ArtifactUseReconcileAttemptError::RegistryChanged(error.to_string()))?;
    claim
        .locations
        .require_existing_unchanged()
        .map_err(|error| ArtifactUseReconcileAttemptError::LayoutChanged(error.to_string()))?;
    super::verify_named_lock_file(&claim.locations.lock_path, &claim.checkout_lock_file)
        .map_err(|error| ArtifactUseReconcileAttemptError::LayoutChanged(error.to_string()))?;
    super::verify_named_lock_file(&claim.locations.layout_lock_path, &claim.layout_lock_file)
        .map_err(|error| ArtifactUseReconcileAttemptError::LayoutChanged(error.to_string()))?;
    let marker_path = claim.locations.marker_dir.join(filename);
    let before_recheck = current_file
        .metadata()
        .map_err(|error| ArtifactUseReconcileAttemptError::MarkerChanged(error.to_string()))?;
    if metadata_identity(&before_recheck)
        .map_err(|error| ArtifactUseReconcileAttemptError::MarkerChanged(error.to_string()))?
        != current.file_identity
    {
        return Err(ArtifactUseReconcileAttemptError::MarkerChanged(
            "artifact-use marker file identity changed before removal".into(),
        ));
    }
    current_file
        .seek(SeekFrom::Start(0))
        .map_err(|error| ArtifactUseReconcileAttemptError::MarkerChanged(error.to_string()))?;
    let mut rechecked_bytes = Vec::new();
    (&mut current_file)
        .take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut rechecked_bytes)
        .map_err(|error| ArtifactUseReconcileAttemptError::MarkerChanged(error.to_string()))?;
    super::verify_named_lock_file(&marker_path, &current_file)
        .map_err(|error| ArtifactUseReconcileAttemptError::MarkerChanged(error.to_string()))?;
    if rechecked_bytes != record.bytes
        || metadata_identity(
            &current_file.metadata().map_err(|error| {
                ArtifactUseReconcileAttemptError::MarkerChanged(error.to_string())
            })?,
        )
        .map_err(|error| ArtifactUseReconcileAttemptError::MarkerChanged(error.to_string()))?
            != current.file_identity
    {
        return Err(ArtifactUseReconcileAttemptError::MarkerChanged(
            "artifact-use marker bytes changed immediately before removal".into(),
        ));
    }
    fs::remove_file(&marker_path).map_err(|error| {
        ArtifactUseReconcileAttemptError::MarkerRemovalFailed(error.to_string())
    })?;
    after_unlink(&claim.locations.marker_dir, filename);
    sync_directory(&claim.locations.marker_dir).map_err(|error| {
        ArtifactUseReconcileAttemptError::MarkerDirectorySyncFailed(error.to_string())
    })?;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReconcileMarkerRecord {
    filename: String,
    marker: ArtifactUseMarker,
    bytes: Vec<u8>,
    bytes_sha256: String,
    file_identity: DirectoryIdentity,
}

struct ArtifactUseReconcileClaim {
    locations: ArtifactUseLocations,
    marker_directory_identity: DirectoryIdentity,
    layout_lock_file: File,
    checkout_lock_file: File,
    _layout_lock: super::ExclusiveFileLock,
    _checkout_lock: super::ExclusiveFileLock,
}

impl ArtifactUseReconcileClaim {
    fn acquire(primary_root: &Path, checkout_root: &Path) -> io::Result<Self> {
        if !cfg!(unix) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "artifact-use marker reconciliation is unavailable on this platform",
            ));
        }
        let locations = resolve_existing_locations(primary_root, checkout_root)?;
        let layout_lock_file = open_existing_lock_readwrite(&locations.layout_lock_path)?;
        let layout_lock =
            super::ExclusiveFileLock::try_acquire(layout_lock_file)?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::WouldBlock, "artifact-use layout is busy")
            })?;
        super::verify_named_lock_file(&locations.layout_lock_path, &layout_lock.file)?;
        locations.require_existing_unchanged()?;

        let checkout_lock_file = open_existing_lock_readwrite(&locations.lock_path)?;
        let checkout_lock =
            super::ExclusiveFileLock::try_acquire(checkout_lock_file)?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::WouldBlock, "artifact-use checkout is busy")
            })?;
        super::verify_named_lock_file(&locations.lock_path, &checkout_lock.file)?;
        locations.require_existing_unchanged()?;

        let (marker_directory_identity, _) = read_reconcile_marker_inventory(&locations)?;
        Ok(Self {
            locations,
            marker_directory_identity,
            layout_lock_file: layout_lock.file.try_clone()?,
            checkout_lock_file: checkout_lock.file.try_clone()?,
            _layout_lock: layout_lock,
            _checkout_lock: checkout_lock,
        })
    }

    fn plan(
        &self,
        selected_session_id: i64,
        cohort: &crate::auto_cleanup::ClosedArtifactCheckoutCohort,
        registry: &crate::worktree_registry::ArtifactUsePrimaryRootWitnessGuard,
    ) -> io::Result<(ArtifactUseReconcilePlan, Vec<ReconcileMarkerRecord>)> {
        self.locations.require_existing_unchanged()?;
        if cohort.worktree() != self.locations.checkout_root.as_path()
            || !cohort.session_ids().contains(&selected_session_id)
        {
            return Err(invalid_marker(
                "selected session is not in the exact closed checkout cohort",
            ));
        }
        if !registry.matches_marker_identity(
            &self.locations.primary_root,
            self.locations.primary_identity.device,
            self.locations.primary_identity.inode,
        ) {
            return Err(invalid_marker(
                "validated root registry witness does not bind the current primary root",
            ));
        }
        registry.revalidate()?;
        let (marker_directory_identity, records) =
            read_reconcile_marker_inventory(&self.locations)?;
        if marker_directory_identity != self.marker_directory_identity {
            return Err(invalid_marker(
                "artifact-use marker directory changed during reconciliation",
            ));
        }
        let mut markers = records
            .iter()
            .map(|record| ArtifactUseReconcileMarker {
                filename: record.filename.clone(),
                session_id: record.marker.session_id,
                invocation_id: record.marker.invocation_id.clone(),
                bytes_sha256: record.bytes_sha256.clone(),
                file_device: record.file_identity.device,
                file_inode: record.file_identity.inode,
                disposition: reconciliation_disposition(
                    &self.locations,
                    selected_session_id,
                    cohort,
                    registry,
                    record,
                ),
            })
            .collect::<Vec<_>>();
        markers.sort_by(|left, right| left.filename.cmp(&right.filename));
        let mut plan = ArtifactUseReconcilePlan {
            schema_version: 1,
            selected_session_id,
            cohort: cohort.clone(),
            primary_root: self.locations.primary_root.clone(),
            checkout_root: self.locations.checkout_root.clone(),
            checkout_key: self.locations.checkout_key.clone(),
            primary_identity: self.locations.primary_identity.clone(),
            checkout_identity: self.locations.checkout_identity.clone(),
            marker_directory_identity: marker_directory_identity.clone(),
            registry_witness: registry.witness().clone(),
            markers,
            digest: String::new(),
        };
        let serialized = serde_json::to_vec(&plan)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        plan.digest = format!("{:x}", Sha256::digest(serialized));
        Ok((plan, records))
    }

    fn verify_inventory(&self, expected: &[ReconcileMarkerRecord]) -> io::Result<()> {
        self.locations.require_existing_unchanged()?;
        let (directory_identity, current) = read_reconcile_marker_inventory(&self.locations)?;
        if directory_identity != self.marker_directory_identity || current != expected {
            return Err(invalid_marker(
                "artifact-use marker inventory changed during apply",
            ));
        }
        Ok(())
    }
}

fn resolve_existing_locations(
    primary_root: &Path,
    checkout_root: &Path,
) -> io::Result<ArtifactUseLocations> {
    let primary_root = primary_root.canonicalize()?;
    let checkout_root = checkout_root.canonicalize()?;
    let primary_identity = directory_identity(&primary_root)?;
    let checkout_identity = directory_identity(&checkout_root)?;
    let checkout_key = checkout_key(&checkout_root)?;
    let state_dir = primary_root.join(".aethyme");
    let run_dir = state_dir.join("run");
    let runtime_dir = run_dir.join("artifact-use");
    for (path, label) in [
        (&state_dir, "repository state"),
        (&run_dir, "repository runtime"),
        (&runtime_dir, "artifact-use runtime"),
    ] {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_dir() {
            return Err(invalid_marker(format!(
                "{label} is not an existing real directory: {}",
                path.display()
            )));
        }
        if path.canonicalize()? != *path {
            return Err(invalid_marker(format!(
                "{label} path changed or contains a symlink: {}",
                path.display()
            )));
        }
    }
    let marker_dir = runtime_dir.join(format!("{checkout_key}.markers"));
    let marker_metadata = fs::symlink_metadata(&marker_dir)?;
    if !marker_metadata.file_type().is_dir()
        || marker_dir.canonicalize()? != marker_dir
        || !marker_dir.starts_with(&runtime_dir)
    {
        return Err(invalid_marker(
            "artifact-use marker path is not an existing real child of the runtime directory",
        ));
    }
    Ok(ArtifactUseLocations {
        primary_root,
        primary_identity,
        checkout_root,
        checkout_identity,
        checkout_key,
        runtime_dir: runtime_dir.clone(),
        layout_lock_path: runtime_dir.join(format!("{checkout_key}.init.lock")),
        lock_path: runtime_dir.join(format!("{checkout_key}.lock")),
        marker_dir,
    })
}

impl ArtifactUseLocations {
    fn require_existing_unchanged(&self) -> io::Result<()> {
        let current = resolve_existing_locations(&self.primary_root, &self.checkout_root)?;
        if current != *self {
            return Err(invalid_marker(
                "artifact-use primary, checkout, or marker layout changed while claims were held",
            ));
        }
        Ok(())
    }
}

fn open_existing_lock_readwrite(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    configure_nofollow(&mut options);
    let file = options.open(path)?;
    super::verify_named_lock_file(path, &file)?;
    Ok(file)
}

fn read_reconcile_marker_inventory(
    locations: &ArtifactUseLocations,
) -> io::Result<(DirectoryIdentity, Vec<ReconcileMarkerRecord>)> {
    let directory_metadata = fs::symlink_metadata(&locations.marker_dir)?;
    if !directory_metadata.file_type().is_dir()
        || locations.marker_dir.canonicalize()? != locations.marker_dir
    {
        return Err(invalid_marker(
            "artifact-use marker directory is not a real directory",
        ));
    }
    let directory_identity = metadata_identity(&directory_metadata)?;
    let mut names = Vec::new();
    for entry in fs::read_dir(&locations.marker_dir)? {
        let entry = entry?;
        if names.len() >= MAX_MARKERS_PER_CHECKOUT {
            return Err(invalid_marker(
                "artifact-use marker inventory exceeded its cap; retain the checkout",
            ));
        }
        let name = entry.file_name().into_string().map_err(|_| {
            invalid_marker("artifact-use marker inventory contains a non-UTF-8 name")
        })?;
        if !name.ends_with(".json") {
            return Err(invalid_marker(format!(
                "unexpected artifact-use marker entry {name:?}"
            )));
        }
        names.push(name);
    }
    names.sort_unstable();
    let mut records = Vec::with_capacity(names.len());
    for name in names {
        records.push(read_reconcile_marker_file(&locations.marker_dir, &name)?);
    }
    let after = fs::symlink_metadata(&locations.marker_dir)?;
    if metadata_identity(&after)? != directory_identity {
        return Err(invalid_marker(
            "artifact-use marker directory changed while it was inventoried",
        ));
    }
    Ok((directory_identity, records))
}

fn read_reconcile_marker_file(
    marker_dir: &Path,
    filename: &str,
) -> io::Result<ReconcileMarkerRecord> {
    read_reconcile_marker_file_with_handle(marker_dir, filename).map(|(record, _file)| record)
}

fn read_reconcile_marker_file_with_handle(
    marker_dir: &Path,
    filename: &str,
) -> io::Result<(ReconcileMarkerRecord, File)> {
    let nonce = filename
        .strip_suffix(".json")
        .ok_or_else(|| invalid_marker("artifact-use marker has an invalid filename"))?;
    if nonce.is_empty() || nonce.contains('/') || nonce.contains('\\') {
        return Err(invalid_marker(
            "artifact-use marker has an invalid nonce filename",
        ));
    }
    let path = marker_dir.join(filename);
    let mut file = open_existing_nofollow(&path)?;
    super::verify_named_lock_file(&path, &file)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > MAX_MARKER_BYTES {
        return Err(invalid_marker(
            "artifact-use marker is not a bounded regular file",
        ));
    }
    let before_identity = metadata_identity(&before)?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    super::verify_named_lock_file(&path, &file)?;
    if before.len() != after.len()
        || bytes.len() as u64 != after.len()
        || metadata_identity(&after)? != before_identity
    {
        return Err(invalid_marker(
            "artifact-use marker changed while it was read",
        ));
    }
    let marker = serde_json::from_slice(&bytes)
        .map_err(|error| invalid_marker(format!("malformed artifact-use marker: {error}")))?;
    Ok((
        ReconcileMarkerRecord {
            filename: filename.to_owned(),
            marker,
            bytes_sha256: format!("{:x}", Sha256::digest(&bytes)),
            bytes,
            file_identity: before_identity,
        },
        file,
    ))
}

fn reconciliation_disposition(
    locations: &ArtifactUseLocations,
    selected_session_id: i64,
    cohort: &crate::auto_cleanup::ClosedArtifactCheckoutCohort,
    registry: &crate::worktree_registry::ArtifactUsePrimaryRootWitnessGuard,
    record: &ReconcileMarkerRecord,
) -> ArtifactUseReconcileDisposition {
    let marker = &record.marker;
    let retain = |reason: &str| ArtifactUseReconcileDisposition::Retain {
        reason: reason.to_owned(),
    };
    let unknown = |reason: &str| ArtifactUseReconcileDisposition::Unknown {
        reason: reason.to_owned(),
    };
    let primary_path = match path_text(&locations.primary_root) {
        Ok(path) => path,
        Err(_) => return retain("primary root path is not representable as UTF-8"),
    };
    let checkout_path = match path_text(&locations.checkout_root) {
        Ok(path) => path,
        Err(_) => return retain("checkout path is not representable as UTF-8"),
    };
    if marker.schema_version != MARKER_SCHEMA_VERSION
        || marker.primary_root != primary_path
        || marker.checkout_root != checkout_path
        || marker.checkout_device != locations.checkout_identity.device
        || marker.checkout_inode != locations.checkout_identity.inode
        || marker.checkout_key != locations.checkout_key
        || marker.session_id < 0
        || marker.invocation_id.is_empty()
        || marker.nonce.is_empty()
    {
        return retain("marker does not match the exact primary and checkout identities");
    }
    if record.filename != format!("{}.json", marker.nonce) {
        return retain("marker nonce does not match its filename");
    }
    let session_ids = cohort.session_ids();
    if !session_ids.contains(&marker.session_id) || !session_ids.contains(&selected_session_id) {
        return retain("marker session is not in the exact closed checkout cohort");
    }
    if !registry.matches_marker_identity(
        Path::new(&marker.primary_root),
        marker.primary_device,
        marker.primary_inode,
    ) {
        return retain("primary device identity has no validated registry witness");
    }
    if marker.phase != ArtifactUsePhase::Running {
        return retain("pre-dispatch marker has no process absence proof");
    }
    let Some(process) = marker.process.as_ref() else {
        return unknown("running marker has no process identity");
    };
    if let Err(reason) = validate_reconciliation_process_identity(process) {
        return unknown(reason);
    }
    #[cfg(unix)]
    {
        match observe_process_identity(process) {
            ProcessIdentityObservation::Same => {
                return retain("recorded producer PID and start token are still present");
            }
            ProcessIdentityObservation::Unknown => {
                return unknown("producer PID/start identity could not be inspected");
            }
            ProcessIdentityObservation::Absent | ProcessIdentityObservation::Reused => {}
        }
        let process_group_absent = crate::gates::gate_process_group_absent(process.process_group);
        if !process_group_absent {
            return unknown("recorded process group is present or could not be proven absent");
        }
        let final_identity = observe_process_identity(process);
        if process_and_group_proof_allows_removal(final_identity, process_group_absent) {
            ArtifactUseReconcileDisposition::RemoveIfConfirmed
        } else {
            match final_identity {
                ProcessIdentityObservation::Same => {
                    retain("recorded producer PID/start token reappeared during proof")
                }
                ProcessIdentityObservation::Unknown => {
                    unknown("producer PID/start identity became unknown during proof")
                }
                ProcessIdentityObservation::Absent | ProcessIdentityObservation::Reused => {
                    unreachable!("absent/reused identities with an absent group are removable")
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        unknown("process absence proof is unavailable on this platform")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessIdentityObservation {
    Absent,
    Same,
    Reused,
    Unknown,
}

fn process_and_group_proof_allows_removal(
    process: ProcessIdentityObservation,
    process_group_absent: bool,
) -> bool {
    process_group_absent
        && matches!(
            process,
            ProcessIdentityObservation::Absent | ProcessIdentityObservation::Reused
        )
}

fn validate_reconciliation_process_identity(
    process: &ProcessStartIdentity,
) -> Result<(), &'static str> {
    if process.pid <= 1 || process.process_group <= 1 {
        return Err("recorded PID or process group is invalid");
    }
    if process.start_time.is_none() {
        return Err("recorded process start token is unavailable");
    }
    let expected_clock = if cfg!(target_os = "macos") {
        Some("macos_epoch_microseconds")
    } else if cfg!(target_os = "linux") {
        Some("linux_clock_ticks")
    } else {
        None
    };
    if expected_clock != Some(process.start_time_clock.as_str()) {
        return Err("recorded process start-token clock is unsupported");
    }
    Ok(())
}

#[cfg(unix)]
fn observe_process_identity(process: &ProcessStartIdentity) -> ProcessIdentityObservation {
    // Callers validate PID and clock before reaching this non-signalling probe.
    // SAFETY: signal zero observes the process id and sends no signal.
    let result = unsafe { libc::kill(process.pid, 0) };
    if result != 0 {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => return ProcessIdentityObservation::Absent,
            Some(libc::EPERM) => {}
            _ => return ProcessIdentityObservation::Unknown,
        }
    }
    classify_process_start_observation(
        process.start_time,
        &process.start_time_clock,
        process_start_token(process.pid),
    )
}

fn classify_process_start_observation(
    expected_start_time: Option<u64>,
    expected_clock: &str,
    observed: io::Result<(Option<u64>, &'static str)>,
) -> ProcessIdentityObservation {
    match (expected_start_time, observed) {
        (Some(expected), Ok((Some(start_time), clock))) if clock == expected_clock => {
            if start_time == expected {
                ProcessIdentityObservation::Same
            } else {
                ProcessIdentityObservation::Reused
            }
        }
        _ => ProcessIdentityObservation::Unknown,
    }
}

fn metadata_identity(
    #[cfg(any(unix, windows))] metadata: &fs::Metadata,
    #[cfg(not(any(unix, windows)))] _metadata: &fs::Metadata,
) -> io::Result<DirectoryIdentity> {
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
                "artifact-use volume identity is unavailable",
            )
        })?;
        let inode = metadata.file_index().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "artifact-use file identity is unavailable",
            )
        })?;
        Ok(DirectoryIdentity {
            device: u64::from(device),
            inode,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "artifact-use file identity is unavailable",
        ))
    }
}

fn invalid_marker(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
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

    #[test]
    fn reconciliation_plan_refuses_missing_layout_without_creating_it() {
        let primary = tempfile::tempdir().unwrap();
        let checkout = checkout(primary.path(), "checkout");
        assert!(resolve_existing_locations(primary.path(), &checkout).is_err());
        assert!(!primary.path().join(".aethyme").exists());
    }

    #[test]
    fn reconciliation_process_identity_rejects_invalid_or_unavailable_fields() {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return;
        }
        let valid_clock = if cfg!(target_os = "macos") {
            "macos_epoch_microseconds"
        } else {
            "linux_clock_ticks"
        };
        let mut process = ProcessStartIdentity {
            pid: 42,
            process_group: 42,
            start_time: Some(123),
            start_time_clock: valid_clock.into(),
        };
        assert!(validate_reconciliation_process_identity(&process).is_ok());
        process.process_group = 0;
        assert!(validate_reconciliation_process_identity(&process).is_err());
        process.process_group = 42;
        process.start_time = None;
        assert!(validate_reconciliation_process_identity(&process).is_err());
        process.start_time = Some(123);
        process.start_time_clock = "unavailable".into();
        assert!(validate_reconciliation_process_identity(&process).is_err());
    }

    #[test]
    fn reconciliation_start_token_and_group_proof_fail_closed() {
        let clock = "linux_clock_ticks";
        assert_eq!(
            classify_process_start_observation(Some(123), clock, Ok((Some(123), clock))),
            ProcessIdentityObservation::Same
        );
        assert_eq!(
            classify_process_start_observation(Some(123), clock, Ok((Some(124), clock))),
            ProcessIdentityObservation::Reused
        );
        assert_eq!(
            classify_process_start_observation(Some(123), clock, Ok((None, clock))),
            ProcessIdentityObservation::Unknown
        );
        assert_eq!(
            classify_process_start_observation(
                Some(123),
                clock,
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
            ),
            ProcessIdentityObservation::Unknown
        );
        assert!(process_and_group_proof_allows_removal(
            ProcessIdentityObservation::Absent,
            true
        ));
        assert!(process_and_group_proof_allows_removal(
            ProcessIdentityObservation::Reused,
            true
        ));
        assert!(!process_and_group_proof_allows_removal(
            ProcessIdentityObservation::Same,
            true
        ));
        assert!(!process_and_group_proof_allows_removal(
            ProcessIdentityObservation::Unknown,
            true
        ));
        assert!(!process_and_group_proof_allows_removal(
            ProcessIdentityObservation::Absent,
            false
        ));
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
        let mut command = Command::new("/bin/true");
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
