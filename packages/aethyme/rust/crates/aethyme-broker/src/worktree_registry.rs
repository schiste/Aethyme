//! Durable, bounded discovery of exact worktree roots, independent of placement.
//!
//! This database shares the host-operation coordination domain. Explicit private
//! host-operation database overrides also get a private registry. Records are
//! hints, never deletion authority; legacy roots without saved volume UUIDs
//! remain blocked when their old mount-scoped identity no longer matches.

use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::WorktreeRootSource;
use crate::broker::{WORKTREE_ROOT_MARKER, WORKTREE_ROOT_SCHEMA_VERSION, WorktreeRootMarker};

pub(crate) const MAX_RECORD_BYTES: usize = 8192;
pub(crate) const MAX_RECORDS: usize = 8192;
const PAGE_RECORDS: usize = 64;
const MAX_INVENTORY_BYTES: usize = 8 * 1024 * 1024;
const REGISTRATION_LOCK_WAIT: Duration = Duration::from_secs(5);
const REGISTRATION_LOCK_RETRY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RootRecord {
    schema_version: u32,
    pub(crate) root: PathBuf,
    pub(crate) repository_key: String,
    pub(crate) repository_root: PathBuf,
    source: WorktreeRootSource,
    identity: Option<DirectoryIdentity>,
    origin_identity: Option<DirectoryIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    filesystem: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    volume_uuid: Option<[u8; 16]>,
}

impl DirectoryIdentity {
    /// Stable registry identity uses the volume UUID and inode when both
    /// observations carry a UUID. A legacy record can upgrade in the normal
    /// locked admission path only when its saved mount-scoped fields and inode
    /// still match the live observation exactly.
    fn same_registered_object(&self, current: &Self) -> bool {
        if self.inode != current.inode {
            return false;
        }
        match (self.volume_uuid, current.volume_uuid) {
            (Some(saved), Some(actual)) => saved == actual,
            (None, _) => self.device == current.device && self.filesystem == current.filesystem,
            _ => false,
        }
    }

    /// A missing path can be tied to a mounted volume through its existing
    /// ancestor, so compare only the volume-level identity here.
    fn same_registered_filesystem(&self, current: &Self) -> bool {
        match (self.volume_uuid, current.volume_uuid) {
            (Some(saved), Some(actual)) => saved == actual,
            (None, _) => self.device == current.device && self.filesystem == current.filesystem,
            _ => false,
        }
    }
}

fn same_optional_registered_object(
    saved: Option<&DirectoryIdentity>,
    current: Option<&DirectoryIdentity>,
) -> bool {
    match (saved, current) {
        (Some(saved), Some(current)) => saved.same_registered_object(current),
        (None, None) => true,
        _ => false,
    }
}

fn legacy_uuid_recovery_needed(
    saved: Option<&DirectoryIdentity>,
    current: Option<&DirectoryIdentity>,
) -> bool {
    matches!((saved, current), (Some(saved), Some(current))
        if saved.volume_uuid.is_none()
            && current.volume_uuid.is_some()
            && (saved.device != current.device || saved.filesystem != current.filesystem))
}

fn changed_identity_reason(
    object: &str,
    saved: Option<&DirectoryIdentity>,
    current: Option<&DirectoryIdentity>,
) -> String {
    if legacy_uuid_recovery_needed(saved, current)
        && matches!((saved, current), (Some(saved), Some(current)) if saved.inode == current.inode)
    {
        format!(
            "registered {object} has a legacy identity without a saved volume UUID; reviewed identity recovery is required"
        )
    } else {
        format!("registered {object} object or filesystem changed")
    }
}

// Apple's `struct attrlist` layout is documented in <sys/attr.h> and
// `man 2 getattrlist` (bitmap count/reserved, then five attribute groups).
#[cfg(target_os = "macos")]
#[repr(C)]
struct MacAttributeList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn getattrlist(
        path: *const libc::c_char,
        attr_list: *mut MacAttributeList,
        attr_buffer: *mut libc::c_void,
        attr_buffer_size: libc::size_t,
        options: libc::c_ulong,
    ) -> libc::c_int;
}

#[cfg(any(target_os = "macos", test))]
fn decode_volume_uuid_response(buffer: &[u8]) -> io::Result<[u8; 16]> {
    if buffer.len() < 4 {
        return Err(invalid("volume UUID response is truncated"));
    }
    let length = u32::from_ne_bytes(
        buffer[..4]
            .try_into()
            .map_err(|_| invalid("volume UUID response is truncated"))?,
    );
    if length != buffer.len() as u32 || length != 20 {
        return Err(invalid("volume UUID response has an unexpected size"));
    }
    let uuid: [u8; 16] = buffer[4..]
        .try_into()
        .map_err(|_| invalid("volume UUID response is truncated"))?;
    if uuid.iter().all(|byte| *byte == 0) {
        return Err(invalid("filesystem returned a nil volume UUID"));
    }
    Ok(uuid)
}

#[cfg(target_os = "macos")]
fn macos_volume_uuid(path: *const libc::c_char) -> io::Result<[u8; 16]> {
    // Values are from Apple's sys/attr.h: ATTR_BIT_MAP_COUNT, ATTR_VOL_INFO,
    // ATTR_VOL_UUID, and FSOPT_NOFOLLOW. The SDK defines ATTR_VOL_UUID as a
    // uuid_t containing the filesystem UUID; getattrlist requires ATTR_VOL_INFO.
    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_VOL_INFO: u32 = 0x8000_0000;
    const ATTR_VOL_UUID: u32 = 0x0004_0000;
    const FSOPT_NOFOLLOW: libc::c_ulong = 0x0000_0001;

    let mut attrs = MacAttributeList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: ATTR_VOL_INFO | ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buffer = [0_u8; 20];
    // SAFETY: path is a live NUL-terminated path, attrs follows the SDK's
    // attrlist layout, and buffer has room for the length plus one uuid_t.
    if unsafe {
        getattrlist(
            path,
            &mut attrs,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            FSOPT_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    decode_volume_uuid_response(&buffer)
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn directory_identity(path: &Path, metadata: &Metadata) -> io::Result<Option<DirectoryIdentity>> {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(invalid)?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: path is NUL terminated and stat points to writable storage of the
    // exact type statfs requires. Read it only after a successful call.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful statfs initialized the structure (zeroed above too).
    let stat = unsafe { stat.assume_init() };
    // fsid_t exposes no portable field accessor. Retain its raw bytes for
    // legacy records and pin-time checks; macOS registry identity uses the
    // filesystem UUID below because f_fsid and st_dev can change on remount.
    // SAFETY: the slice covers only the initialized f_fsid field's bytes.
    let filesystem = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!(stat.f_fsid).cast::<u8>(),
            std::mem::size_of_val(&stat.f_fsid),
        )
    }
    .to_vec();
    #[cfg(target_os = "macos")]
    let volume_uuid = Some(macos_volume_uuid(path.as_ptr())?);
    #[cfg(target_os = "linux")]
    let volume_uuid = None;
    Ok(Some(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        filesystem,
        volume_uuid,
    }))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn directory_identity(_path: &Path, _metadata: &Metadata) -> io::Result<Option<DirectoryIdentity>> {
    Ok(None)
}

fn real_directory(path: &Path) -> io::Result<Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid(format!(
            "{} is not a real directory",
            path.display()
        )));
    }
    Ok(metadata)
}

fn registry_directory(host_operation_database: &Path) -> io::Result<PathBuf> {
    let parent = host_operation_database
        .parent()
        .ok_or_else(|| invalid("host operation database has no parent"))?;
    let name = host_operation_database
        .file_name()
        .ok_or_else(|| invalid("host operation database has no filename"))?;
    Ok(parent.join(format!(
        "worktree-roots-{:x}",
        Sha256::digest(name.as_encoded_bytes())
    )))
}

pub(crate) fn registry_path(host_operation_database: &Path) -> io::Result<PathBuf> {
    Ok(registry_directory(host_operation_database)?.join("registry.sqlite3"))
}

const MAX_RECOVERY_REGISTRY_BYTES: u64 = 8192 * 65_536;
const RECOVERY_LOCK_NAME: &str = "registry.lock";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FreshRootRegistryState {
    MissingDatabase,
    ExistingNoTargetRow,
    ExactTargetRow,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RecoveryArtifactIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) owner: u32,
    pub(crate) mode: u32,
    pub(crate) links: u64,
    pub(crate) bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct FreshRootRegistryObservation {
    pub(crate) host_operation_database: PathBuf,
    pub(crate) registry_directory: PathBuf,
    pub(crate) database_path: PathBuf,
    pub(crate) lock_path: PathBuf,
    pub(crate) directory_identity: Option<RecoveryArtifactIdentity>,
    pub(crate) database_identity: Option<RecoveryArtifactIdentity>,
    pub(crate) lock_identity: Option<RecoveryArtifactIdentity>,
    pub(crate) directory_entries: Vec<String>,
    pub(crate) state: FreshRootRegistryState,
    pub(crate) schema_version: Option<u32>,
    pub(crate) journal_mode: Option<String>,
    pub(crate) integrity_check: Option<String>,
    pub(crate) row_count: Option<usize>,
    pub(crate) target_payload: Option<String>,
    pub(crate) prune_cursor: Option<String>,
}

/// A recovery guard keeps an already-existing registry lock through the
/// root/marker/row sequence. When the reviewed state has no lock, the guard
/// deliberately stays lockless until the root exists, then creates exactly
/// the missing lock with create-new semantics.
pub(crate) struct FreshRootRegistryRecoveryGuard {
    host_operation_database: PathBuf,
    root: PathBuf,
    expected_marker: WorktreeRootMarker,
    expected_source: WorktreeRootSource,
    exclusion: Option<crate::file_lock::ExclusiveFileLock>,
    observation: FreshRootRegistryObservation,
}

impl FreshRootRegistryRecoveryGuard {
    pub(crate) fn observation(&self) -> &FreshRootRegistryObservation {
        &self.observation
    }

    pub(crate) fn ensure_initialized_after_root(&mut self) -> io::Result<()> {
        if self.observation.state != FreshRootRegistryState::MissingDatabase {
            return Ok(());
        }
        if self.exclusion.is_none() {
            self.create_or_acquire_recovery_lock()?;
        }
        let latest = observe_fresh_root_registry_locked(
            &self.host_operation_database,
            &self.root,
            &self.expected_marker,
            self.expected_source,
        )?;
        match latest.state {
            FreshRootRegistryState::MissingDatabase => {
                initialize_fresh_recovery_registry(&self.host_operation_database)?;
            }
            FreshRootRegistryState::ExistingNoTargetRow
            | FreshRootRegistryState::ExactTargetRow => {
                // A cooperating registrar may have initialized the registry
                // between the read-only plan and this process acquiring the
                // previously absent lock. Its exact schema and target state
                // are revalidated under the acquired lock.
            }
        }
        self.observation = observe_fresh_root_registry_locked(
            &self.host_operation_database,
            &self.root,
            &self.expected_marker,
            self.expected_source,
        )?;
        Ok(())
    }

    pub(crate) fn check_capacity_before_marker(&mut self) -> io::Result<()> {
        if self.exclusion.is_none() {
            return Err(invalid("recovery registry exclusion is not held"));
        }
        let current = observe_fresh_root_registry_locked(
            &self.host_operation_database,
            &self.root,
            &self.expected_marker,
            self.expected_source,
        )?;
        match current.state {
            FreshRootRegistryState::MissingDatabase => {
                return Err(invalid("fresh recovery registry is still missing"));
            }
            FreshRootRegistryState::ExistingNoTargetRow
                if current.row_count.unwrap_or(usize::MAX) >= MAX_RECORDS =>
            {
                return Err(invalid(
                    "worktree registry is full; recovery will not create the root marker or prune existing rows",
                ));
            }
            FreshRootRegistryState::ExistingNoTargetRow
            | FreshRootRegistryState::ExactTargetRow => {}
        }
        self.observation = current;
        Ok(())
    }

    pub(crate) fn record_exact_target(&mut self) -> io::Result<()> {
        if self.exclusion.is_none() {
            return Err(invalid("recovery registry exclusion is not held"));
        }
        let current = observe_fresh_root_registry_locked(
            &self.host_operation_database,
            &self.root,
            &self.expected_marker,
            self.expected_source,
        )?;
        if current.state == FreshRootRegistryState::MissingDatabase {
            return Err(invalid("fresh recovery registry is still missing"));
        }
        if current.state == FreshRootRegistryState::ExactTargetRow {
            self.observation = current;
            return Ok(());
        }
        let count = current
            .row_count
            .ok_or_else(|| invalid("registry row count is unavailable"))?;
        if count >= MAX_RECORDS {
            return Err(invalid(
                "worktree registry is full; recovery did not prune or change it",
            ));
        }

        let root = fs::canonicalize(&self.root)?;
        if root != self.root {
            return Err(invalid("recovery root changed its reserved path"));
        }
        let record = RootRecord {
            schema_version: 1,
            root: root.clone(),
            repository_key: self.expected_marker.repository_key.clone(),
            repository_root: self.expected_marker.repository_root.clone(),
            source: self.expected_source,
            identity: directory_identity(&root, &real_directory(&root)?)?,
            origin_identity: directory_identity(
                &self.expected_marker.repository_root,
                &real_directory(&self.expected_marker.repository_root)?,
            )?,
        };
        let payload = serde_json::to_string(&record).map_err(invalid)?;
        if payload.len() > MAX_RECORD_BYTES {
            return Err(invalid(
                "worktree root identity exceeds registry byte limit",
            ));
        }
        marker_matches(&record)?;

        let mut connection = open_existing_registry_for_write(&self.host_operation_database)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(invalid)?;
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(invalid)?;
        let existing = retained_registration(&transaction, &root)?;
        if existing.is_some() {
            return Err(invalid(
                "target registry row changed after recovery observation",
            ));
        }
        let count: usize = transaction
            .query_row("SELECT count(*) FROM roots", [], |row| row.get(0))
            .map_err(invalid)?;
        if count >= MAX_RECORDS {
            return Err(invalid(
                "worktree registry became full; recovery did not prune or change it",
            ));
        }
        transaction
            .execute(
                "INSERT INTO roots(key,payload) VALUES(?1,?2)",
                params![record_key(&root), payload],
            )
            .map_err(invalid)?;
        transaction.commit().map_err(invalid)?;
        drop(connection);
        sync_registry_database(&self.host_operation_database)?;
        self.observation = observe_fresh_root_registry_locked(
            &self.host_operation_database,
            &self.root,
            &self.expected_marker,
            self.expected_source,
        )?;
        if self.observation.state != FreshRootRegistryState::ExactTargetRow {
            return Err(invalid(
                "durable target row did not verify after recovery insert",
            ));
        }
        Ok(())
    }

    fn create_or_acquire_recovery_lock(&mut self) -> io::Result<()> {
        let directory = registry_directory(&self.host_operation_database)?;
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                validate_recovery_registry_directory(&directory, &metadata)?;
            }
            Ok(_) => return Err(invalid("worktree registry directory changed type")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_recovery_registry_directory(&self.host_operation_database)?;
            }
            Err(error) => return Err(error),
        }
        let lock_path = registry_path(&self.host_operation_database)?.with_extension("lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        }
        match options.open(&lock_path) {
            Ok(file) => {
                let opened_metadata = file.metadata()?;
                validate_recovery_regular_file(&lock_path, &opened_metadata, true)?;
                let opened_identity = recovery_artifact_identity(&opened_metadata);
                file.sync_all()?;
                sync_registry_namespace(&self.host_operation_database)?;
                let named_before_lock = fs::symlink_metadata(&lock_path)?;
                if recovery_artifact_identity(&named_before_lock) != opened_identity {
                    return Err(invalid(
                        "new recovery lock changed before exclusion was acquired",
                    ));
                }
                self.exclusion = Some(
                    crate::file_lock::ExclusiveFileLock::try_acquire(file)?.ok_or_else(|| {
                        io::Error::new(io::ErrorKind::WouldBlock, "new registry lock is busy")
                    })?,
                );
                let named_after_lock = fs::symlink_metadata(&lock_path)?;
                if recovery_artifact_identity(&named_after_lock) != opened_identity {
                    return Err(invalid(
                        "new recovery lock changed while exclusion was acquired",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.exclusion = Some(registry_lock(&self.host_operation_database, false)?);
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }
}

pub(crate) fn plan_fresh_root_registry_recovery(
    host_operation_database: &Path,
    root: &Path,
    expected_marker: &WorktreeRootMarker,
    expected_source: WorktreeRootSource,
) -> io::Result<FreshRootRegistryObservation> {
    let guard = begin_fresh_root_registry_recovery(
        host_operation_database,
        root,
        expected_marker,
        expected_source,
    )?;
    Ok(guard.observation.clone())
}

pub(crate) fn begin_fresh_root_registry_recovery(
    host_operation_database: &Path,
    root: &Path,
    expected_marker: &WorktreeRootMarker,
    expected_source: WorktreeRootSource,
) -> io::Result<FreshRootRegistryRecoveryGuard> {
    registry_outside_root(host_operation_database, root)?;
    let lock_path = registry_path(host_operation_database)?.with_extension("lock");
    let exclusion = match fs::symlink_metadata(&lock_path) {
        Ok(_) => Some(registry_lock(host_operation_database, false)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let observation = observe_fresh_root_registry_locked(
        host_operation_database,
        root,
        expected_marker,
        expected_source,
    )?;
    Ok(FreshRootRegistryRecoveryGuard {
        host_operation_database: host_operation_database.to_path_buf(),
        root: root.to_path_buf(),
        expected_marker: expected_marker.clone(),
        expected_source,
        exclusion,
        observation,
    })
}

fn observe_fresh_root_registry_locked(
    host_operation_database: &Path,
    root: &Path,
    expected_marker: &WorktreeRootMarker,
    expected_source: WorktreeRootSource,
) -> io::Result<FreshRootRegistryObservation> {
    #[cfg(not(unix))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fresh-root recovery requires Unix no-follow filesystem identity checks",
    ));

    #[cfg(unix)]
    {
        let directory = registry_directory(host_operation_database)?;
        let database_path = registry_path(host_operation_database)?;
        let lock_path = database_path.with_extension("lock");
        let mut observation = FreshRootRegistryObservation {
            host_operation_database: host_operation_database.to_path_buf(),
            registry_directory: directory.clone(),
            database_path: database_path.clone(),
            lock_path: lock_path.clone(),
            directory_identity: None,
            database_identity: None,
            lock_identity: None,
            directory_entries: Vec::new(),
            state: FreshRootRegistryState::MissingDatabase,
            schema_version: None,
            journal_mode: None,
            integrity_check: None,
            row_count: None,
            target_payload: None,
            prune_cursor: None,
        };
        let directory_metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let database_exists = match fs::symlink_metadata(&database_path) {
                    Ok(_) => true,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                    Err(error) => return Err(error),
                };
                let lock_exists = match fs::symlink_metadata(&lock_path) {
                    Ok(_) => true,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                    Err(error) => return Err(error),
                };
                if database_exists || lock_exists {
                    return Err(invalid("registry artifacts exist without their directory"));
                }
                match fs::symlink_metadata(&directory) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Ok(_) => {
                        return Err(invalid(
                            "worktree registry directory appeared during inspection",
                        ));
                    }
                    Err(error) => return Err(error),
                }
                return Ok(observation);
            }
            Err(error) => return Err(error),
        };
        validate_recovery_registry_directory(&directory, &directory_metadata)?;
        observation.directory_identity = Some(recovery_artifact_identity(&directory_metadata));
        let mut entries = Vec::new();
        for entry in fs::read_dir(&directory)? {
            if entries.len() >= 8 {
                return Err(invalid(
                    "registry recovery directory exceeds its entry limit",
                ));
            }
            let entry = entry?;
            let name = entry.file_name();
            let text = name
                .to_str()
                .ok_or_else(|| invalid("registry recovery entry name is not UTF-8"))?
                .to_owned();
            if !matches!(text.as_str(), "registry.sqlite3" | RECOVERY_LOCK_NAME) {
                return Err(invalid(format!(
                    "unexpected worktree registry artifact {text:?}; preserve it for operator review"
                )));
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() || !kind.is_file() {
                return Err(invalid(format!(
                    "worktree registry artifact {text:?} is not a regular file"
                )));
            }
            entries.push(text);
        }
        entries.sort();
        observation.directory_entries = entries;

        let lock_metadata = match fs::symlink_metadata(&lock_path) {
            Ok(metadata) => {
                validate_recovery_regular_file(&lock_path, &metadata, true)?;
                Some(metadata)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        observation.lock_identity = lock_metadata.as_ref().map(recovery_artifact_identity);
        let database_metadata = match fs::symlink_metadata(&database_path) {
            Ok(metadata) => {
                validate_recovery_regular_file(&database_path, &metadata, false)?;
                if metadata.len() == 0 || metadata.len() > MAX_RECOVERY_REGISTRY_BYTES {
                    return Err(invalid(
                        "worktree registry database size is outside its recovery bound",
                    ));
                }
                if lock_metadata.is_none() {
                    return Err(invalid("existing registry database has no recovery lock"));
                }
                Some(metadata)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let Some(database_metadata) = database_metadata else {
            let directory_after = fs::symlink_metadata(&directory)?;
            if recovery_artifact_identity(&directory_after)
                != recovery_artifact_identity(&directory_metadata)
            {
                return Err(invalid(
                    "worktree registry directory identity changed during inspection",
                ));
            }
            let lock_after = match fs::symlink_metadata(&lock_path) {
                Ok(metadata) => Some(metadata),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            if lock_after.as_ref().map(recovery_artifact_identity)
                != lock_metadata.as_ref().map(recovery_artifact_identity)
            {
                return Err(invalid(
                    "worktree registry lock identity changed during inspection",
                ));
            }
            match fs::symlink_metadata(&database_path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(invalid(
                        "worktree registry database appeared during inspection",
                    ));
                }
                Err(error) => return Err(error),
            }
            return Ok(observation);
        };
        observation.database_identity = Some(recovery_artifact_identity(&database_metadata));
        let open_path = fs::canonicalize(&directory)?.join("registry.sqlite3");
        let connection = Connection::open_with_flags(
            &open_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(invalid)?;
        connection
            .busy_timeout(Duration::from_millis(25))
            .map_err(invalid)?;
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(invalid)?;
        if version != 1 {
            return Err(invalid(format!(
                "worktree registry schema version {version} is unsupported; preserve the database"
            )));
        }
        validate_recovery_registry_schema(&connection)?;
        let journal_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .map_err(invalid)?;
        if !journal_mode.eq_ignore_ascii_case("delete") {
            return Err(invalid(format!(
                "worktree registry journal mode {journal_mode:?} is unsupported for recovery"
            )));
        }
        let mut integrity = connection
            .prepare("PRAGMA integrity_check")
            .map_err(invalid)?;
        let mut rows = integrity.query([]).map_err(invalid)?;
        let first = rows
            .next()
            .map_err(invalid)?
            .ok_or_else(|| invalid("worktree registry integrity check returned no result"))?;
        let integrity_result: String = first.get(0).map_err(invalid)?;
        if integrity_result != "ok" || rows.next().map_err(invalid)?.is_some() {
            return Err(invalid(
                "worktree registry integrity check did not return only ok",
            ));
        }
        drop(rows);
        drop(integrity);
        let row_count: i64 = connection
            .query_row("SELECT count(*) FROM roots", [], |row| row.get(0))
            .map_err(invalid)?;
        let row_count = usize::try_from(row_count)
            .map_err(|_| invalid("worktree registry row count is invalid"))?;
        if row_count > MAX_RECORDS {
            return Err(invalid(
                "worktree registry exceeds its supported record limit",
            ));
        }
        let prune_cursor: Option<String> = connection
            .query_row(
                "SELECT value FROM registry_meta WHERE key='prune_cursor'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(invalid)?;
        let mut meta = connection
            .prepare("SELECT key,value FROM registry_meta ORDER BY key")
            .map_err(invalid)?;
        let meta_rows = meta
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(invalid)?;
        let mut meta_count = 0;
        for row in meta_rows {
            let (key, value) = row.map_err(invalid)?;
            meta_count += 1;
            if key != "prune_cursor" || value.len() > 64 {
                return Err(invalid("worktree registry contains unknown metadata"));
            }
            if !value.is_empty()
                && (value.len() != 64
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            {
                return Err(invalid("worktree registry prune cursor is malformed"));
            }
        }
        if meta_count > 1 {
            return Err(invalid("worktree registry contains duplicate metadata"));
        }
        let key = record_key(root);
        let target_payload: Option<String> = connection
            .query_row(
                "SELECT payload FROM roots WHERE key=?1",
                [key.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(invalid)?;
        if let Some(payload) = &target_payload {
            if payload.len() > MAX_RECORD_BYTES {
                return Err(invalid("target root registry row exceeds its byte bound"));
            }
            let record = decode(&key, payload)?;
            if record.root != root
                || record.repository_key != expected_marker.repository_key
                || record.repository_root != expected_marker.repository_root
                || record.source != expected_source
            {
                return Err(invalid(
                    "target registry row conflicts with the reserved root",
                ));
            }
            marker_owner_matches(&record, &read_marker(root)?)?;
            validate_retained_registration(root, payload)?;
            observation.state = FreshRootRegistryState::ExactTargetRow;
        } else {
            observation.state = FreshRootRegistryState::ExistingNoTargetRow;
        }
        observation.schema_version = Some(version);
        observation.journal_mode = Some(journal_mode);
        observation.integrity_check = Some(integrity_result);
        observation.row_count = Some(row_count);
        observation.target_payload = target_payload;
        observation.prune_cursor = prune_cursor;
        let after = fs::symlink_metadata(&database_path)?;
        if recovery_artifact_identity(&after) != recovery_artifact_identity(&database_metadata) {
            return Err(invalid(
                "worktree registry database identity changed during inspection",
            ));
        }
        let directory_after = fs::symlink_metadata(&directory)?;
        if recovery_artifact_identity(&directory_after)
            != recovery_artifact_identity(&directory_metadata)
        {
            return Err(invalid(
                "worktree registry directory identity changed during inspection",
            ));
        }
        let lock_after = match fs::symlink_metadata(&lock_path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if lock_after.as_ref().map(recovery_artifact_identity)
            != lock_metadata.as_ref().map(recovery_artifact_identity)
        {
            return Err(invalid(
                "worktree registry lock identity changed during inspection",
            ));
        }
        Ok(observation)
    }
}

#[cfg(unix)]
fn create_recovery_registry_directory(host_operation_database: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let directory = registry_directory(host_operation_database)?;
    let parent = directory
        .parent()
        .ok_or_else(|| invalid("worktree registry has no parent directory"))?;
    real_directory(parent)?;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(&directory)?;
    let metadata = fs::symlink_metadata(&directory)?;
    validate_recovery_registry_directory(&directory, &metadata)?;
    sync_registry_namespace(host_operation_database)
}

#[cfg(not(unix))]
fn create_recovery_registry_directory(_host_operation_database: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fresh-root recovery requires Unix directory durability",
    ))
}

#[cfg(unix)]
fn initialize_fresh_recovery_registry(host_operation_database: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let database_path = registry_path(host_operation_database)?;
    let directory = database_path
        .parent()
        .ok_or_else(|| invalid("worktree registry has no directory"))?;
    let directory_metadata = fs::symlink_metadata(directory)?;
    validate_recovery_registry_directory(directory, &directory_metadata)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = match options.open(&database_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error),
    };
    file.sync_all()?;
    let created_identity = recovery_artifact_identity(&file.metadata()?);
    drop(file);
    let open_path = fs::canonicalize(directory)?.join("registry.sqlite3");
    let connection = Connection::open_with_flags(
        &open_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(invalid)?;
    let named_before = fs::symlink_metadata(&database_path)?;
    if recovery_artifact_identity(&named_before) != created_identity {
        return Err(invalid(
            "new recovery registry database changed before initialization",
        ));
    }
    connection
        .busy_timeout(Duration::from_millis(25))
        .map_err(invalid)?;
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA max_page_count=8192;",
        )
        .map_err(invalid)?;
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(invalid)?;
    let object_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .map_err(invalid)?;
    if version != 0 || object_count != 0 {
        return Err(invalid("new recovery registry database is not empty"));
    }
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE roots (key TEXT PRIMARY KEY, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 8192));
             CREATE TABLE registry_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             PRAGMA user_version=1;
             COMMIT;",
        )
        .map_err(invalid)?;
    let named_after = fs::symlink_metadata(&database_path)?;
    if !same_recovery_artifact_object(&recovery_artifact_identity(&named_after), &created_identity)
    {
        return Err(invalid(
            "recovery registry database changed during initialization",
        ));
    }
    drop(connection);
    sync_registry_database(host_operation_database)
}

#[cfg(not(unix))]
fn initialize_fresh_recovery_registry(_host_operation_database: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fresh-root recovery requires Unix no-follow database creation",
    ))
}

#[cfg(unix)]
fn sync_registry_database(host_operation_database: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let database_path = registry_path(host_operation_database)?;
    let metadata = fs::symlink_metadata(&database_path)?;
    validate_recovery_regular_file(&database_path, &metadata, false)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let database = options.open(&database_path)?;
    if recovery_artifact_identity(&database.metadata()?) != recovery_artifact_identity(&metadata) {
        return Err(invalid("worktree registry changed before durable sync"));
    }
    database.sync_all()?;
    sync_registry_namespace(host_operation_database)
}

#[cfg(not(unix))]
fn sync_registry_database(_host_operation_database: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fresh-root recovery requires Unix database syncing",
    ))
}

#[cfg(unix)]
fn recovery_artifact_identity(metadata: &Metadata) -> RecoveryArtifactIdentity {
    use std::os::unix::fs::MetadataExt;
    RecoveryArtifactIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode: metadata.mode() & 0o7777,
        links: metadata.nlink(),
        bytes: metadata.len(),
    }
}

fn same_recovery_artifact_object(
    left: &RecoveryArtifactIdentity,
    right: &RecoveryArtifactIdentity,
) -> bool {
    left.device == right.device
        && left.inode == right.inode
        && left.owner == right.owner
        && left.mode == right.mode
        && left.links == right.links
}

#[cfg(not(unix))]
fn recovery_artifact_identity(metadata: &Metadata) -> RecoveryArtifactIdentity {
    RecoveryArtifactIdentity {
        device: 0,
        inode: 0,
        owner: 0,
        mode: 0,
        links: 0,
        bytes: metadata.len(),
    }
}

#[cfg(unix)]
fn validate_recovery_registry_directory(path: &Path, metadata: &Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no pointer arguments or caller-side memory preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o022 != 0
    {
        return Err(invalid(format!(
            "registry directory {} is not a private directory owned by the current user",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_recovery_registry_directory(_path: &Path, metadata: &Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid("registry directory is not a real directory"));
    }
    Ok(())
}

fn validate_recovery_regular_file(path: &Path, metadata: &Metadata, lock: bool) -> io::Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid(format!(
            "registry artifact {} is not a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no pointer arguments or caller-side memory preconditions.
        let effective_uid = unsafe { libc::geteuid() };
        if metadata.nlink() != 1 || metadata.uid() != effective_uid {
            return Err(invalid(format!(
                "registry artifact {} is not a single-link file owned by the current user",
                path.display()
            )));
        }
        if lock && metadata.mode() & 0o022 != 0 {
            return Err(invalid("registry lock is writable by group or other users"));
        }
    }
    Ok(())
}

fn validate_recovery_registry_schema(connection: &Connection) -> io::Result<()> {
    let mut statement = connection
        .prepare("SELECT type,name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name")
        .map_err(invalid)?;
    let objects = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(invalid)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    let expected = vec![
        (
            "table".to_owned(),
            "registry_meta".to_owned(),
            Some("CREATE TABLE registry_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)".to_owned()),
        ),
        (
            "table".to_owned(),
            "roots".to_owned(),
            Some("CREATE TABLE roots (key TEXT PRIMARY KEY, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 8192))".to_owned()),
        ),
    ];
    if objects != expected {
        return Err(invalid(
            "worktree registry schema differs from the supported exact schema",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn effective_user_id() -> libc::uid_t {
    // SAFETY: geteuid takes no pointers and only returns the calling process'
    // effective user ID.
    unsafe { libc::geteuid() }
}

/// Sync the entries that name the host registry before its row can be used to
/// complete a repository-root binding. SQLite's FULL commit makes registry
/// contents durable; these directory syncs make a newly-created registry file
/// and its containing directory reachable after a crash as well.
#[cfg(unix)]
fn sync_registry_namespace(host_operation_database: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    fn open_checked_directory(
        path: &Path,
        require_current_owner: bool,
    ) -> io::Result<(fs::File, Metadata)> {
        let named_before = fs::symlink_metadata(path)?;
        if !named_before.is_dir() || named_before.file_type().is_symlink() {
            return Err(invalid("worktree registry directory changed type"));
        }
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        );
        let directory = options.open(path)?;
        let opened = directory.metadata()?;
        let named_after = fs::symlink_metadata(path)?;
        if !opened.is_dir()
            || !named_after.is_dir()
            || named_after.file_type().is_symlink()
            || opened.dev() != named_before.dev()
            || opened.ino() != named_before.ino()
            || opened.uid() != named_before.uid()
            || opened.dev() != named_after.dev()
            || opened.ino() != named_after.ino()
            || opened.uid() != named_after.uid()
            || (require_current_owner && opened.uid() != effective_user_id())
        {
            return Err(invalid(
                "worktree registry directory identity changed before syncing",
            ));
        }
        Ok((directory, opened))
    }

    let host_parent = host_operation_database
        .parent()
        .ok_or_else(|| invalid("host operation database has no parent"))?;
    let host_parent = fs::canonicalize(host_parent)?;
    let directory_name = registry_directory(host_operation_database)?
        .file_name()
        .ok_or_else(|| invalid("worktree registry has no directory name"))?
        .to_os_string();
    let registry_directory = host_parent.join(directory_name);

    // Open both names without following their final components. The registry
    // directory is a child of the existing host database parent; syncing the
    // child makes registry.sqlite3 durable, and syncing the parent makes the
    // child directory name durable.
    let (registry, registry_identity) = open_checked_directory(&registry_directory, true)?;
    // The host database parent may be an intentionally shared, root-owned
    // sticky directory such as /tmp. Pin its identity for the sync without
    // changing its ownership or requiring it to belong to this user.
    let (parent, parent_identity) = open_checked_directory(&host_parent, false)?;
    registry.sync_all()?;
    parent.sync_all()?;

    for (path, opened) in [
        (&registry_directory, registry_identity),
        (&host_parent, parent_identity),
    ] {
        let named_after = fs::symlink_metadata(path)?;
        if !named_after.is_dir()
            || named_after.file_type().is_symlink()
            || named_after.dev() != opened.dev()
            || named_after.ino() != opened.ino()
            || named_after.uid() != opened.uid()
        {
            return Err(invalid(
                "worktree registry directory identity changed while syncing",
            ));
        }
    }
    Ok(())
}

// Fresh/bound repository-root provisioning is refused on non-Unix platforms,
// where this module cannot open and sync directory entries without following
// links. Ordinary legacy registry operations remain available there.
#[cfg(not(unix))]
fn sync_registry_namespace(_host_operation_database: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fresh-root recovery requires Unix directory durability",
    ))
}
fn record_key(root: &Path) -> String {
    format!("{:x}", Sha256::digest(root.as_os_str().as_encoded_bytes()))
}

fn regular_file(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("worktree registry is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(invalid("worktree registry has multiple hard links"));
        }
    }
    Ok(())
}

fn registry_outside_root(host_database: &Path, root: &Path) -> io::Result<()> {
    let registry = crate::broker::lifecycle::canonical_prefix(&registry_directory(host_database)?)
        .map_err(invalid)?;
    if registry.starts_with(root) {
        return Err(invalid(
            "worktree registry must reside outside the root it coordinates",
        ));
    }
    Ok(())
}

/// One stable host-domain lock excludes root admission for the entire orphan
/// removal. It is never unlinked or truncated. Contention refuses admission
/// promptly; a crashed remover releases the OS lock without stale tombstones.
fn registry_lock(
    host_database: &Path,
    create: bool,
) -> io::Result<crate::file_lock::ExclusiveFileLock> {
    let path = registry_path(host_database)?.with_extension("lock");
    if create {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("registry lock has no parent"))?;
        fs::create_dir_all(parent)?;
        real_directory(parent)?;
        crate::host_state::protect_host_state_path(parent, true)?;
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(&path)?;
    regular_file(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid("registry lock changed type"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let named = fs::symlink_metadata(&path)?;
        if metadata.nlink() != 1 || named.ino() != metadata.ino() || named.dev() != metadata.dev() {
            return Err(invalid(
                "registry lock object changed or has multiple links",
            ));
        }
    }
    let claim = crate::file_lock::ExclusiveFileLock::try_acquire(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "worktree history is being updated or reclaimed; retry after GC completes",
        )
    })?;
    regular_file(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let named = fs::symlink_metadata(&path)?;
        if named.nlink() != 1 || named.ino() != metadata.ino() || named.dev() != metadata.dev() {
            return Err(invalid(
                "registry lock object changed while acquiring exclusion",
            ));
        }
    }
    Ok(claim)
}

/// Foreground creation waits briefly for another registrar or remover. Each
/// attempt repeats the lock object's type, identity and pathname checks. GC
/// keeps its nonblocking acquisition so it yields to an admitted foreground
/// operation rather than waiting while holding other cleanup resources.
fn registration_lock(
    host_database: &Path,
    wait: Duration,
) -> io::Result<crate::file_lock::ExclusiveFileLock> {
    let deadline = Instant::now() + wait;
    loop {
        match registry_lock(host_database, true) {
            Ok(claim) => return Ok(claim),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
                tests::observe_registration_contention();
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "worktree history coordination remained busy through the bounded registration wait; retry the command",
                    ));
                }
                std::thread::sleep(REGISTRATION_LOCK_RETRY.min(remaining));
            }
            Err(error) => return Err(error),
        }
    }
}

fn open_registry(host_database: &Path, create: bool) -> io::Result<Connection> {
    open_registry_with_mode(host_database, create, create)
}

fn open_existing_registry_for_write(host_database: &Path) -> io::Result<Connection> {
    open_registry_with_mode(host_database, false, true)
}

fn open_registry_with_mode(
    host_database: &Path,
    create: bool,
    writable: bool,
) -> io::Result<Connection> {
    let path = registry_path(host_database)?;
    if create {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("registry has no parent"))?;
        fs::create_dir_all(parent)?;
        real_directory(parent)?;
        crate::host_state::protect_host_state_path(parent, true)?;
    }
    match regular_file(&path) {
        Ok(()) => {}
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            }
            match options.open(&path) {
                Ok(file) => file.sync_all()?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => regular_file(&path)?,
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    }
    // SQLite NOFOLLOW rejects symlinks in parent components too (notably
    // macOS /var -> /private/var). Resolve only the verified private parent;
    // never canonicalize the database leaf and thereby accept a symlink.
    let path = fs::canonicalize(
        path.parent()
            .ok_or_else(|| invalid("registry has no parent"))?,
    )?
    .join("registry.sqlite3");
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(&path, flags).map_err(invalid)?;
    connection
        .busy_timeout(Duration::from_millis(25))
        .map_err(invalid)?;
    if create {
        // The registry itself must not become another unbounded artifact.
        // DELETE journaling avoids WAL growth pinned by a slow inventory reader.
        connection
            .execute_batch(
                "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA max_page_count=8192;",
            )
            .map_err(invalid)?;
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(invalid)?;
        match version {
            0 => connection.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS roots (key TEXT PRIMARY KEY, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 8192));
                CREATE TABLE IF NOT EXISTS registry_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                PRAGMA user_version=1; COMMIT;").map_err(invalid)?,
            1 => {},
            _ => return Err(invalid("unsupported worktree registry schema")),
        }
    }
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(invalid)?;
    if version != 1 {
        return Err(invalid("unsupported worktree registry schema"));
    }
    regular_file(&path)?;
    Ok(connection)
}

/// Exact, one-root proposal for upgrading a legacy filesystem identity.
/// Historical UUID identity is deliberately never claimed: the user reviews
/// the live inodes, owner marker, repository binding, both payload hashes, and
/// the exact marker-byte digest.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RootIdentityRecoveryPlan {
    pub(crate) host_operation_database: PathBuf,
    pub(crate) root: PathBuf,
    pub(crate) repository_root: PathBuf,
    pub(crate) repository_key: String,
    pub(crate) old_payload_sha256: String,
    pub(crate) new_payload_sha256: String,
    pub(crate) owner_marker_sha256: String,
    pub(crate) root_inode: u64,
    pub(crate) root_volume_uuid_hex: String,
    pub(crate) origin_inode: u64,
    pub(crate) origin_volume_uuid_hex: String,
    pub(crate) historical_uuid_proven: bool,
    pub(crate) confirmation: String,
    pub(crate) applied: bool,
}

struct PreparedRootIdentityRecovery {
    plan: RootIdentityRecoveryPlan,
    key: String,
    previous_payload: String,
    replacement_payload: String,
    replacement_record: RootRecord,
    owner_marker_payload: Vec<u8>,
}

struct ObservedRecoveryRecord {
    record: RootRecord,
    owner_marker_payload: Vec<u8>,
}

fn recovery_volume_uuid(identity: &DirectoryIdentity, object: &str) -> io::Result<[u8; 16]> {
    match identity.volume_uuid {
        Some(uuid) if uuid.iter().any(|byte| *byte != 0) => Ok(uuid),
        Some(_) => Err(invalid(format!(
            "current {object} filesystem returned a nil volume UUID"
        ))),
        None => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("current {object} filesystem has no stable volume UUID"),
        )),
    }
}

fn recovery_identity_matches(
    saved: &DirectoryIdentity,
    current: &DirectoryIdentity,
    object: &str,
) -> io::Result<()> {
    if saved.inode != current.inode {
        return Err(invalid(format!(
            "registered {object} inode changed; retained history was preserved"
        )));
    }
    if let Some(saved_uuid) = saved.volume_uuid
        && recovery_volume_uuid(current, object)? != saved_uuid
    {
        return Err(invalid(format!(
            "registered {object} volume UUID changed; retained history was preserved"
        )));
    }
    Ok(())
}

fn observe_recovery_record(
    record: &RootRecord,
    root: &Path,
    repository_root: &Path,
    repository_key: &str,
    identity_reader: &impl Fn(&Path, &Metadata) -> io::Result<Option<DirectoryIdentity>>,
) -> io::Result<ObservedRecoveryRecord> {
    if !root.is_absolute() || record.root != root {
        return Err(invalid(
            "the supplied exact root differs from the retained root record",
        ));
    }
    if !repository_root.is_absolute()
        || record.repository_root != repository_root
        || record.repository_key != repository_key
    {
        return Err(invalid(
            "current repository binding differs from the retained root owner",
        ));
    }

    let (marker, marker_payload) = read_marker_with_payload(root)?;
    marker_owner_matches(record, &marker)?;
    ensure_canonical_marker_payload(&marker, &marker_payload)?;

    let saved_root = record
        .identity
        .as_ref()
        .ok_or_else(|| invalid("retained root has no filesystem identity"))?;
    let root_identity = identity_reader(root, &real_directory(root)?)?
        .ok_or_else(|| invalid("current root filesystem identity is unavailable"))?;
    recovery_volume_uuid(&root_identity, "root")?;
    recovery_identity_matches(saved_root, &root_identity, "root")?;

    let saved_origin = record
        .origin_identity
        .as_ref()
        .ok_or_else(|| invalid("retained origin has no filesystem identity"))?;
    let origin_identity = identity_reader(repository_root, &real_directory(repository_root)?)?
        .ok_or_else(|| invalid("current origin filesystem identity is unavailable"))?;
    recovery_volume_uuid(&origin_identity, "origin")?;
    recovery_identity_matches(saved_origin, &origin_identity, "origin")?;

    Ok(ObservedRecoveryRecord {
        record: RootRecord {
            identity: Some(root_identity),
            origin_identity: Some(origin_identity),
            ..record.clone()
        },
        owner_marker_payload: marker_payload,
    })
}

fn prepare_root_identity_recovery(
    host_database: &Path,
    root: &Path,
    repository_root: &Path,
    repository_key: &str,
    previous_payload: &str,
    identity_reader: &impl Fn(&Path, &Metadata) -> io::Result<Option<DirectoryIdentity>>,
) -> io::Result<PreparedRootIdentityRecovery> {
    require_absolute_recovery_paths(host_database, root, repository_root)?;
    let key = record_key(root);
    let retained = decode(&key, previous_payload)?;
    let was_legacy = retained
        .identity
        .as_ref()
        .is_some_and(|identity| identity.volume_uuid.is_none())
        || retained
            .origin_identity
            .as_ref()
            .is_some_and(|identity| identity.volume_uuid.is_none());
    if !was_legacy {
        return Err(invalid(
            "retained root already has stable volume UUIDs; identity recovery is unnecessary",
        ));
    }

    let observed = observe_recovery_record(
        &retained,
        root,
        repository_root,
        repository_key,
        identity_reader,
    )?;
    let replacement_record = observed.record;
    let owner_marker_payload = observed.owner_marker_payload;
    let replacement_payload = serde_json::to_string(&replacement_record).map_err(invalid)?;
    if replacement_payload.len() > MAX_RECORD_BYTES {
        return Err(invalid(
            "recovered worktree root identity exceeds registry byte limit",
        ));
    }
    let old_payload_sha256 = format!("{:x}", Sha256::digest(previous_payload.as_bytes()));
    let new_payload_sha256 = format!("{:x}", Sha256::digest(replacement_payload.as_bytes()));
    let owner_marker_sha256 = format!("{:x}", Sha256::digest(&owner_marker_payload));
    let root_identity = replacement_record
        .identity
        .as_ref()
        .ok_or_else(|| invalid("recovery target has no root identity"))?;
    let origin_identity = replacement_record
        .origin_identity
        .as_ref()
        .ok_or_else(|| invalid("recovery target has no origin identity"))?;
    let confirmation = format!("{old_payload_sha256}:{new_payload_sha256}:{owner_marker_sha256}");
    Ok(PreparedRootIdentityRecovery {
        plan: RootIdentityRecoveryPlan {
            host_operation_database: host_database.to_path_buf(),
            root: root.to_path_buf(),
            repository_root: repository_root.to_path_buf(),
            repository_key: repository_key.to_string(),
            old_payload_sha256,
            new_payload_sha256,
            owner_marker_sha256,
            root_inode: root_identity.inode,
            root_volume_uuid_hex: format_uuid_hex(&recovery_volume_uuid(root_identity, "root")?),
            origin_inode: origin_identity.inode,
            origin_volume_uuid_hex: format_uuid_hex(&recovery_volume_uuid(
                origin_identity,
                "origin",
            )?),
            historical_uuid_proven: false,
            confirmation,
            applied: false,
        },
        key,
        previous_payload: previous_payload.to_string(),
        replacement_payload,
        replacement_record,
        owner_marker_payload,
    })
}

fn format_uuid_hex(uuid: &[u8; 16]) -> String {
    let digits = b"0123456789abcdef";
    let mut output = String::with_capacity(32);
    for byte in uuid {
        output.push(digits[(*byte >> 4) as usize] as char);
        output.push(digits[(*byte & 0x0f) as usize] as char);
    }
    output
}

fn ensure_canonical_marker_payload(marker: &WorktreeRootMarker, payload: &[u8]) -> io::Result<()> {
    let compact = serde_json::to_vec(marker).map_err(invalid)?;
    let pretty = serde_json::to_vec_pretty(marker).map_err(invalid)?;
    let mut compact_with_newline = compact.clone();
    compact_with_newline.push(b'\n');
    let mut pretty_with_newline = pretty;
    pretty_with_newline.push(b'\n');
    let canonical = payload == compact.as_slice()
        || payload == compact_with_newline.as_slice()
        || payload == pretty_with_newline.as_slice();
    if canonical {
        Ok(())
    } else {
        Err(invalid(
            "worktree owner marker payload changed; retained history was preserved",
        ))
    }
}

fn require_absolute_recovery_paths(
    host_database: &Path,
    root: &Path,
    repository_root: &Path,
) -> io::Result<()> {
    if !host_database.is_absolute() || !root.is_absolute() || !repository_root.is_absolute() {
        return Err(invalid(
            "host database, exact root, and repository root must be absolute paths",
        ));
    }
    Ok(())
}

/// Read-only plan for upgrading one exact legacy root under the same lock used
/// by admission and removal claims. The caller supplies the host database and
/// repository binding explicitly; environment-selected paths are not consulted.
pub(crate) fn plan_root_identity_recovery(
    host_database: &Path,
    root: &Path,
    repository_root: &Path,
    repository_key: &str,
) -> io::Result<RootIdentityRecoveryPlan> {
    plan_root_identity_recovery_with(
        host_database,
        root,
        repository_root,
        repository_key,
        &directory_identity,
    )
}

fn plan_root_identity_recovery_with(
    host_database: &Path,
    root: &Path,
    repository_root: &Path,
    repository_key: &str,
    identity_reader: &impl Fn(&Path, &Metadata) -> io::Result<Option<DirectoryIdentity>>,
) -> io::Result<RootIdentityRecoveryPlan> {
    require_absolute_recovery_paths(host_database, root, repository_root)?;
    let _exclusion = registry_lock(host_database, false)?;
    let connection = open_registry(host_database, false)?;
    let previous_payload = retained_registration(&connection, root)?
        .ok_or_else(|| invalid("no retained root record exists for the exact path"))?;
    Ok(prepare_root_identity_recovery(
        host_database,
        root,
        repository_root,
        repository_key,
        &previous_payload,
        identity_reader,
    )?
    .plan)
}

/// Apply one explicitly reviewed exact-root upgrade. The old payload,
/// replacement payload, and exact owner-marker bytes hashes are all required;
/// the retained row changes only by exact SQL compare-and-swap under the shared
/// registry lock.
pub(crate) fn apply_root_identity_recovery(
    host_database: &Path,
    root: &Path,
    repository_root: &Path,
    repository_key: &str,
    confirm: &str,
) -> io::Result<RootIdentityRecoveryPlan> {
    apply_root_identity_recovery_with(
        host_database,
        root,
        repository_root,
        repository_key,
        confirm,
        &directory_identity,
        || Ok(()),
    )
}

fn apply_root_identity_recovery_with(
    host_database: &Path,
    root: &Path,
    repository_root: &Path,
    repository_key: &str,
    confirm: &str,
    identity_reader: &impl Fn(&Path, &Metadata) -> io::Result<Option<DirectoryIdentity>>,
    before_compare_and_swap: impl FnOnce() -> io::Result<()>,
) -> io::Result<RootIdentityRecoveryPlan> {
    apply_root_identity_recovery_with_hooks(
        host_database,
        root,
        repository_root,
        repository_key,
        confirm,
        identity_reader,
        RootIdentityRecoveryHooks {
            before_compare_and_swap,
            after_update_before_reobservation: || Ok(()),
        },
    )
}

struct RootIdentityRecoveryHooks<BeforeUpdate, AfterUpdate> {
    before_compare_and_swap: BeforeUpdate,
    after_update_before_reobservation: AfterUpdate,
}

fn apply_root_identity_recovery_with_hooks<BeforeUpdate, AfterUpdate>(
    host_database: &Path,
    root: &Path,
    repository_root: &Path,
    repository_key: &str,
    confirm: &str,
    identity_reader: &impl Fn(&Path, &Metadata) -> io::Result<Option<DirectoryIdentity>>,
    hooks: RootIdentityRecoveryHooks<BeforeUpdate, AfterUpdate>,
) -> io::Result<RootIdentityRecoveryPlan>
where
    BeforeUpdate: FnOnce() -> io::Result<()>,
    AfterUpdate: FnOnce() -> io::Result<()>,
{
    let RootIdentityRecoveryHooks {
        before_compare_and_swap,
        after_update_before_reobservation,
    } = hooks;
    require_absolute_recovery_paths(host_database, root, repository_root)?;
    let _exclusion = registry_lock(host_database, false)?;
    let mut connection = open_existing_registry_for_write(host_database)?;
    let previous_payload = retained_registration(&connection, root)?
        .ok_or_else(|| invalid("no retained root record exists for the exact path"))?;
    let prepared = prepare_root_identity_recovery(
        host_database,
        root,
        repository_root,
        repository_key,
        &previous_payload,
        identity_reader,
    )?;
    if prepared.plan.confirmation != confirm {
        return Err(invalid(
            "old:new:owner-marker confirmation does not match the current exact-root recovery plan",
        ));
    }

    // This hook is a no-op in production. It makes a writer that ignores the
    // registry lock deterministic in the CAS regression test.
    before_compare_and_swap()?;

    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(invalid)?;
    let changed = transaction
        .execute(
            "UPDATE roots SET payload=?1 WHERE key=?2 AND payload=?3",
            params![
                prepared.replacement_payload,
                prepared.key,
                prepared.previous_payload
            ],
        )
        .map_err(invalid)?;
    if changed != 1 {
        return Err(invalid(
            "retained root history changed before exact recovery update; current row was preserved",
        ));
    }

    // This test-only hook models an external marker writer after the row has
    // been updated but before the transaction's final live reobservation.
    after_update_before_reobservation()?;

    let observed = observe_recovery_record(
        &prepared.replacement_record,
        root,
        repository_root,
        repository_key,
        identity_reader,
    )?;
    if observed.owner_marker_payload != prepared.owner_marker_payload {
        return Err(invalid(
            "owner marker bytes changed during recovery; transaction rolled back",
        ));
    }
    let current_payload = serde_json::to_string(&observed.record).map_err(invalid)?;
    if current_payload != prepared.replacement_payload {
        return Err(invalid(
            "root, origin, or owner marker changed during recovery; transaction rolled back",
        ));
    }
    transaction.commit().map_err(invalid)?;
    let mut plan = prepared.plan;
    plan.applied = true;
    Ok(plan)
}

pub(crate) fn read_marker(root: &Path) -> io::Result<WorktreeRootMarker> {
    read_marker_with_payload(root).map(|(marker, _)| marker)
}

fn read_marker_with_payload(root: &Path) -> io::Result<(WorktreeRootMarker, Vec<u8>)> {
    real_directory(root)?;
    let path = root.join(WORKTREE_ROOT_MARKER);
    regular_file(&path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("worktree marker changed type"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let marker = decode_marker(&bytes)?;
    Ok((marker, bytes))
}

fn decode_marker(bytes: &[u8]) -> io::Result<WorktreeRootMarker> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(invalid("worktree marker exceeds its byte limit"));
    }
    let marker: WorktreeRootMarker = serde_json::from_slice(bytes).map_err(invalid)?;
    if marker.schema_version != WORKTREE_ROOT_SCHEMA_VERSION
        || marker.repository_key.is_empty()
        || !marker.repository_root.is_absolute()
    {
        return Err(invalid("worktree marker identity or schema is invalid"));
    }
    Ok(marker)
}

fn marker_matches(record: &RootRecord) -> io::Result<()> {
    let marker = read_marker(&record.root)?;
    marker_owner_matches(record, &marker)
}

fn marker_owner_matches(record: &RootRecord, marker: &WorktreeRootMarker) -> io::Result<()> {
    if marker.repository_key != record.repository_key
        || marker.repository_root != record.repository_root
    {
        return Err(invalid("worktree marker differs from registered owner"));
    }
    Ok(())
}

fn decode(key: &str, payload: &str) -> io::Result<RootRecord> {
    if payload.len() > MAX_RECORD_BYTES {
        return Err(invalid("registry record exceeds byte limit"));
    }
    let record: RootRecord = serde_json::from_str(payload).map_err(invalid)?;
    if record.schema_version != 1
        || !record.root.is_absolute()
        || !record.repository_root.is_absolute()
        || record.repository_key.is_empty()
        || record_key(&record.root) != key
    {
        return Err(invalid("registry record identity or schema is invalid"));
    }
    Ok(record)
}

/// A missing path counts only when its nearest existing real ancestor is on the
/// exact filesystem recorded while the path existed. An unmounted/replaced
/// external volume, symlink or unsupported platform leaves the record unknown.
fn absent_on_original_filesystem(
    path: &Path,
    identity: Option<&DirectoryIdentity>,
) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let identity = identity.ok_or_else(|| invalid("no saved filesystem identity"))?;
    for ancestor in path.ancestors().skip(1) {
        match real_directory(ancestor) {
            Ok(metadata) => {
                let actual = directory_identity(ancestor, &metadata)?
                    .ok_or_else(|| invalid("unsupported filesystem identity"))?;
                if identity.same_registered_filesystem(&actual) {
                    return Ok(true);
                }
                if legacy_uuid_recovery_needed(Some(identity), Some(&actual)) {
                    return Err(invalid(
                        "legacy root identity has no saved volume UUID; reviewed identity recovery is required before absence can be proved",
                    ));
                }
                return Err(invalid("original filesystem is unavailable or replaced"));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Err(invalid("no inspectable ancestor"))
}

fn root_still_present(record: &RootRecord) -> io::Result<bool> {
    let metadata = match real_directory(&record.root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return absent_on_original_filesystem(&record.root, record.identity.as_ref())
                .map(|absent| !absent);
        }
        Err(error) => return Err(error),
    };
    let current = directory_identity(&record.root, &metadata)?;
    if record.identity.is_none()
        || !same_optional_registered_object(record.identity.as_ref(), current.as_ref())
    {
        return Err(invalid(changed_identity_reason(
            "root",
            record.identity.as_ref(),
            current.as_ref(),
        )));
    }
    marker_matches(record)?;
    Ok(true)
}

pub(crate) fn origin_is_missing(record: &RootRecord) -> io::Result<bool> {
    absent_on_original_filesystem(&record.repository_root, record.origin_identity.as_ref())
}

// Materialize small pages and release the statement before any filesystem IO.
fn page(connection: &Connection, after: &str) -> io::Result<Vec<(String, String)>> {
    let mut statement = connection.prepare("SELECT key, CASE WHEN length(CAST(payload AS BLOB)) <= 8192 THEN payload ELSE NULL END FROM roots WHERE key > ?1 ORDER BY key LIMIT 64").map_err(invalid)?;
    let rows = statement
        .query_map([after], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(invalid)?;
    let mut result = Vec::new();
    for row in rows {
        let (key, payload) = row.map_err(invalid)?;
        if key.len() != 64 {
            return Err(invalid("invalid registry key"));
        }
        result.push((
            key,
            payload.ok_or_else(|| invalid("oversized registry record"))?,
        ));
    }
    Ok(result)
}

/// Holds the host-domain exclusion from capacity admission until the exact
/// created root has been recorded. Git checkout/session allocation must wait
/// for `record` to succeed.
pub(crate) struct RegistrationAdmission {
    root: PathBuf,
    existing_payload: Option<String>,
    connection: Connection,
    _exclusion: crate::file_lock::ExclusiveFileLock,
}

pub(crate) fn admit(
    host_database: &Path,
    planned_root: &Path,
) -> io::Result<RegistrationAdmission> {
    admit_with_limit(host_database, planned_root, MAX_RECORDS)
}

fn admit_with_limit(
    host_database: &Path,
    planned_root: &Path,
    limit: usize,
) -> io::Result<RegistrationAdmission> {
    admit_with_limit_and_wait(host_database, planned_root, limit, REGISTRATION_LOCK_WAIT)
}

fn admit_with_limit_and_wait(
    host_database: &Path,
    planned_root: &Path,
    limit: usize,
    wait: Duration,
) -> io::Result<RegistrationAdmission> {
    let root = crate::broker::lifecycle::canonical_prefix(planned_root).map_err(invalid)?;
    registry_outside_root(host_database, &root)?;
    let exclusion = registration_lock(host_database, wait)?;
    let mut connection = open_registry(host_database, true)?;
    maintain_capacity(&mut connection, &record_key(&root), limit)?;
    let existing_payload = retained_registration(&connection, &root)?;
    if let Some(payload) = &existing_payload {
        validate_retained_registration(&root, payload)?;
    }
    Ok(RegistrationAdmission {
        root,
        existing_payload,
        connection,
        _exclusion: exclusion,
    })
}

impl RegistrationAdmission {
    pub(crate) fn record(
        mut self,
        root: &Path,
        marker: &WorktreeRootMarker,
        source: WorktreeRootSource,
    ) -> io::Result<()> {
        let root = fs::canonicalize(root)?;
        if root != self.root {
            return Err(invalid("worktree root changed after storage admission"));
        }
        let actual = read_marker(&root)?;
        if actual.schema_version != marker.schema_version
            || actual.repository_key != marker.repository_key
            || actual.repository_root != marker.repository_root
        {
            return Err(invalid("worktree marker differs from admission owner"));
        }
        let record = RootRecord {
            schema_version: 1,
            root: root.clone(),
            repository_key: marker.repository_key.clone(),
            repository_root: marker.repository_root.clone(),
            source,
            identity: directory_identity(&root, &real_directory(&root)?)?,
            origin_identity: directory_identity(
                &marker.repository_root,
                &real_directory(&marker.repository_root)?,
            )?,
        };
        let payload = serde_json::to_string(&record).map_err(invalid)?;
        if payload.len() > MAX_RECORD_BYTES {
            return Err(invalid(
                "worktree root identity exceeds registry byte limit",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(invalid)?;
        if retained_registration(&transaction, &root)? != self.existing_payload {
            return Err(invalid("retained root history changed after admission"));
        }
        if let Some(previous) = &self.existing_payload {
            let retained = validate_retained_registration(&root, previous)?;
            if !same_optional_registered_object(
                retained.identity.as_ref(),
                record.identity.as_ref(),
            ) || !same_optional_registered_object(
                retained.origin_identity.as_ref(),
                record.origin_identity.as_ref(),
            ) || record.repository_key != retained.repository_key
                || record.repository_root != retained.repository_root
            {
                return Err(invalid(
                    "new root observation differs from retained identity or owner",
                ));
            }
            if (retained.identity.is_none() || retained.origin_identity.is_none())
                && previous != &payload
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "unsupported filesystem identity permits only unchanged retained history",
                ));
            }
        }
        marker_matches(&record)?;
        let current_root = directory_identity(&root, &real_directory(&root)?)?;
        if !same_optional_registered_object(record.identity.as_ref(), current_root.as_ref()) {
            return Err(invalid("root changed during registration"));
        }
        match self.existing_payload {
            Some(previous) if previous == payload => {}
            Some(previous) => {
                let updated = transaction
                    .execute(
                        "UPDATE roots SET payload=?1 WHERE key=?2 AND payload=?3",
                        params![payload, record_key(&root), previous],
                    )
                    .map_err(invalid)?;
                if updated != 1 {
                    return Err(invalid("retained root history changed before update"));
                }
            }
            None => {
                transaction
                    .execute(
                        "INSERT INTO roots(key,payload) VALUES(?1,?2)",
                        params![record_key(&root), payload],
                    )
                    .map_err(invalid)?;
            }
        }
        transaction.commit().map_err(invalid)
    }
}

fn retained_registration(connection: &Connection, root: &Path) -> io::Result<Option<String>> {
    let payload: Option<Option<String>> = connection.query_row(
        "SELECT CASE WHEN length(CAST(payload AS BLOB)) <= 8192 THEN payload ELSE NULL END FROM roots WHERE key=?1",
        [record_key(root)], |row| row.get(0),
    ).optional().map_err(invalid)?;
    match payload {
        Some(Some(payload)) => Ok(Some(payload)),
        Some(None) => Err(invalid("oversized retained root history")),
        None => Ok(None),
    }
}

fn validate_retained_registration(root: &Path, payload: &str) -> io::Result<RootRecord> {
    let retained = decode(&record_key(root), payload)?;
    marker_matches(&retained)?;
    let current_root = directory_identity(root, &real_directory(root)?)?;
    if !same_optional_registered_object(retained.identity.as_ref(), current_root.as_ref()) {
        return Err(invalid(format!(
            "{}; retained history was preserved",
            changed_identity_reason("root", retained.identity.as_ref(), current_root.as_ref(),)
        )));
    }
    let current_origin = directory_identity(
        &retained.repository_root,
        &real_directory(&retained.repository_root)?,
    )?;
    if !same_optional_registered_object(retained.origin_identity.as_ref(), current_origin.as_ref())
    {
        return Err(invalid(format!(
            "{}; retained history was preserved",
            changed_identity_reason(
                "origin",
                retained.origin_identity.as_ref(),
                current_origin.as_ref(),
            )
        )));
    }
    Ok(retained)
}

// Legacy fixture helpers exercise the same pre-creation admission boundary.
#[cfg(test)]
pub(crate) fn register(
    host_database: &Path,
    root: &Path,
    marker: &WorktreeRootMarker,
    source: WorktreeRootSource,
) -> io::Result<()> {
    register_with_limit(host_database, root, marker, source, MAX_RECORDS)
}

#[cfg(test)]
fn register_with_limit(
    host_database: &Path,
    root: &Path,
    marker: &WorktreeRootMarker,
    source: WorktreeRootSource,
    limit: usize,
) -> io::Result<()> {
    admit_with_limit(host_database, root, limit)?.record(root, marker, source)
}

fn maintain_capacity(connection: &mut Connection, key: &str, limit: usize) -> io::Result<()> {
    let cursor: String = connection
        .query_row(
            "SELECT value FROM registry_meta WHERE key='prune_cursor'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(invalid)?
        .unwrap_or_default();
    let mut entries = page(connection, &cursor)?;
    if entries.is_empty() && !cursor.is_empty() {
        entries = page(connection, "")?;
    }
    let removable: Vec<_> = entries
        .iter()
        .filter(|(key, payload)| {
            decode(key, payload).is_ok_and(|old| {
                absent_on_original_filesystem(&old.root, old.identity.as_ref()).unwrap_or(false)
            })
        })
        .collect();
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(invalid)?;
    for (key, old_payload) in removable {
        let old = decode(key, old_payload)?;
        // Re-check absence under the writer lock, and compare the exact prior
        // payload so an upsert by a concurrent registrar cannot be pruned.
        if absent_on_original_filesystem(&old.root, old.identity.as_ref())? {
            transaction
                .execute(
                    "DELETE FROM roots WHERE key=?1 AND payload=?2",
                    params![key, old_payload],
                )
                .map_err(invalid)?;
        }
    }
    let count: usize = transaction
        .query_row("SELECT count(*) FROM roots", [], |row| row.get(0))
        .map_err(invalid)?;
    let exists: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM roots WHERE key=?1)",
            [key],
            |row| row.get(0),
        )
        .map_err(invalid)?;
    let next = entries.last().map_or("", |(key, _)| key.as_str());
    transaction.execute("INSERT INTO registry_meta(key,value) VALUES('prune_cursor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [next]).map_err(invalid)?;
    if count >= limit && !exists {
        // Persist maintenance progress even when this admission is denied.
        // Otherwise a full registry would retry its first page forever.
        transaction.commit().map_err(invalid)?;
        return Err(invalid(
            "worktree registry is full; retry bounded maintenance or inspect retained root history before admitting more storage",
        ));
    }
    transaction.commit().map_err(invalid)
}

#[derive(Debug, Default)]
pub(crate) struct Inventory {
    pub(crate) roots: Vec<RootRecord>,
    pub(crate) issues: Vec<String>,
    pub(crate) complete: bool,
}

pub(crate) fn inventory(host_database: &Path, deadline: Option<Instant>) -> Inventory {
    let mut result = Inventory {
        complete: true,
        ..Inventory::default()
    };
    let inspected = (|| -> io::Result<()> {
        let connection = open_registry(host_database, false)?;
        let mut cursor = String::new();
        let mut bytes = 0;
        let mut count = 0;
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(invalid("worktree history inspection budget expired"));
            }
            let entries = page(&connection, &cursor)?;
            if entries.is_empty() {
                break;
            }
            for (key, payload) in &entries {
                count += 1;
                bytes += payload.len();
                if count > MAX_RECORDS
                    || bytes > MAX_INVENTORY_BYTES
                    || deadline.is_some_and(|deadline| Instant::now() >= deadline)
                {
                    return Err(invalid("worktree history inspection budget expired"));
                }
                let record = decode(key, payload)?;
                match root_still_present(&record) {
                    Ok(true) => result.roots.push(record),
                    Ok(false) => {}
                    Err(error) => {
                        result.complete = false;
                        result.issues.push(format!(
                            "registered root {} uninspected: {error}",
                            record.root.display()
                        ));
                    }
                }
            }
            cursor = entries.last().expect("nonempty page").0.clone();
            if entries.len() < PAGE_RECORDS {
                break;
            }
        }
        Ok(())
    })();
    if let Err(error) = inspected {
        result.complete = false;
        result
            .issues
            .push(format!("worktree root history unavailable: {error}"));
    }
    result.roots.sort_by(|a, b| a.root.cmp(&b.root));
    result
}

/// Verify that a bound repository root still has its exact retained
/// registration. This is read-only: an absent or damaged record is a refusal,
/// never an invitation to recreate ownership from the current environment.
pub(crate) fn verify_exact_registration(
    host_database: &Path,
    root: &Path,
    marker: &WorktreeRootMarker,
    source: WorktreeRootSource,
) -> io::Result<()> {
    let registered_source = registered_source(host_database, root, marker)?;
    if registered_source != source {
        return Err(invalid("retained root source differs from binding"));
    }
    Ok(())
}

pub(crate) fn registered_source(
    host_database: &Path,
    root: &Path,
    marker: &WorktreeRootMarker,
) -> io::Result<WorktreeRootSource> {
    // A bound repository needs only its exact retained row. A host-wide
    // inventory here would make an unrelated damaged registration prevent
    // this repository from opening, even though the inventory remains
    // bounded and is still used for global discovery/GC.
    let connection = open_registry(host_database, false)?;
    let payload = retained_registration(&connection, root)?
        .ok_or_else(|| invalid("bound root has no retained registry record"))?;
    let record = validate_retained_registration(root, &payload)?;
    if record.root != root
        || record.repository_key != marker.repository_key
        || record.repository_root != marker.repository_root
    {
        return Err(invalid("retained root registration differs from marker"));
    }
    marker_owner_matches(&record, marker)?;
    Ok(record.source)
}

/// Opaque registry evidence retained while an artifact-use marker is
/// reconciled. The held registry lock prevents retained rows from changing
/// between plan revalidation and marker removal.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ArtifactUsePrimaryRootWitness {
    primary_root: PathBuf,
    repository_key: String,
    current_identity: DirectoryIdentity,
    rows: Vec<ArtifactUseRootRowWitness>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ArtifactUseRootRowWitness {
    root: PathBuf,
    payload_sha256: String,
    owner_marker_sha256: String,
    origin_identity: DirectoryIdentity,
}

pub(crate) struct ArtifactUsePrimaryRootWitnessGuard {
    host_database: PathBuf,
    _registry_lock: crate::file_lock::ExclusiveFileLock,
    witness: ArtifactUsePrimaryRootWitness,
}

impl ArtifactUsePrimaryRootWitnessGuard {
    pub(crate) fn witness(&self) -> &ArtifactUsePrimaryRootWitness {
        &self.witness
    }

    /// Re-read the exact registry rows and owner-marker bytes while retaining
    /// the same exclusive registry lock.
    pub(crate) fn revalidate(&self) -> io::Result<()> {
        let current = build_artifact_use_primary_root_witness(
            &self.host_database,
            &self.witness.primary_root,
            &self.witness.repository_key,
        )?;
        if current != self.witness {
            return Err(invalid(
                "primary-root registry witness changed during artifact-use reconciliation",
            ));
        }
        Ok(())
    }

    /// Accept the live primary identity or an exact mount-scoped primary
    /// identity saved by a validated registration for this same repository.
    /// The inode must match in both cases; an old device is accepted only
    /// through the stable identity rules of the retained origin record.
    pub(crate) fn matches_marker_identity(
        &self,
        primary_root: &Path,
        device: u64,
        inode: u64,
    ) -> bool {
        self.witness
            .matches_marker_identity(primary_root, device, inode)
    }
}

impl ArtifactUsePrimaryRootWitness {
    fn matches_marker_identity(&self, primary_root: &Path, device: u64, inode: u64) -> bool {
        if primary_root != self.primary_root || inode != self.current_identity.inode {
            return false;
        }
        if device == self.current_identity.device {
            return true;
        }
        self.rows.iter().any(|row| {
            row.origin_identity.device == device
                && row.origin_identity.inode == inode
                && row
                    .origin_identity
                    .same_registered_object(&self.current_identity)
        })
    }
}

/// Build a held, read-only witness for one exact repository primary root.
/// Every matching retained root row and owner marker is revalidated while the
/// host registry lock is held. A partial registry inventory is a refusal.
pub(crate) fn artifact_use_primary_root_witness_guard(
    host_database: &Path,
    primary_root: &Path,
    repository_key: &str,
) -> io::Result<ArtifactUsePrimaryRootWitnessGuard> {
    if repository_key.is_empty() {
        return Err(invalid("repository key is empty"));
    }
    let primary_root = primary_root.canonicalize()?;
    let lock = registry_lock(host_database, false)?;
    let witness =
        build_artifact_use_primary_root_witness(host_database, &primary_root, repository_key)?;
    Ok(ArtifactUsePrimaryRootWitnessGuard {
        host_database: host_database.to_path_buf(),
        _registry_lock: lock,
        witness,
    })
}

fn build_artifact_use_primary_root_witness(
    host_database: &Path,
    primary_root: &Path,
    repository_key: &str,
) -> io::Result<ArtifactUsePrimaryRootWitness> {
    let primary_root = primary_root.canonicalize()?;
    let metadata = real_directory(&primary_root)?;
    let current_identity = directory_identity(&primary_root, &metadata)?
        .ok_or_else(|| invalid("primary root filesystem identity is unavailable"))?;
    let inventory = inventory(host_database, None);
    if !inventory.complete {
        return Err(invalid(format!(
            "worktree registry inventory is incomplete: {}",
            inventory.issues.join("; ")
        )));
    }
    let connection = open_registry(host_database, false)?;

    let mut rows = Vec::new();
    for candidate in inventory.roots.iter().filter(|record| {
        record.repository_key == repository_key && record.repository_root == primary_root
    }) {
        let payload = retained_registration(&connection, &candidate.root)?
            .ok_or_else(|| invalid("matching retained root row disappeared under registry lock"))?;
        let record = validate_retained_registration(&candidate.root, &payload)?;
        if &record != candidate
            || record.repository_key != repository_key
            || record.repository_root != primary_root
        {
            return Err(invalid(
                "retained root registration changed repository binding",
            ));
        }
        let origin_identity = record
            .origin_identity
            .clone()
            .ok_or_else(|| invalid("retained root has no origin identity"))?;
        if !origin_identity.same_registered_object(&current_identity) {
            return Err(invalid(
                "retained root origin does not match the current primary identity",
            ));
        }
        let (marker, marker_bytes) = read_marker_with_payload(&record.root)?;
        marker_owner_matches(&record, &marker)?;
        rows.push(ArtifactUseRootRowWitness {
            root: record.root,
            payload_sha256: format!("{:x}", Sha256::digest(payload.as_bytes())),
            owner_marker_sha256: format!("{:x}", Sha256::digest(&marker_bytes)),
            origin_identity,
        });
    }
    rows.sort_by(|left, right| left.root.cmp(&right.root));
    Ok(ArtifactUsePrimaryRootWitness {
        primary_root,
        repository_key: repository_key.to_owned(),
        current_identity,
        rows,
    })
}

pub(crate) fn ownership_fingerprint(record: &RootRecord) -> io::Result<String> {
    if !root_still_present(record)? || !origin_is_missing(record)? {
        return Err(invalid("root is no longer an orphan"));
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(record).map_err(invalid)?)
    ))
}

#[cfg(test)]
fn revalidate(host_database: &Path, root: &Path, expected: &str) -> io::Result<()> {
    let connection = open_registry(host_database, false)?;
    let key = record_key(root);
    let payload: Option<String> = connection.query_row("SELECT CASE WHEN length(CAST(payload AS BLOB)) <= 8192 THEN payload ELSE NULL END FROM roots WHERE key=?1", [&key], |row| row.get(0)).optional().map_err(invalid)?.flatten();
    let record = decode(
        &key,
        &payload.ok_or_else(|| invalid("root no longer registered"))?,
    )?;
    if ownership_fingerprint(&record)? != expected {
        return Err(invalid("registered orphan evidence changed"));
    }
    Ok(())
}

pub(crate) struct RemovalClaim {
    _lock: crate::file_lock::ExclusiveFileLock,
    pub(crate) already_absent: bool,
    tree: Option<crate::gc::PinnedTreeRemoval>,
    record: RootRecord,
}

impl std::fmt::Debug for RemovalClaim {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemovalClaim")
            .field("already_absent", &self.already_absent)
            .finish_non_exhaustive()
    }
}

impl RemovalClaim {
    /// Delete through the directory handle whose identity this claim proved.
    /// Keep this claim alive through the caller's durable journal update.
    pub(crate) fn remove(
        &mut self,
        deadline: Option<Instant>,
    ) -> io::Result<crate::gc::TreeRemoval> {
        if self.already_absent {
            return Ok(crate::gc::TreeRemoval::Complete);
        }
        let record = &self.record;
        self.tree
            .take()
            .ok_or_else(|| invalid("removal claim has already consumed its directory handle"))?
            .remove_verified_witness(WORKTREE_ROOT_MARKER, deadline, |bytes| {
                marker_owner_matches(record, &decode_marker(bytes)?)?;
                if !origin_is_missing(record)? {
                    return Err(invalid("owning repository reappeared before deletion"));
                }
                Ok(())
            })
    }
}

#[derive(Debug)]
pub(crate) enum ClaimError {
    /// The old authorization has conclusively lost its ownership premise.
    Revoked(String),
    /// The root is retained; a fresh plan may not rediscover missing history.
    HistoryLost(String),
    /// Inspection or coordination could not finish. Keep durable work pending.
    Unavailable(io::Error),
}

impl From<io::Error> for ClaimError {
    fn from(error: io::Error) -> Self {
        Self::Unavailable(error)
    }
}

pub(crate) fn claim_for_removal(
    host_database: &Path,
    root: &Path,
    expected: &str,
) -> Result<RemovalClaim, ClaimError> {
    registry_outside_root(host_database, root)?;
    let claim = registry_lock(host_database, false)?;
    let connection = open_registry(host_database, false)?;
    let key = record_key(root);
    let payload: Option<String> = connection.query_row("SELECT CASE WHEN length(CAST(payload AS BLOB)) <= 8192 THEN payload ELSE NULL END FROM roots WHERE key=?1", [&key], |row| row.get(0)).optional().map_err(invalid)?.flatten();
    let payload = payload.ok_or_else(|| {
        ClaimError::HistoryLost("registered ownership record is missing or oversized".into())
    })?;
    let record =
        decode(&key, &payload).map_err(|error| ClaimError::HistoryLost(error.to_string()))?;
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&record).map_err(invalid)?)
    );
    if fingerprint != expected {
        return Err(ClaimError::Revoked(
            "registered ownership record changed".into(),
        ));
    }
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if absent_on_original_filesystem(root, record.identity.as_ref())? {
                return Ok(RemovalClaim {
                    _lock: claim,
                    already_absent: true,
                    tree: None,
                    record,
                });
            }
            return Err(ClaimError::Unavailable(invalid(
                "root absence could not be proved",
            )));
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ClaimError::Revoked(
            "registered root was replaced by another object".into(),
        ));
    }
    if record.identity.is_none() {
        return Err(invalid("no supported root filesystem identity").into());
    }
    let current_identity = directory_identity(root, &metadata)?;
    if !same_optional_registered_object(record.identity.as_ref(), current_identity.as_ref()) {
        return Err(ClaimError::Revoked(changed_identity_reason(
            "root",
            record.identity.as_ref(),
            current_identity.as_ref(),
        )));
    }
    if let Err(error) = marker_matches(&record) {
        return Err(match error.kind() {
            io::ErrorKind::InvalidData | io::ErrorKind::NotFound => {
                ClaimError::Revoked(format!("owner marker changed: {error}"))
            }
            _ => ClaimError::Unavailable(error),
        });
    }
    if !origin_is_missing(&record)? {
        return Err(ClaimError::Revoked("owning repository reappeared".into()));
    }
    let tree = crate::gc::PinnedTreeRemoval::open(root)?;
    let pinned: crate::gc::TreeObjectIdentity = tree.object_identity()?;
    let current = current_identity
        .as_ref()
        .ok_or_else(|| invalid("no supported current root filesystem identity"))?;
    if pinned.device != current.device
        || pinned.inode != current.inode
        || tree.filesystem_identity()? != current.filesystem
    {
        return Err(ClaimError::Revoked(
            "registered root changed before its removal handle was pinned".into(),
        ));
    }
    Ok(RemovalClaim {
        _lock: claim,
        already_absent: false,
        tree: Some(tree),
        record,
    })
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn artifact_use_witness_accepts_only_exact_validated_mount_history() {
        let current_identity = DirectoryIdentity {
            device: 16_777_233,
            inode: 20_093_191,
            filesystem: vec![1, 2, 3],
            volume_uuid: Some([7; 16]),
        };
        let saved_origin = DirectoryIdentity {
            device: 16_777_230,
            inode: 20_093_191,
            filesystem: vec![9, 8, 7],
            volume_uuid: Some([7; 16]),
        };
        let witness = ArtifactUsePrimaryRootWitness {
            primary_root: PathBuf::from("/fixture/primary"),
            repository_key: "fixture-repository".into(),
            current_identity: current_identity.clone(),
            rows: vec![ArtifactUseRootRowWitness {
                root: PathBuf::from("/fixture/worktree"),
                payload_sha256: "payload".into(),
                owner_marker_sha256: "owner".into(),
                origin_identity: saved_origin,
            }],
        };
        assert!(witness.matches_marker_identity(
            Path::new("/fixture/primary"),
            16_777_233,
            20_093_191,
        ));
        assert!(witness.matches_marker_identity(
            Path::new("/fixture/primary"),
            16_777_230,
            20_093_191,
        ));
        assert!(!witness.matches_marker_identity(
            Path::new("/fixture/primary"),
            16_777_234,
            20_093_191,
        ));
        assert!(!witness.matches_marker_identity(
            Path::new("/fixture/primary"),
            16_777_230,
            20_093_192,
        ));
        let mut changed_volume = witness;
        changed_volume.current_identity.volume_uuid = Some([8; 16]);
        assert!(!changed_volume.matches_marker_identity(
            Path::new("/fixture/primary"),
            16_777_230,
            20_093_191,
        ));
    }

    #[test]
    fn artifact_use_witness_requires_exact_linux_identity_without_uuid() {
        let current_identity = DirectoryIdentity {
            device: 31,
            inode: 47,
            filesystem: vec![1, 2, 3],
            volume_uuid: None,
        };
        let witness = ArtifactUsePrimaryRootWitness {
            primary_root: PathBuf::from("/fixture/primary"),
            repository_key: "fixture-repository".into(),
            current_identity: current_identity.clone(),
            rows: vec![ArtifactUseRootRowWitness {
                root: PathBuf::from("/fixture/worktree"),
                payload_sha256: "payload".into(),
                owner_marker_sha256: "owner".into(),
                origin_identity: current_identity,
            }],
        };
        assert!(witness.matches_marker_identity(Path::new("/fixture/primary"), 31, 47,));
        assert!(!witness.matches_marker_identity(Path::new("/fixture/primary"), 30, 47,));
        assert!(!witness.matches_marker_identity(Path::new("/fixture/primary"), 31, 48,));
    }

    #[test]
    fn artifact_use_registry_guard_revalidates_retained_owner_marker_bytes() {
        let fixture = Fixture::new();
        let (root, mut marker) = fixture.root("registered-root");
        marker.repository_root = fixture.origin.canonicalize().unwrap();
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        fixture.register(&root, &marker);
        let guard = artifact_use_primary_root_witness_guard(
            &fixture.database,
            &marker.repository_root,
            &marker.repository_key,
        )
        .unwrap();
        let metadata = real_directory(&marker.repository_root).unwrap();
        let identity = directory_identity(&marker.repository_root, &metadata)
            .unwrap()
            .unwrap();
        assert!(guard.matches_marker_identity(
            &marker.repository_root,
            identity.device,
            identity.inode,
        ));

        marker.repository_key = "changed-owner".into();
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        assert!(guard.revalidate().is_err());
    }

    thread_local! {
        static REGISTRATION_CONTENTION: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn observe_registration_contention() {
        REGISTRATION_CONTENTION.with(|observer| {
            if let Some(sender) = observer.borrow_mut().take() {
                sender
                    .send(())
                    .expect("contention observer must remain alive");
            }
        });
    }

    struct Fixture {
        _owned: tempfile::TempDir,
        database: PathBuf,
        origin: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let owned = tempfile::tempdir().unwrap();
            let database = owned.path().join("private-state/host-operations.db");
            let origin = owned.path().join("origin");
            fs::create_dir(&origin).unwrap();
            Self {
                _owned: owned,
                database,
                origin,
            }
        }

        fn root(&self, name: &str) -> (PathBuf, WorktreeRootMarker) {
            let root = self._owned.path().join(name);
            fs::create_dir_all(&root).unwrap();
            let marker = WorktreeRootMarker {
                schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
                repository_key: name.replace('/', "-"),
                repository_root: self.origin.clone(),
            };
            fs::write(
                root.join(WORKTREE_ROOT_MARKER),
                serde_json::to_vec(&marker).unwrap(),
            )
            .unwrap();
            (root.canonicalize().unwrap(), marker)
        }

        fn register(&self, root: &Path, marker: &WorktreeRootMarker) {
            register(
                &self.database,
                root,
                marker,
                WorktreeRootSource::LibraryOverride,
            )
            .unwrap();
        }
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct LegacyDirectoryIdentity {
        device: u64,
        inode: u64,
        filesystem: Vec<u8>,
    }

    fn current_recovery_identity(
        _path: &Path,
        metadata: &Metadata,
    ) -> io::Result<Option<DirectoryIdentity>> {
        use std::os::unix::fs::MetadataExt;
        Ok(Some(DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            filesystem: vec![0x5a; 8],
            volume_uuid: Some([0x5a; 16]),
        }))
    }

    fn retain_legacy_registration(fixture: &Fixture, root: &Path) -> String {
        let mut record = inventory(&fixture.database, None)
            .roots
            .into_iter()
            .find(|record| record.root == root)
            .expect("fixture root should have a registration");
        record.identity.as_mut().unwrap().volume_uuid = None;
        record.origin_identity.as_mut().unwrap().volume_uuid = None;
        let payload = serde_json::to_string(&record).unwrap();
        let changed = open_registry(&fixture.database, true)
            .unwrap()
            .execute(
                "UPDATE roots SET payload=?1 WHERE key=?2",
                params![payload, record_key(root)],
            )
            .unwrap();
        assert_eq!(changed, 1);
        payload
    }

    fn plan_recovery(
        fixture: &Fixture,
        root: &Path,
        owner: &WorktreeRootMarker,
    ) -> RootIdentityRecoveryPlan {
        plan_root_identity_recovery_with(
            &fixture.database,
            root,
            &owner.repository_root,
            &owner.repository_key,
            &current_recovery_identity,
        )
        .unwrap()
    }

    #[test]
    fn exact_historical_roots_survive_placement_changes_and_owner_disappearance() {
        let fixture = Fixture::new();
        let (first, a) = fixture.root("old-placement/exact-root");
        let (second, b) = fixture.root("new-placement/different-root");
        fixture.register(&first, &a);
        fixture.register(&second, &b);
        fs::remove_dir(&fixture.origin).unwrap();
        let found = inventory(&fixture.database, None);
        assert!(found.complete, "{:?}", found.issues);
        assert_eq!(found.roots.len(), 2);
        let record = found
            .roots
            .iter()
            .find(|record| record.root == first)
            .unwrap();
        let fingerprint = ownership_fingerprint(record).unwrap();
        revalidate(&fixture.database, &first, &fingerprint).unwrap();
        assert!(
            registry_path(&fixture.database)
                .unwrap()
                .starts_with(fixture._owned.path())
        );
        // A restored origin revokes the saved deletion proof.
        fs::create_dir(&fixture.origin).unwrap();
        assert!(revalidate(&fixture.database, &first, &fingerprint).is_err());
        assert!(first.is_dir());
        assert!(second.is_dir());
    }

    #[test]
    fn replacing_a_claimed_root_cannot_redirect_removal_to_the_new_directory() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fs::write(root.join("original.txt"), "keep original").unwrap();
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        let mut claim = claim_for_removal(&fixture.database, &root, &fingerprint).unwrap();

        let retained = root.with_extension("retained");
        fs::rename(&root, &retained).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("replacement.txt"), "keep replacement").unwrap();
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();

        assert!(claim.remove(None).is_err());
        assert_eq!(
            fs::read_to_string(retained.join("original.txt")).unwrap(),
            "keep original"
        );
        assert_eq!(
            fs::read_to_string(root.join("replacement.txt")).unwrap(),
            "keep replacement"
        );
        assert!(retained.join(WORKTREE_ROOT_MARKER).is_file());
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
        assert!(
            claim.remove(None).is_err(),
            "a consumed claim cannot be replayed"
        );
    }

    #[test]
    fn full_history_refuses_admission_before_creating_the_planned_root() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("existing");
        register_with_limit(
            &fixture.database,
            &root,
            &owner,
            WorktreeRootSource::LibraryOverride,
            1,
        )
        .unwrap();
        let planned = fixture._owned.path().join("must-not-create");
        let error = admit_with_limit(&fixture.database, &planned, 1)
            .err()
            .expect("full history must refuse");
        assert!(error.to_string().contains("registry is full"));
        assert!(!planned.exists());
        let existing = admit_with_limit(&fixture.database, &root, 1).unwrap();
        existing
            .record(&root, &owner, WorktreeRootSource::LibraryOverride)
            .unwrap();
        assert_eq!(inventory(&fixture.database, None).roots.len(), 1);
    }

    #[test]
    fn admission_holds_exclusion_until_the_exact_created_root_is_recorded() {
        let fixture = Fixture::new();
        let planned = fixture._owned.path().join("admitted");
        let admission = admit_with_limit(&fixture.database, &planned, 1).unwrap();
        assert!(
            !planned.exists(),
            "admission must not create its checkout root"
        );
        let contender = fixture._owned.path().join("contender");
        let error = admit_with_limit_and_wait(&fixture.database, &contender, 1, Duration::ZERO)
            .err()
            .expect("exclusion must stay held");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        let (root, owner) = fixture.root("admitted");
        admission
            .record(&root, &owner, WorktreeRootSource::LibraryOverride)
            .unwrap();
        let error = admit_with_limit(&fixture.database, &contender, 1)
            .err()
            .expect("the recorded root consumes capacity");
        assert!(error.to_string().contains("registry is full"));
        assert!(!contender.exists());
    }

    #[test]
    fn foreground_admission_waits_for_transient_history_contention() {
        use std::sync::mpsc;

        let fixture = Fixture::new();
        let planned = fixture._owned.path().join("waiting-admission");
        let held = registry_lock(&fixture.database, true).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let database = &fixture.database;
            let planned = &planned;
            scope.spawn(move || {
                REGISTRATION_CONTENTION.with(|observer| {
                    assert!(observer.borrow_mut().replace(started_tx).is_none());
                });
                let result = admit(database, planned)
                    .map(drop)
                    .map_err(|error| error.to_string());
                result_tx.send(result).unwrap();
            });
            // This signal comes only after the public admission path actually
            // observed WouldBlock, so scheduler delay cannot mimic waiting.
            let blocked = started_rx.recv_timeout(Duration::from_secs(2));
            // Release before asserting so a failed assertion cannot keep a
            // foreground waiter blocked while the scoped thread is joined.
            drop(held);
            assert!(
                blocked.is_ok(),
                "foreground admission did not observe contention and wait: {blocked:?}"
            );
            result_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .expect("foreground admission must retry transient contention");
        });
        assert!(
            !planned.exists(),
            "admission alone must not create the root"
        );
    }

    #[test]
    fn foreground_contention_timeout_preserves_lock_and_storage() {
        let fixture = Fixture::new();
        let planned = fixture._owned.path().join("still-blocked");
        let held = registry_lock(&fixture.database, true).unwrap();
        let started = Instant::now();
        let wait = Duration::from_millis(20);
        let error = admit_with_limit_and_wait(&fixture.database, &planned, MAX_RECORDS, wait)
            .err()
            .expect("held history lock must time out");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= wait,
            "registration refused before its wait expired"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "registration wait was not bounded"
        );
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(error.to_string().contains("bounded registration wait"));
        assert!(registry_lock(&fixture.database, true).is_err());
        assert!(!planned.exists());
        drop(held);
        assert!(admit(&fixture.database, &planned).is_ok());
    }

    #[test]
    fn stable_volume_uuid_and_inode_survive_mount_identity_changes() {
        let uuid = [7; 16];
        let saved = DirectoryIdentity {
            device: 11,
            inode: 23,
            filesystem: vec![1, 2, 3],
            volume_uuid: Some(uuid),
        };
        let remounted = DirectoryIdentity {
            device: 12,
            inode: 23,
            filesystem: vec![4, 5, 6],
            volume_uuid: Some(uuid),
        };
        assert!(saved.same_registered_object(&remounted));
        assert!(saved.same_registered_filesystem(&remounted));

        let replacement_volume = DirectoryIdentity {
            volume_uuid: Some([8; 16]),
            ..remounted.clone()
        };
        assert!(!saved.same_registered_object(&replacement_volume));
        assert!(!saved.same_registered_filesystem(&replacement_volume));

        let replacement_inode = DirectoryIdentity {
            inode: 24,
            ..remounted
        };
        assert!(!saved.same_registered_object(&replacement_inode));
    }

    #[test]
    fn legacy_identity_upgrades_only_when_saved_mount_identity_matches_live() {
        let legacy = DirectoryIdentity {
            device: 11,
            inode: 23,
            filesystem: vec![1, 2, 3],
            volume_uuid: None,
        };
        let current_with_uuid = DirectoryIdentity {
            volume_uuid: Some([7; 16]),
            ..legacy.clone()
        };
        assert!(legacy.same_registered_object(&current_with_uuid));
        assert!(legacy.same_registered_filesystem(&current_with_uuid));
        assert!(!legacy_uuid_recovery_needed(
            Some(&legacy),
            Some(&current_with_uuid)
        ));
        assert!(!current_with_uuid.same_registered_object(&legacy));

        let remounted_with_uuid = DirectoryIdentity {
            device: 12,
            filesystem: vec![4, 5, 6],
            ..current_with_uuid.clone()
        };
        assert!(!legacy.same_registered_object(&remounted_with_uuid));
        assert!(!legacy.same_registered_filesystem(&remounted_with_uuid));
        assert!(legacy_uuid_recovery_needed(
            Some(&legacy),
            Some(&remounted_with_uuid)
        ));

        let unchanged_legacy = DirectoryIdentity {
            volume_uuid: None,
            ..current_with_uuid
        };
        assert!(legacy.same_registered_object(&unchanged_legacy));
    }

    #[test]
    fn volume_uuid_response_rejects_wrong_length_and_nil_uuid() {
        let uuid = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let mut valid = Vec::from(20_u32.to_ne_bytes());
        valid.extend_from_slice(&uuid);
        assert_eq!(decode_volume_uuid_response(&valid).unwrap(), uuid);

        let mut nil = Vec::from(20_u32.to_ne_bytes());
        nil.extend_from_slice(&[0; 16]);
        assert!(decode_volume_uuid_response(&nil).is_err());

        let mut wrong_length = valid.clone();
        wrong_length[..4].copy_from_slice(&19_u32.to_ne_bytes());
        assert!(decode_volume_uuid_response(&wrong_length).is_err());
        assert!(decode_volume_uuid_response(&valid[..19]).is_err());
    }

    #[test]
    fn pre_uuid_decoder_rejects_new_payload_and_admission_preserves_it() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let mut record = inventory(&fixture.database, None).roots.remove(0);
        record.identity.as_mut().unwrap().volume_uuid = Some([0x31; 16]);
        record.origin_identity.as_mut().unwrap().volume_uuid = Some([0x32; 16]);
        let stable_payload = serde_json::to_string(&record).unwrap();

        // This is the strict pre-UUID decoder: the actual current payload is
        // refused because the old schema rejects unknown identity fields.
        let legacy_decoder_error = serde_json::from_str::<LegacyDirectoryIdentity>(
            &serde_json::to_string(record.identity.as_ref().unwrap()).unwrap(),
        )
        .expect_err("pre-UUID decoder must reject a new identity payload");
        assert!(legacy_decoder_error.to_string().contains("unknown field"));

        let changed = open_registry(&fixture.database, true)
            .unwrap()
            .execute(
                "UPDATE roots SET payload=?1 WHERE key=?2",
                params![stable_payload, record_key(&root)],
            )
            .unwrap();
        assert_eq!(changed, 1);
        assert!(admit(&fixture.database, &root).is_err());
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(stable_payload),
            "an old-shaped admission cannot downgrade or replace a UUID row"
        );
    }

    #[test]
    fn reviewed_identity_recovery_applies_once_and_refuses_confirmation_replay() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let legacy_payload = retain_legacy_registration(&fixture, &root);

        // Production lifecycle and storage attribution writers use pretty
        // JSON followed by exactly one newline.
        let mut production_marker = serde_json::to_vec_pretty(&owner).unwrap();
        production_marker.push(b'\n');
        fs::write(root.join(WORKTREE_ROOT_MARKER), &production_marker).unwrap();

        let plan = plan_recovery(&fixture, &root, &owner);
        assert!(!plan.historical_uuid_proven);
        assert!(!plan.applied);
        assert_eq!(plan.root_volume_uuid_hex, "5a".repeat(16));
        assert_eq!(plan.origin_volume_uuid_hex, "5a".repeat(16));
        assert_eq!(plan.old_payload_sha256.len(), 64);
        assert_eq!(plan.new_payload_sha256.len(), 64);
        assert_eq!(plan.owner_marker_sha256.len(), 64);
        assert_eq!(
            plan.owner_marker_sha256,
            format!("{:x}", Sha256::digest(&production_marker))
        );
        assert_eq!(
            plan.confirmation,
            format!(
                "{}:{}:{}",
                plan.old_payload_sha256, plan.new_payload_sha256, plan.owner_marker_sha256
            )
        );

        let applied = apply_root_identity_recovery_with(
            &fixture.database,
            &root,
            &owner.repository_root,
            &owner.repository_key,
            &plan.confirmation,
            &current_recovery_identity,
            || Ok(()),
        )
        .unwrap();
        assert!(applied.applied);
        assert_eq!(applied.confirmation, plan.confirmation);
        let upgraded_payload =
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap()
                .unwrap();
        assert_ne!(upgraded_payload, legacy_payload);
        let upgraded = decode(&record_key(&root), &upgraded_payload).unwrap();
        assert_eq!(upgraded.identity.unwrap().volume_uuid, Some([0x5a; 16]));
        assert_eq!(
            upgraded.origin_identity.unwrap().volume_uuid,
            Some([0x5a; 16])
        );

        assert!(
            apply_root_identity_recovery_with(
                &fixture.database,
                &root,
                &owner.repository_root,
                &owner.repository_key,
                &plan.confirmation,
                &current_recovery_identity,
                || Ok(()),
            )
            .is_err()
        );
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(upgraded_payload),
            "replayed confirmation must leave the already-upgraded row intact"
        );
    }

    #[test]
    fn recovery_confirmation_refuses_owner_marker_reencoding_between_plan_and_apply() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let legacy_payload = retain_legacy_registration(&fixture, &root);

        let plan = plan_recovery(&fixture, &root, &owner);
        let mut production_marker = serde_json::to_vec_pretty(&owner).unwrap();
        production_marker.push(b'\n');
        fs::write(root.join(WORKTREE_ROOT_MARKER), production_marker).unwrap();

        let error = apply_root_identity_recovery_with(
            &fixture.database,
            &root,
            &owner.repository_root,
            &owner.repository_key,
            &plan.confirmation,
            &current_recovery_identity,
            || Ok(()),
        )
        .expect_err("confirmation must pin exact owner marker bytes across plan/apply");

        assert!(
            error
                .to_string()
                .contains("old:new:owner-marker confirmation")
        );
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(legacy_payload),
            "a marker re-encoding must leave retained history unchanged"
        );
    }

    #[test]
    fn recovery_rolls_back_when_owner_marker_bytes_change_after_cas() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let legacy_payload = retain_legacy_registration(&fixture, &root);
        let plan = plan_recovery(&fixture, &root, &owner);

        let error = apply_root_identity_recovery_with_hooks(
            &fixture.database,
            &root,
            &owner.repository_root,
            &owner.repository_key,
            &plan.confirmation,
            &current_recovery_identity,
            RootIdentityRecoveryHooks {
                before_compare_and_swap: || Ok(()),
                after_update_before_reobservation: || {
                    let mut production_marker =
                        serde_json::to_vec_pretty(&owner).map_err(invalid)?;
                    production_marker.push(b'\n');
                    fs::write(root.join(WORKTREE_ROOT_MARKER), production_marker)?;
                    Ok(())
                },
            },
        )
        .expect_err("post-CAS reobservation must reject changed owner marker bytes");

        assert!(
            error
                .to_string()
                .contains("owner marker bytes changed during recovery"),
            "{error}"
        );
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(legacy_payload),
            "a failed post-CAS marker check must roll back the upgraded row"
        );
    }

    #[test]
    fn reviewed_identity_recovery_requires_two_nonzero_live_uuids() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let legacy_payload = retain_legacy_registration(&fixture, &root);
        for nil_path in [root.as_path(), owner.repository_root.as_path()] {
            let nil_identity = |path: &Path, metadata: &Metadata| {
                let mut identity = current_recovery_identity(path, metadata)?.unwrap();
                if path == nil_path {
                    identity.volume_uuid = Some([0; 16]);
                }
                Ok(Some(identity))
            };
            let error = plan_root_identity_recovery_with(
                &fixture.database,
                &root,
                &owner.repository_root,
                &owner.repository_key,
                &nil_identity,
            )
            .unwrap_err();
            assert!(error.to_string().contains("nil volume UUID"));
        }
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(legacy_payload)
        );
    }

    #[test]
    fn recovery_cannot_update_history_while_removal_holds_the_registry_lock() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let legacy_payload = retain_legacy_registration(&fixture, &root);
        let plan = plan_recovery(&fixture, &root, &owner);
        let removal_claim = registry_lock(&fixture.database, false).unwrap();

        let error = apply_root_identity_recovery_with(
            &fixture.database,
            &root,
            &owner.repository_root,
            &owner.repository_key,
            &plan.confirmation,
            &current_recovery_identity,
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        drop(removal_claim);
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(legacy_payload)
        );
    }

    #[test]
    fn reviewed_recovery_rolls_back_when_root_changes_after_initial_observation() {
        use std::os::unix::fs::MetadataExt;

        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let legacy_payload = retain_legacy_registration(&fixture, &root);
        let plan = plan_recovery(&fixture, &root, &owner);
        let retained = root.with_extension("retained");

        let error = apply_root_identity_recovery_with(
            &fixture.database,
            &root,
            &owner.repository_root,
            &owner.repository_key,
            &plan.confirmation,
            &current_recovery_identity,
            || {
                fs::rename(&root, &retained)?;
                fs::create_dir(&root)?;
                fs::write(
                    root.join(WORKTREE_ROOT_MARKER),
                    serde_json::to_vec(&owner).unwrap(),
                )?;
                Ok(())
            },
        )
        .expect_err("post-update reobservation must refuse a replaced root");

        assert!(
            error.to_string().contains("registered root inode changed"),
            "{error}"
        );
        assert_ne!(
            fs::metadata(&root).unwrap().ino(),
            plan.root_inode,
            "the injected replacement must have a different live inode"
        );
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(legacy_payload),
            "a failed post-update reobservation must roll back the upgraded row"
        );
        assert!(retained.join(WORKTREE_ROOT_MARKER).is_file());
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
    }

    #[test]
    fn reviewed_recovery_refuses_changed_root_inode_and_preserves_history() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let legacy_payload = retain_legacy_registration(&fixture, &root);
        let plan = plan_recovery(&fixture, &root, &owner);

        let retained = root.with_extension("retained");
        fs::rename(&root, &retained).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();

        assert!(
            apply_root_identity_recovery_with(
                &fixture.database,
                &root,
                &owner.repository_root,
                &owner.repository_key,
                &plan.confirmation,
                &current_recovery_identity,
                || Ok(()),
            )
            .is_err()
        );
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(legacy_payload)
        );
        assert!(retained.join(WORKTREE_ROOT_MARKER).is_file());
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
    }

    #[test]
    fn reviewed_recovery_refuses_changed_origin_inode_and_preserves_history() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let legacy_payload = retain_legacy_registration(&fixture, &root);
        let plan = plan_recovery(&fixture, &root, &owner);

        let retained = fixture.origin.with_extension("retained");
        fs::rename(&fixture.origin, &retained).unwrap();
        fs::create_dir(&fixture.origin).unwrap();

        assert!(
            apply_root_identity_recovery_with(
                &fixture.database,
                &root,
                &owner.repository_root,
                &owner.repository_key,
                &plan.confirmation,
                &current_recovery_identity,
                || Ok(()),
            )
            .is_err()
        );
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(legacy_payload)
        );
        assert!(retained.is_dir());
        assert!(fixture.origin.is_dir());
    }

    #[test]
    fn reviewed_recovery_refuses_changed_owner_marker_payload_and_preserves_history() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let legacy_payload = retain_legacy_registration(&fixture, &root);
        let plan = plan_recovery(&fixture, &root, &owner);
        let mut changed_marker_payload = serde_json::to_vec(&owner).unwrap();
        changed_marker_payload.push(b' ');
        fs::write(root.join(WORKTREE_ROOT_MARKER), changed_marker_payload).unwrap();

        assert!(
            apply_root_identity_recovery_with(
                &fixture.database,
                &root,
                &owner.repository_root,
                &owner.repository_key,
                &plan.confirmation,
                &current_recovery_identity,
                || Ok(()),
            )
            .is_err()
        );
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(legacy_payload)
        );
    }

    #[test]
    fn concurrent_writer_cannot_be_overwritten_by_identity_recovery_cas() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let legacy_payload = retain_legacy_registration(&fixture, &root);
        let plan = plan_recovery(&fixture, &root, &owner);
        let competing_payload = format!("{legacy_payload} ");

        assert!(
            apply_root_identity_recovery_with(
                &fixture.database,
                &root,
                &owner.repository_root,
                &owner.repository_key,
                &plan.confirmation,
                &current_recovery_identity,
                || {
                    let connection = open_existing_registry_for_write(&fixture.database)?;
                    let changed = connection
                        .execute(
                            "UPDATE roots SET payload=?1 WHERE key=?2",
                            params![competing_payload, record_key(&root)],
                        )
                        .map_err(invalid)?;
                    assert_eq!(changed, 1);
                    Ok(())
                },
            )
            .is_err()
        );
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(competing_payload),
            "the exact CAS must preserve the competing row"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn directory_identity_reads_the_filesystem_uuid() {
        let fixture = Fixture::new();
        let identity =
            directory_identity(&fixture.origin, &real_directory(&fixture.origin).unwrap())
                .unwrap()
                .expect("macOS filesystem identity is supported");
        assert!(identity.volume_uuid.is_some());
    }

    fn mutate_filesystem_identity(identity: &mut DirectoryIdentity) {
        if let Some(uuid) = identity.volume_uuid.as_mut() {
            uuid[0] ^= 1;
        } else {
            identity.filesystem[0] ^= 1;
        }
    }

    #[test]
    fn orphan_removal_yields_while_foreground_admission_owns_history() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("orphan");
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        let planned = fixture._owned.path().join("foreground");
        let held = admit(&fixture.database, &planned).unwrap();
        assert!(matches!(
            claim_for_removal(&fixture.database, &root, &fingerprint),
            Err(ClaimError::Unavailable(error)) if error.kind() == io::ErrorKind::WouldBlock
        ));
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
        assert!(!planned.exists());
        drop(held);
        assert!(claim_for_removal(&fixture.database, &root, &fingerprint).is_ok());
    }

    #[test]
    fn admission_cannot_record_a_different_root() {
        let fixture = Fixture::new();
        let planned = fixture._owned.path().join("planned");
        let admission = admit_with_limit(&fixture.database, &planned, 1).unwrap();
        let (other, owner) = fixture.root("other");
        fs::write(other.join("preserve.txt"), "unrelated data").unwrap();
        assert!(
            admission
                .record(&other, &owner, WorktreeRootSource::LibraryOverride)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(other.join("preserve.txt")).unwrap(),
            "unrelated data"
        );
        assert!(!planned.exists());
        assert!(inventory(&fixture.database, None).roots.is_empty());
    }

    #[test]
    fn same_path_root_replacement_preserves_the_previous_filesystem_history() {
        for replace_after_admission in [false, true] {
            let fixture = Fixture::new();
            let (root, owner) = fixture.root("root");
            fs::write(root.join("original.txt"), "preserve original").unwrap();
            fixture.register(&root, &owner);
            let previous =
                retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                    .unwrap();
            let admission =
                replace_after_admission.then(|| admit(&fixture.database, &root).unwrap());
            let held = root.with_extension("held");
            fs::rename(&root, &held).unwrap();
            fs::create_dir(&root).unwrap();
            fs::write(
                root.join(WORKTREE_ROOT_MARKER),
                serde_json::to_vec(&owner).unwrap(),
            )
            .unwrap();
            fs::write(root.join("replacement.txt"), "preserve replacement").unwrap();
            if let Some(admission) = admission {
                assert!(
                    admission
                        .record(&root, &owner, WorktreeRootSource::LibraryOverride)
                        .is_err()
                );
            } else {
                assert!(admit(&fixture.database, &root).is_err());
            }
            assert_eq!(
                retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                    .unwrap(),
                previous
            );
            assert_eq!(
                fs::read_to_string(held.join("original.txt")).unwrap(),
                "preserve original"
            );
            assert_eq!(
                fs::read_to_string(root.join("replacement.txt")).unwrap(),
                "preserve replacement"
            );
        }
    }

    #[test]
    fn same_path_origin_replacement_cannot_refresh_old_root_history() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let previous =
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap();
        fs::rename(&fixture.origin, fixture.origin.with_extension("held")).unwrap();
        fs::create_dir(&fixture.origin).unwrap();
        assert!(admit(&fixture.database, &root).is_err());
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            previous
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn retained_uuid_identity_admits_same_object_after_mount_fields_change() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let current_root = directory_identity(&root, &real_directory(&root).unwrap()).unwrap();
        let current_origin =
            directory_identity(&fixture.origin, &real_directory(&fixture.origin).unwrap()).unwrap();
        let mut retained = inventory(&fixture.database, None).roots.remove(0);
        let saved_root = retained.identity.as_mut().unwrap();
        assert!(saved_root.volume_uuid.is_some());
        saved_root.device ^= 1;
        saved_root.filesystem[0] ^= 1;
        let saved_origin = retained.origin_identity.as_mut().unwrap();
        saved_origin.device ^= 1;
        saved_origin.filesystem[0] ^= 1;
        let payload = serde_json::to_string(&retained).unwrap();
        open_registry(&fixture.database, true)
            .unwrap()
            .execute(
                "UPDATE roots SET payload=?1 WHERE key=?2",
                params![payload, record_key(&root)],
            )
            .unwrap();

        admit(&fixture.database, &root)
            .expect("stable UUID and inode should survive remount-scoped fields")
            .record(&root, &owner, WorktreeRootSource::LibraryOverride)
            .unwrap();

        let refreshed = inventory(&fixture.database, None).roots.remove(0);
        assert_eq!(refreshed.identity, current_root);
        assert_eq!(refreshed.origin_identity, current_origin);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn legacy_registration_with_exact_raw_identity_upgrades_during_admission() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let mut retained = inventory(&fixture.database, None).roots.remove(0);
        retained.identity.as_mut().unwrap().volume_uuid = None;
        retained.origin_identity.as_mut().unwrap().volume_uuid = None;
        let previous = serde_json::to_string(&retained).unwrap();
        open_registry(&fixture.database, true)
            .unwrap()
            .execute(
                "UPDATE roots SET payload=?1 WHERE key=?2",
                params![previous, record_key(&root)],
            )
            .unwrap();

        admit(&fixture.database, &root)
            .unwrap()
            .record(&root, &owner, WorktreeRootSource::LibraryOverride)
            .expect("exact prior raw identity and owner proof should allow UUID capture");

        let refreshed = inventory(&fixture.database, None).roots.remove(0);
        assert!(refreshed.identity.unwrap().volume_uuid.is_some());
        assert!(refreshed.origin_identity.unwrap().volume_uuid.is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn remounted_legacy_registration_stays_fenced_for_reviewed_recovery() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let mut retained = inventory(&fixture.database, None).roots.remove(0);
        retained.identity.as_mut().unwrap().volume_uuid = None;
        retained.origin_identity.as_mut().unwrap().volume_uuid = None;
        retained.identity.as_mut().unwrap().device ^= 1;
        retained.identity.as_mut().unwrap().filesystem[0] ^= 1;
        retained.origin_identity.as_mut().unwrap().device ^= 1;
        retained.origin_identity.as_mut().unwrap().filesystem[0] ^= 1;
        let previous = serde_json::to_string(&retained).unwrap();
        open_registry(&fixture.database, true)
            .unwrap()
            .execute(
                "UPDATE roots SET payload=?1 WHERE key=?2",
                params![previous, record_key(&root)],
            )
            .unwrap();

        let error = admit(&fixture.database, &root)
            .err()
            .expect("changed legacy mount identity must require reviewed recovery");
        assert!(
            error
                .to_string()
                .contains("legacy identity without a saved volume UUID")
        );
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(previous)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn absent_legacy_origin_uses_only_exact_raw_filesystem_proof() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let mut retained = inventory(&fixture.database, None).roots.remove(0);
        retained.origin_identity.as_mut().unwrap().volume_uuid = None;
        let previous = serde_json::to_string(&retained).unwrap();
        open_registry(&fixture.database, true)
            .unwrap()
            .execute(
                "UPDATE roots SET payload=?1 WHERE key=?2",
                params![previous, record_key(&root)],
            )
            .unwrap();

        fs::remove_dir(&fixture.origin).unwrap();
        assert!(origin_is_missing(&retained).expect("matching raw filesystem proves absence"));
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(previous),
            "absence inspection must not capture a UUID into retained history"
        );

        let mismatch_fixture = Fixture::new();
        let (mismatch_root, mismatch_owner) = mismatch_fixture.root("root");
        mismatch_fixture.register(&mismatch_root, &mismatch_owner);
        let mut mismatched = inventory(&mismatch_fixture.database, None).roots.remove(0);
        let saved_origin = mismatched.origin_identity.as_mut().unwrap();
        saved_origin.volume_uuid = None;
        saved_origin.device ^= 1;
        saved_origin.filesystem[0] ^= 1;
        let mismatch_payload = serde_json::to_string(&mismatched).unwrap();
        open_registry(&mismatch_fixture.database, true)
            .unwrap()
            .execute(
                "UPDATE roots SET payload=?1 WHERE key=?2",
                params![mismatch_payload, record_key(&mismatch_root)],
            )
            .unwrap();
        fs::remove_dir(&mismatch_fixture.origin).unwrap();
        assert!(origin_is_missing(&mismatched).is_err());
        assert_eq!(
            retained_registration(
                &open_registry(&mismatch_fixture.database, false).unwrap(),
                &mismatch_root
            )
            .unwrap(),
            Some(mismatch_payload),
            "mismatched absence must stay unknown without rewriting history"
        );
    }

    #[test]
    fn origin_replacement_after_admission_cannot_rewrite_saved_history() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fs::write(fixture.origin.join("original.txt"), b"original origin").unwrap();
        fixture.register(&root, &owner);
        let previous =
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap();
        let admission = admit(&fixture.database, &root).unwrap();
        let held = fixture.origin.with_extension("held");
        fs::rename(&fixture.origin, &held).unwrap();
        fs::create_dir(&fixture.origin).unwrap();
        fs::write(
            fixture.origin.join("replacement.txt"),
            b"replacement origin",
        )
        .unwrap();

        assert!(
            admission
                .record(&root, &owner, WorktreeRootSource::LibraryOverride)
                .is_err()
        );
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            previous
        );
        assert_eq!(
            fs::read(held.join("original.txt")).unwrap(),
            b"original origin"
        );
        assert_eq!(
            fs::read(fixture.origin.join("replacement.txt")).unwrap(),
            b"replacement origin"
        );
    }

    #[test]
    fn unknown_saved_filesystem_identity_is_preserved_without_a_fresh_proof() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let connection = open_registry(&fixture.database, true).unwrap();
        let mut retained = inventory(&fixture.database, None).roots.remove(0);
        retained.identity = None;
        let unknown = serde_json::to_string(&retained).unwrap();
        connection
            .execute("UPDATE roots SET payload=?1", [&unknown])
            .unwrap();
        drop(connection);
        assert!(admit(&fixture.database, &root).is_err());
        assert_eq!(
            retained_registration(&open_registry(&fixture.database, false).unwrap(), &root)
                .unwrap(),
            Some(unknown)
        );
    }

    #[test]
    fn marker_edits_after_claim_are_validated_from_the_pinned_witness_before_deletion() {
        use std::os::unix::fs::MetadataExt;

        let fixture = Fixture::new();
        let (root, mut owner) = fixture.root("root");
        let payload = root.join("retained.txt");
        fs::write(&payload, "must survive changed authority").unwrap();
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        let mut claim = claim_for_removal(&fixture.database, &root, &fingerprint).unwrap();
        let marker_path = root.join(WORKTREE_ROOT_MARKER);
        let before = fs::metadata(&marker_path).unwrap();
        owner.repository_key = "changed-owner-after-claim".into();
        let changed = serde_json::to_vec(&owner).unwrap();
        fs::write(&marker_path, &changed).unwrap();
        let after = fs::metadata(&marker_path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));

        assert!(claim.remove(None).is_err());
        assert_eq!(
            fs::read_to_string(&payload).unwrap(),
            "must survive changed authority"
        );
        assert_eq!(fs::read(&marker_path).unwrap(), changed);
    }

    #[test]
    fn a_restored_origin_after_claim_revokes_deletion_before_the_first_unlink() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fs::write(
            root.join("retained.txt"),
            "restored repository keeps its worktrees",
        )
        .unwrap();
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        let mut claim = claim_for_removal(&fixture.database, &root, &fingerprint).unwrap();

        fs::create_dir(&fixture.origin).unwrap();
        assert!(claim.remove(None).is_err());
        assert_eq!(
            fs::read_to_string(root.join("retained.txt")).unwrap(),
            "restored repository keeps its worktrees"
        );
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
    }

    #[test]
    fn readonly_missing_history_does_not_create_state_and_expiry_is_incomplete() {
        let fixture = Fixture::new();
        assert!(!inventory(&fixture.database, None).complete);
        assert!(!fixture.database.parent().unwrap().exists());
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let found = inventory(&fixture.database, Some(Instant::now()));
        assert!(!found.complete);
        assert!(found.roots.is_empty());
    }

    #[test]
    fn changed_root_marker_and_registry_cannot_reuse_a_reviewed_proof() {
        let fixture = Fixture::new();
        let (root, mut owner) = fixture.root("root");
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        owner.repository_key = "forged-other-owner".into();
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
        assert!(revalidate(&fixture.database, &root, &fingerprint).is_err());
        assert!(!inventory(&fixture.database, None).complete);
        fs::write(root.join(WORKTREE_ROOT_MARKER), b"{}").unwrap();
        assert!(revalidate(&fixture.database, &root, &fingerprint).is_err());
        // Replacing the entire directory cannot retain the old object proof.
        fs::rename(&root, root.with_extension("retained")).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
        assert!(!inventory(&fixture.database, None).complete);
        assert!(root.with_extension("retained").is_dir());
    }

    #[test]
    fn unavailable_or_replaced_origin_filesystem_is_not_an_orphan() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let mut record = inventory(&fixture.database, None).roots.remove(0);
        assert!(origin_is_missing(&record).unwrap());
        mutate_filesystem_identity(record.origin_identity.as_mut().unwrap());
        assert!(origin_is_missing(&record).is_err());
        assert!(ownership_fingerprint(&record).is_err());
        record.origin_identity = None;
        assert!(origin_is_missing(&record).is_err());
    }

    #[test]
    fn unverified_marker_symlink_and_oversized_record_are_refused() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fs::rename(root.join(WORKTREE_ROOT_MARKER), root.join("held-marker")).unwrap();
        symlink(root.join("held-marker"), root.join(WORKTREE_ROOT_MARKER)).unwrap();
        assert!(
            register(
                &fixture.database,
                &root,
                &owner,
                WorktreeRootSource::HostState
            )
            .is_err()
        );
        // Admission initializes its private coordination database before root
        // creation. An unverified marker must never acquire a history row.
        assert!(inventory(&fixture.database, None).roots.is_empty());
        assert_eq!(
            fs::read(root.join("held-marker")).unwrap(),
            serde_json::to_vec(&owner).unwrap()
        );
        fs::remove_file(root.join(WORKTREE_ROOT_MARKER)).unwrap();
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            vec![b'x'; MAX_RECORD_BYTES + 1],
        )
        .unwrap();
        assert!(read_marker(&root).is_err());
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
        fixture.register(&root, &owner);
        let connection = open_registry(&fixture.database, true).unwrap();
        connection
            .execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        connection
            .execute(
                "UPDATE roots SET payload=?1",
                ["x".repeat(MAX_RECORD_BYTES + 1)],
            )
            .unwrap();
        drop(connection);
        assert!(!inventory(&fixture.database, None).complete);
    }

    #[test]
    fn missing_roots_are_pruned_with_a_rotating_bounded_cursor() {
        let fixture = Fixture::new();
        let mut roots = Vec::new();
        for index in 0..PAGE_RECORDS + 5 {
            let (root, owner) = fixture.root(&format!("root-{index}"));
            fixture.register(&root, &owner);
            roots.push(root);
        }
        for root in &roots {
            fs::remove_dir_all(root).unwrap();
        }
        let (live, owner) = fixture.root("live");
        for _ in 0..4 {
            fixture.register(&live, &owner);
        }
        let connection = open_registry(&fixture.database, false).unwrap();
        let count: usize = connection
            .query_row("SELECT count(*) FROM roots", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "missing metadata must not accumulate forever");
        drop(connection);
        let found = inventory(&fixture.database, None);
        assert!(found.complete, "{:?}", found.issues);
        assert_eq!(found.roots.len(), 1);
    }

    #[test]
    fn symlinked_registry_and_unsupported_schema_are_retained_unknown() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let path = registry_path(&fixture.database).unwrap();
        let kept = path.with_extension("kept");
        fs::rename(&path, &kept).unwrap();
        symlink(&kept, &path).unwrap();
        assert!(!inventory(&fixture.database, None).complete);
        assert!(
            register(
                &fixture.database,
                &root,
                &owner,
                WorktreeRootSource::HostState
            )
            .is_err()
        );
        fs::remove_file(&path).unwrap();
        fs::rename(&kept, &path).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);
        assert!(!inventory(&fixture.database, None).complete);
    }
    #[test]
    fn full_registry_advances_maintenance_even_when_admission_is_refused() {
        let fixture = Fixture::new();
        let mut roots = Vec::new();
        for index in 0..PAGE_RECORDS + 1 {
            let (root, owner) = fixture.root(&format!("kept-{index}"));
            register_with_limit(
                &fixture.database,
                &root,
                &owner,
                WorktreeRootSource::HostState,
                PAGE_RECORDS + 1,
            )
            .unwrap();
            roots.push(root);
        }
        roots.sort_by_key(|root| record_key(root));
        fs::remove_dir_all(roots.last().unwrap()).unwrap();
        let connection = open_registry(&fixture.database, true).unwrap();
        connection
            .execute(
                "UPDATE registry_meta SET value='' WHERE key='prune_cursor'",
                [],
            )
            .unwrap();
        drop(connection);
        let (new_root, owner) = fixture.root("new-admission");
        assert!(
            register_with_limit(
                &fixture.database,
                &new_root,
                &owner,
                WorktreeRootSource::HostState,
                PAGE_RECORDS + 1
            )
            .is_err()
        );
        let connection = open_registry(&fixture.database, false).unwrap();
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM roots WHERE key=?1)",
                [record_key(&new_root)],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !present,
            "denied admission must not be registered prematurely"
        );
        drop(connection);
        register_with_limit(
            &fixture.database,
            &new_root,
            &owner,
            WorktreeRootSource::HostState,
            PAGE_RECORDS + 1,
        )
        .unwrap();
        assert!(
            inventory(&fixture.database, None)
                .roots
                .iter()
                .any(|record| record.root == new_root)
        );
    }

    #[test]
    #[ignore = "owned subprocess entrypoint for cross-process registry exclusion"]
    fn registry_admission_child() {
        let Some(database) = std::env::var_os("AETHYME_REGISTRY_TEST_DATABASE") else {
            return;
        };
        let root = PathBuf::from(std::env::var_os("AETHYME_REGISTRY_TEST_ROOT").unwrap());
        let marker = read_marker(&root).unwrap();
        let expected_blocked = std::env::var_os("AETHYME_REGISTRY_TEST_BLOCKED").is_some();
        let result = if expected_blocked {
            admit_with_limit_and_wait(Path::new(&database), &root, MAX_RECORDS, Duration::ZERO)
                .and_then(|admission| {
                    admission.record(&root, &marker, WorktreeRootSource::HostState)
                })
        } else {
            register(
                Path::new(&database),
                &root,
                &marker,
                WorktreeRootSource::HostState,
            )
        };
        if expected_blocked {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        } else {
            result.unwrap();
        }
    }

    #[test]
    fn removal_claim_excludes_new_admission_in_another_process_until_it_drops() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let held_origin = fixture.origin.with_extension("held");
        fs::rename(&fixture.origin, &held_origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        let claim = claim_for_removal(&fixture.database, &root, &fingerprint).unwrap();
        // Restore the same directory: a replacement origin is independently
        // refused even after exclusion ends. This case isolates the lock.
        fs::rename(&held_origin, &fixture.origin).unwrap();
        let child = |blocked| {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "worktree_registry::tests::registry_admission_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("AETHYME_REGISTRY_TEST_DATABASE", &fixture.database)
                .env("AETHYME_REGISTRY_TEST_ROOT", &root)
                .env_remove("AETHYME_REGISTRY_TEST_BLOCKED");
            if blocked {
                command.env("AETHYME_REGISTRY_TEST_BLOCKED", "1");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "child must actually execute its assertion"
            );
        };
        child(true);
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
        drop(claim);
        child(false);
        assert!(root.is_dir());
    }

    #[test]
    fn registry_never_changes_arbitrary_host_database_parent_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let parent = fixture.database.parent().unwrap();
        fs::create_dir(parent).unwrap();
        fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).unwrap();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        assert_eq!(
            fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(registry_directory(&fixture.database).unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn symlinked_and_hardlinked_registry_locks_cannot_split_exclusion() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let path = registry_path(&fixture.database)
            .unwrap()
            .with_extension("lock");
        let kept = path.with_extension("kept-lock");
        fs::rename(&path, &kept).unwrap();
        symlink(&kept, &path).unwrap();
        assert!(registry_lock(&fixture.database, true).is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&kept, &path).unwrap();
        assert!(registry_lock(&fixture.database, true).is_err());
        assert_eq!(fs::metadata(&kept).unwrap().len(), 0);
    }
    #[test]
    fn absent_root_requires_original_mounted_filesystem_before_retiring_work() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        fs::remove_dir_all(&root).unwrap();
        let claim = claim_for_removal(&fixture.database, &root, &fingerprint).unwrap();
        assert!(claim.already_absent);
        drop(claim);
        let mut changed_volume = record;
        mutate_filesystem_identity(changed_volume.identity.as_mut().unwrap());
        let payload = serde_json::to_string(&changed_volume).unwrap();
        let fingerprint = format!("{:x}", Sha256::digest(payload.as_bytes()));
        let connection = open_registry(&fixture.database, true).unwrap();
        connection
            .execute(
                "UPDATE roots SET payload=?1 WHERE key=?2",
                params![payload, record_key(&root)],
            )
            .unwrap();
        drop(connection);
        assert!(matches!(
            claim_for_removal(&fixture.database, &root, &fingerprint),
            Err(ClaimError::Unavailable(_))
        ));
    }
    #[test]
    fn registry_cannot_be_placed_inside_a_root_it_may_remove() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        let database = root.join("host-operations.db");
        assert!(
            register(
                &database,
                &root,
                &owner,
                WorktreeRootSource::LibraryOverride
            )
            .is_err()
        );
        assert!(!registry_directory(&database).unwrap().exists());
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
    }
}
