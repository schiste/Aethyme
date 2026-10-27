//! Durable, repository-scoped intent for shared preparation-cache rotation.
//!
//! The record is an immutable witness in the cache root. It is published with
//! a no-replace rename and is removed only after the old store is gone and the
//! replacement store is a new, empty directory on the same filesystem. Every
//! mutation requires the caller to hold the repository's preparation-cache
//! lease for its full duration.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::collections::BTreeSet;
use std::ffi::{CStr, CString, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::{self, MaybeUninit};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const SCHEMA: &str = "aethyme.preparation-cache-rotation-debt";
const VERSION: u8 = 1;
const MAX_RECORD_BYTES: u64 = 8 * 1024;
const MAX_ROOT_IDENTITY_BYTES: usize = 512;
const RECORD_PREFIX: &str = ".aethyme-preparation-rotation-";
const SCRATCH_SUFFIX: &str = ".scratch";
const FRESH_STAGE_PREFIX: &str = ".aethyme-preparation-stage-";
const FRESH_STAGE_SCHEMA: &str = "aethyme.preparation-cache-fresh-stage";
const FRESH_STAGE_VERSION: u8 = 1;
const DIRECTORY_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryIdentity {
    device: u64,
    inode: u64,
    filesystem_id: String,
}

impl DirectoryIdentity {
    pub(crate) fn capture(path: &Path) -> io::Result<Self> {
        let directory = open_directory(path)?;
        identity_for_directory(&directory)
    }

    pub(crate) fn encode(&self) -> io::Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|error| {
            invalid_record(&format!("could not encode cache-root identity: {error}"))
        })
    }

    fn validate(&self) -> io::Result<()> {
        if !is_hex(&self.filesystem_id, 2, 128) || !self.filesystem_id.len().is_multiple_of(2) {
            return Err(invalid_record("filesystem identity is malformed"));
        }
        Ok(())
    }

    fn same_directory(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.filesystem_id == other.filesystem_id
    }

    pub(crate) fn same_filesystem(&self, other: &Self) -> bool {
        self.device == other.device && self.filesystem_id == other.filesystem_id
    }
}

const PREPARATION_ROOT_MARKER: &str = ".aethyme-preparation-cache-root-v2";
const PREPARATION_ROOT_LOCK: &str = ".aethyme-preparation-cache-root.lock";
const PREPARATION_ROOT_MARKER_SCHEMA: &str = "aethyme.preparation-cache-root";
const PREPARATION_ROOT_MARKER_VERSION: u8 = 2;
const PREPARATION_ROOT_LOCK_WAIT: Duration = Duration::from_secs(5);
const PREPARATION_ROOT_MAX_ENTRIES: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PreparationCacheRootMarker {
    schema: String,
    version: u8,
    root_path: PathBuf,
    root_identity: DirectoryIdentity,
    coordinator_db_path: PathBuf,
    nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparationCacheRootCaptureState {
    Uninitialized,
    Marked {
        coordinator_db_path: PathBuf,
        nonce: String,
        cache_root_identity: Option<Vec<u8>>,
    },
}

#[derive(Debug)]
pub(crate) struct PreparationCacheRootCapture {
    pub(crate) directory: File,
    pub(crate) identity: Vec<u8>,
    pub(crate) state: PreparationCacheRootCaptureState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitializedPreparationCacheRoot {
    pub(crate) nonce: String,
    pub(crate) root_identity: Vec<u8>,
    pub(crate) cache_root_identity: Vec<u8>,
    pub(crate) coordinator_db_path: PathBuf,
}

/// Capture an existing, canonical directory without creating or repairing any
/// state. A missing marker is represented as Uninitialized; callers may
/// promote it only from an explicit writable open while holding the repository
/// preparation-cache lease.
pub(crate) fn capture_preparation_cache_root(
    path: &Path,
) -> io::Result<PreparationCacheRootCapture> {
    if !path.is_absolute()
        || path.parent().is_none()
        || path.to_str().is_none()
        || path.as_os_str().as_bytes().len() > 4096
    {
        return Err(invalid_record(
            "preparation-cache override must be a bounded absolute UTF-8 directory path",
        ));
    }
    let canonical = std::fs::canonicalize(path)?;
    if canonical != path {
        return Err(invalid_record(
            "preparation-cache override must already be canonical",
        ));
    }
    let deadline = Instant::now() + PREPARATION_ROOT_LOCK_WAIT;
    let directory = open_real_directory_chain(path, deadline)?;
    let identity = identity_for_directory(&directory)?;
    ensure_cache_root_path_identity(path, &directory, &identity, deadline)?;
    let identity_bytes = identity.encode()?;
    let lock_name = component(PREPARATION_ROOT_LOCK)?;
    let lock = read_component(directory.as_raw_fd(), &lock_name)?;
    if lock.as_ref().is_some_and(|lock| !lock.bytes.is_empty()) {
        return Err(invalid_record("preparation-cache root lock is not empty"));
    }

    let marker_name = component(PREPARATION_ROOT_MARKER)?;
    let marker_read = read_component(directory.as_raw_fd(), &marker_name)?;
    let state = match marker_read {
        None => {
            let mut allowed = Vec::new();
            if lock.is_some() {
                allowed.push(PREPARATION_ROOT_LOCK.as_bytes());
            }
            if !preparation_root_contains_only_names(&directory, &allowed, deadline)? {
                return Err(invalid_record(
                    "uninitialized preparation-cache root contains unowned entries",
                ));
            }
            PreparationCacheRootCaptureState::Uninitialized
        }
        Some(marker) => {
            let parsed = decode_preparation_cache_root_marker(&marker.bytes, path, &identity)?;
            if lock.is_none() {
                return Err(invalid_record(
                    "initialized preparation-cache root has no stable lock file",
                ));
            }
            let child_name = preparation_cache_child_component(&parsed.nonce)?;
            let child_identity = directory_identity_at(directory.as_raw_fd(), &child_name)?
                .map(|child| child.encode())
                .transpose()?;
            let mut allowed = vec![
                PREPARATION_ROOT_LOCK.as_bytes(),
                PREPARATION_ROOT_MARKER.as_bytes(),
            ];
            if child_identity.is_some() {
                allowed.push(child_name.to_bytes());
            }
            if !preparation_root_contains_only_names(&directory, &allowed, deadline)? {
                return Err(invalid_record(
                    "initialized preparation-cache root contains unknown entries",
                ));
            }
            PreparationCacheRootCaptureState::Marked {
                coordinator_db_path: parsed.coordinator_db_path,
                nonce: parsed.nonce,
                cache_root_identity: child_identity,
            }
        }
    };
    ensure_cache_root_path_identity(path, &directory, &identity, deadline)?;
    Ok(PreparationCacheRootCapture {
        directory,
        identity: identity_bytes,
        state,
    })
}

/// Create or open one private cache-root child beneath a pinned, owner-bound
/// worktree placement directory. Existing entries are opened without following
/// symlinks; their cache-root marker is validated by the caller before use.
pub(crate) fn ensure_preparation_cache_root_child(
    parent_path: &Path,
    child_name: &str,
    expected_parent_identity: &[u8],
) -> io::Result<()> {
    let expected_parent = decode_directory_identity(expected_parent_identity)?;
    let deadline = Instant::now() + PREPARATION_ROOT_LOCK_WAIT;
    let parent = open_real_directory_chain(parent_path, deadline)?;
    let parent_identity = identity_for_directory(&parent)?;
    if !parent_identity.same_directory(&expected_parent) {
        return Err(invalid_record(
            "worktree placement root changed before cache-root creation",
        ));
    }
    let child_name = component(child_name)?;
    let child = match open_directory_at(parent.as_raw_fd(), &child_name) {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // SAFETY: parent is pinned and child_name is a validated single
            // component. mkdirat cannot follow or replace another entry.
            let created = if unsafe {
                libc::mkdirat(
                    parent.as_raw_fd(),
                    child_name.as_ptr(),
                    0o700 as libc::mode_t,
                )
            } == 0
            {
                true
            } else {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
                false
            };
            if created {
                normalize_created_directory_mode(
                    parent.as_raw_fd(),
                    &child_name,
                    0o700 as libc::mode_t,
                )?;
                parent.sync_all()?;
            }
            open_directory_at(parent.as_raw_fd(), &child_name)?
        }
        Err(error) => return Err(error),
    };
    identity_for_directory(&child)?;
    ensure_cache_root_path_identity(parent_path, &parent, &expected_parent, deadline)
}

/// Initialize only the opt-in root marker/nonce child. The caller must already
/// hold the unchanged repository-scoped preparation lease. Journal inspection
/// runs under the root lock and must use the captured host-resource DB.
pub(crate) fn validate_preparation_cache_root_handle(
    path: &Path,
    pinned_directory: &File,
    expected_root_identity: &[u8],
) -> io::Result<()> {
    let expected_identity = decode_directory_identity(expected_root_identity)?;
    let deadline = Instant::now() + PREPARATION_ROOT_LOCK_WAIT;
    ensure_cache_root_path_identity(path, pinned_directory, &expected_identity, deadline)
}

pub(crate) fn initialize_preparation_cache_root(
    path: &Path,
    expected_root_identity: &[u8],
    pinned_directory: &File,
    coordinator_db_path: &Path,
    journal_has_rows_under_root: impl Fn() -> io::Result<bool>,
) -> io::Result<InitializedPreparationCacheRoot> {
    let deadline = Instant::now() + PREPARATION_ROOT_LOCK_WAIT;
    let expected_identity = decode_directory_identity(expected_root_identity)?;
    let pinned_identity = identity_for_directory(pinned_directory)?;
    if !pinned_identity.same_directory(&expected_identity) {
        return Err(invalid_record(
            "pinned preparation-cache root identity changed before initialization",
        ));
    }
    validate_coordinator_db_path(coordinator_db_path)?;
    ensure_cache_root_path_identity(path, pinned_directory, &expected_identity, deadline)?;
    let lock_name = component(PREPARATION_ROOT_LOCK)?;
    let lock = open_or_create_preparation_root_lock(pinned_directory, &lock_name)?;
    acquire_preparation_root_lock(&lock, deadline)?;
    ensure_component_identity(
        pinned_directory.as_raw_fd(),
        &lock_name,
        file_identity(&lock)?,
    )?;
    ensure_cache_root_path_identity(path, pinned_directory, &expected_identity, deadline)?;

    let marker_name = component(PREPARATION_ROOT_MARKER)?;
    let (marker, marker_before) = match read_component(pinned_directory.as_raw_fd(), &marker_name)?
    {
        Some(read) => {
            let marker =
                decode_preparation_cache_root_marker(&read.bytes, path, &expected_identity)?;
            if marker.coordinator_db_path != coordinator_db_path {
                return Err(invalid_record(
                    "preparation-cache root is bound to a different host-resource database path",
                ));
            }
            (marker, read)
        }
        None => {
            if !preparation_root_contains_only_names(
                pinned_directory,
                &[PREPARATION_ROOT_LOCK.as_bytes()],
                deadline,
            )? {
                return Err(invalid_record(
                    "uninitialized preparation-cache root contains unowned entries",
                ));
            }
            if journal_has_rows_under_root()? {
                return Err(invalid_record(
                    "uninitialized preparation-cache root has prior rotation journal state",
                ));
            }
            ensure_cache_root_path_identity(path, pinned_directory, &expected_identity, deadline)?;
            let marker = PreparationCacheRootMarker {
                schema: PREPARATION_ROOT_MARKER_SCHEMA.to_owned(),
                version: PREPARATION_ROOT_MARKER_VERSION,
                root_path: path.to_path_buf(),
                root_identity: expected_identity.clone(),
                coordinator_db_path: coordinator_db_path.to_path_buf(),
                nonce: random_nonce()?,
            };
            let bytes = serde_json::to_vec(&marker)
                .map_err(|error| invalid_record(&format!("cannot encode root marker: {error}")))?;
            if bytes.len() > MAX_RECORD_BYTES as usize {
                return Err(invalid_record("preparation-cache root marker is too large"));
            }
            let mut file = create_regular_exclusive(pinned_directory.as_raw_fd(), &marker_name)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            pinned_directory.sync_all()?;
            let read = read_component(pinned_directory.as_raw_fd(), &marker_name)?
                .ok_or_else(|| invalid_record("preparation-cache root marker disappeared"))?;
            if read.bytes != bytes {
                return Err(invalid_record(
                    "preparation-cache root marker changed after publication",
                ));
            }
            (marker, read)
        }
    };

    let child_name = preparation_cache_child_component(&marker.nonce)?;
    let child = match directory_identity_at(pinned_directory.as_raw_fd(), &child_name)? {
        Some(identity) => {
            let directory = open_directory_at(pinned_directory.as_raw_fd(), &child_name)?;
            let actual = identity_for_directory(&directory)?;
            if !actual.same_directory(&identity) {
                return Err(invalid_record(
                    "preparation-cache namespace changed while opening",
                ));
            }
            (directory, actual)
        }
        None => {
            if journal_has_rows_under_root()? {
                return Err(invalid_record(
                    "preparation-cache namespace is absent while rotation journal state remains",
                ));
            }
            let current_marker = read_component(pinned_directory.as_raw_fd(), &marker_name)?
                .ok_or_else(|| invalid_record("preparation-cache root marker disappeared"))?;
            if current_marker.identity != marker_before.identity
                || current_marker.bytes != marker_before.bytes
            {
                return Err(invalid_record(
                    "preparation-cache root marker changed before namespace creation",
                ));
            }
            if !preparation_root_contains_only_names(
                pinned_directory,
                &[
                    PREPARATION_ROOT_LOCK.as_bytes(),
                    PREPARATION_ROOT_MARKER.as_bytes(),
                ],
                deadline,
            )? {
                return Err(invalid_record(
                    "preparation-cache root contains unknown entries before namespace creation",
                ));
            }
            ensure_cache_root_path_identity(path, pinned_directory, &expected_identity, deadline)?;
            // SAFETY: pinned_directory is a live directory descriptor and
            // child_name is a NUL-terminated single component.
            if unsafe {
                libc::mkdirat(
                    pinned_directory.as_raw_fd(),
                    child_name.as_ptr(),
                    0o700 as libc::mode_t,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            normalize_created_directory_mode(
                pinned_directory.as_raw_fd(),
                &child_name,
                0o700 as libc::mode_t,
            )?;
            pinned_directory.sync_all()?;
            let directory = open_directory_at(pinned_directory.as_raw_fd(), &child_name)?;
            let identity = identity_for_directory(&directory)?;
            (directory, identity)
        }
    };
    let child_identity = child.1.encode()?;
    if !preparation_root_contains_only_names(
        pinned_directory,
        &[
            PREPARATION_ROOT_LOCK.as_bytes(),
            PREPARATION_ROOT_MARKER.as_bytes(),
            child_name.to_bytes(),
        ],
        deadline,
    )? {
        return Err(invalid_record(
            "initialized preparation-cache root contains unknown entries",
        ));
    }
    let marker_after = read_component(pinned_directory.as_raw_fd(), &marker_name)?
        .ok_or_else(|| invalid_record("preparation-cache root marker disappeared"))?;
    let validated_after =
        decode_preparation_cache_root_marker(&marker_after.bytes, path, &expected_identity)?;
    if validated_after != marker || marker_before.identity != marker_after.identity {
        return Err(invalid_record(
            "preparation-cache root marker changed during initialization",
        ));
    }
    ensure_cache_root_path_identity(path, pinned_directory, &expected_identity, deadline)?;
    Ok(InitializedPreparationCacheRoot {
        nonce: marker.nonce,
        root_identity: expected_identity.encode()?,
        cache_root_identity: child_identity,
        coordinator_db_path: marker.coordinator_db_path,
    })
}

fn validate_coordinator_db_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path.parent().is_none()
        || path.file_name().is_none()
        || path.to_str().is_none()
        || path.as_os_str().as_bytes().len() > 4096
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(invalid_record(
            "captured host-resource database path is not canonical and bounded",
        ));
    }
    Ok(())
}

fn decode_directory_identity(bytes: &[u8]) -> io::Result<DirectoryIdentity> {
    if bytes.is_empty() || bytes.len() > MAX_ROOT_IDENTITY_BYTES {
        return Err(invalid_record(
            "captured preparation-cache root identity is invalid",
        ));
    }
    let identity: DirectoryIdentity = serde_json::from_slice(bytes).map_err(|error| {
        invalid_record(&format!("captured root identity is malformed: {error}"))
    })?;
    identity.validate()?;
    if identity.encode()? != bytes {
        return Err(invalid_record(
            "captured preparation-cache root identity is not canonical",
        ));
    }
    Ok(identity)
}

fn decode_preparation_cache_root_marker(
    bytes: &[u8],
    expected_path: &Path,
    expected_identity: &DirectoryIdentity,
) -> io::Result<PreparationCacheRootMarker> {
    if bytes.is_empty() || bytes.len() > MAX_RECORD_BYTES as usize {
        return Err(invalid_record(
            "preparation-cache root marker has invalid size",
        ));
    }
    let marker: PreparationCacheRootMarker = serde_json::from_slice(bytes).map_err(|error| {
        invalid_record(&format!(
            "preparation-cache root marker is malformed: {error}"
        ))
    })?;
    if marker.schema != PREPARATION_ROOT_MARKER_SCHEMA
        || marker.version != PREPARATION_ROOT_MARKER_VERSION
        || marker.root_path != expected_path
        || !marker.root_identity.same_directory(expected_identity)
        || !is_hex(&marker.nonce, 64, 64)
        || serde_json::to_vec(&marker)
            .map_err(|error| invalid_record(&format!("cannot validate root marker: {error}")))?
            != bytes
    {
        return Err(invalid_record(
            "preparation-cache root marker does not match its pinned directory",
        ));
    }
    Ok(marker)
}

fn preparation_cache_child_component(nonce: &str) -> io::Result<CString> {
    if !is_hex(nonce, 64, 64) {
        return Err(invalid_record("preparation-cache root nonce is malformed"));
    }
    component(&format!(".aethyme-preparation-cache-{nonce}"))
}

fn random_nonce() -> io::Result<String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(encode_hex(&bytes))
}

fn file_identity(file: &File) -> io::Result<FileIdentity> {
    let metadata = file.metadata()?;
    if !is_private_owned_file(&metadata) || metadata.len() != 0 {
        return Err(invalid_record(
            "preparation-cache root lock must be a private empty regular file",
        ));
    }
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn open_or_create_preparation_root_lock(directory: &File, name: &CStr) -> io::Result<File> {
    // SAFETY: `directory` owns an open directory descriptor and `name` is a
    // NUL-terminated component. O_NOFOLLOW prevents opening a symlink, and
    // O_EXCL makes the create attempt fail rather than replace an existing file.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
            0o600 as libc::mode_t as libc::c_uint,
        )
    };
    let (fd, created) = if fd >= 0 {
        (fd, true)
    } else {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
        // SAFETY: `directory` owns an open directory descriptor and `name` is a
        // NUL-terminated component. O_NOFOLLOW rejects a symlink replacement.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        (fd, false)
    };
    // SAFETY: fd is the fresh descriptor returned by successful openat above.
    let lock = unsafe { File::from_raw_fd(fd) };
    if created {
        normalize_created_file_mode(&lock, 0o600 as libc::mode_t)?;
    }
    let identity = file_identity(&lock)?;
    ensure_component_identity(directory.as_raw_fd(), name, identity)?;
    if created {
        lock.sync_all()?;
        directory.sync_all()?;
    }
    Ok(lock)
}

fn acquire_preparation_root_lock(lock: &File, deadline: Instant) -> io::Result<()> {
    loop {
        // SAFETY: flock operates on a live descriptor and does not dereference
        // memory owned by this crate.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        let lock_busy = error
            .raw_os_error()
            .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN);
        if !lock_busy || Instant::now() >= deadline {
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn ensure_cache_root_path_identity(
    path: &Path,
    pinned_directory: &File,
    expected_identity: &DirectoryIdentity,
    deadline: Instant,
) -> io::Result<()> {
    if std::fs::canonicalize(path)? != path {
        return Err(invalid_record(
            "preparation-cache root path is no longer canonical",
        ));
    }
    let path_directory = open_real_directory_chain(path, deadline)?;
    let pinned_identity = identity_for_directory(pinned_directory)?;
    let current_identity = identity_for_directory(&path_directory)?;
    if !pinned_identity.same_directory(expected_identity)
        || !current_identity.same_directory(expected_identity)
    {
        return Err(invalid_record(
            "preparation-cache root path was replaced during initialization",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RotationDebt {
    schema: String,
    version: u8,
    repository: String,
    transaction: String,
    cache_root: DirectoryIdentity,
    original_store: DirectoryIdentity,
    retired_component: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
enum FreshStageSource {
    CanonicalOriginal,
    RetiredOriginal,
    LegacyAdoption,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FreshStageIntent {
    schema: String,
    version: u8,
    debt: RotationDebt,
    source: FreshStageSource,
    adopted_identity: Option<DirectoryIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FreshStageProof {
    schema: String,
    version: u8,
    debt: RotationDebt,
    source: FreshStageSource,
    fresh_store: DirectoryIdentity,
}

impl FreshStageIntent {
    fn new(
        debt: &RotationDebt,
        source: FreshStageSource,
        adopted_identity: Option<DirectoryIdentity>,
    ) -> Self {
        Self {
            schema: FRESH_STAGE_SCHEMA.to_owned(),
            version: FRESH_STAGE_VERSION,
            debt: debt.clone(),
            source,
            adopted_identity,
        }
    }

    fn validate(&self, expected: &RotationDebt) -> io::Result<()> {
        self.debt.validate()?;
        let adoption_is_valid = match (&self.source, &self.adopted_identity) {
            (FreshStageSource::LegacyAdoption, Some(identity)) => {
                identity.validate()?;
                identity.same_filesystem(&self.debt.cache_root)
            }
            (FreshStageSource::CanonicalOriginal | FreshStageSource::RetiredOriginal, None) => true,
            _ => false,
        };
        if self.schema != FRESH_STAGE_SCHEMA
            || self.version != FRESH_STAGE_VERSION
            || self.debt != *expected
            || !adoption_is_valid
        {
            return Err(invalid_record("fresh-store stage intent is inconsistent"));
        }
        Ok(())
    }
}

impl FreshStageProof {
    fn new(intent: &FreshStageIntent, fresh_store: DirectoryIdentity) -> io::Result<Self> {
        let proof = Self {
            schema: FRESH_STAGE_SCHEMA.to_owned(),
            version: FRESH_STAGE_VERSION,
            debt: intent.debt.clone(),
            source: intent.source.clone(),
            fresh_store,
        };
        proof.validate(&intent.debt)?;
        Ok(proof)
    }

    fn validate(&self, expected: &RotationDebt) -> io::Result<()> {
        self.debt.validate()?;
        self.fresh_store.validate()?;
        if self.schema != FRESH_STAGE_SCHEMA
            || self.version != FRESH_STAGE_VERSION
            || self.debt != *expected
            || !self.fresh_store.same_filesystem(&self.debt.cache_root)
        {
            return Err(invalid_record("fresh-store stage proof is inconsistent"));
        }
        Ok(())
    }

    fn encode(&self) -> io::Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self).map_err(|error| {
            invalid_record(&format!("could not encode fresh-store proof: {error}"))
        })?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid_record("fresh-store proof exceeds its size limit"));
        }
        Ok(bytes)
    }
}

impl RotationDebt {
    pub(crate) fn new(
        repository: &str,
        transaction: &str,
        cache_root: DirectoryIdentity,
        original_store: DirectoryIdentity,
    ) -> io::Result<Self> {
        let debt = Self {
            schema: SCHEMA.to_owned(),
            version: VERSION,
            repository: repository.to_owned(),
            transaction: transaction.to_owned(),
            retired_component: format!("{repository}.retired"),
            cache_root,
            original_store,
        };
        debt.validate()?;
        Ok(debt)
    }

    pub(crate) fn retired_component(&self) -> &str {
        &self.retired_component
    }

    fn validate(&self) -> io::Result<()> {
        self.cache_root.validate()?;
        self.original_store.validate()?;
        if self.schema != SCHEMA
            || self.version != VERSION
            || !is_hex(&self.repository, 64, 64)
            || !is_hex(&self.transaction, 32, 32)
            || self.retired_component != format!("{}.retired", self.repository)
            || !self.original_store.same_filesystem(&self.cache_root)
        {
            return Err(invalid_record("rotation debt identity is inconsistent"));
        }
        Ok(())
    }

    pub(crate) fn from_journal_payload(
        cache_root: &Path,
        repository: &str,
        bytes: &[u8],
    ) -> io::Result<Self> {
        let debt: Self = serde_json::from_slice(bytes).map_err(|error| {
            invalid_record(&format!("journal rotation debt is malformed: {error}"))
        })?;
        debt.validate()?;
        let root = open_cache_root(cache_root)?;
        let identity = identity_for_directory(&root)?;
        if debt.repository != repository
            || !debt.cache_root.same_directory(&identity)
            || encode(&debt)? != bytes
        {
            return Err(invalid_record(
                "journal rotation debt does not match its repository or cache root",
            ));
        }
        Ok(debt)
    }

    fn record_component(repository: &str) -> io::Result<CString> {
        if !is_hex(repository, 64, 64) {
            return Err(invalid_record("repository identity is malformed"));
        }
        component(&format!("{RECORD_PREFIX}{repository}.json"))
    }

    fn scratch_component(repository: &str) -> io::Result<CString> {
        if !is_hex(repository, 64, 64) {
            return Err(invalid_record("repository identity is malformed"));
        }
        component(&format!("{RECORD_PREFIX}{repository}{SCRATCH_SUFFIX}"))
    }

    fn tree_witness_component(&self) -> io::Result<CString> {
        component(&format!(
            "{RECORD_PREFIX}{}-{}.witness",
            self.repository, self.transaction
        ))
    }

    fn tree_witness_scratch_component(&self) -> io::Result<CString> {
        component(&format!(
            "{RECORD_PREFIX}{}-{}.witness{SCRATCH_SUFFIX}",
            self.repository, self.transaction
        ))
    }

    fn fresh_stage_component(repository: &str) -> io::Result<CString> {
        if !is_hex(repository, 64, 64) {
            return Err(invalid_record("repository identity is malformed"));
        }
        component(&format!("{FRESH_STAGE_PREFIX}{repository}"))
    }

    fn fresh_stage_intent_component(repository: &str) -> io::Result<CString> {
        if !is_hex(repository, 64, 64) {
            return Err(invalid_record("repository identity is malformed"));
        }
        component(&format!("{FRESH_STAGE_PREFIX}{repository}.intent"))
    }

    fn fresh_stage_intent_scratch_component(repository: &str) -> io::Result<CString> {
        if !is_hex(repository, 64, 64) {
            return Err(invalid_record("repository identity is malformed"));
        }
        component(&format!("{FRESH_STAGE_PREFIX}{repository}.intent.scratch"))
    }

    fn fresh_stage_proof_component(repository: &str) -> io::Result<CString> {
        if !is_hex(repository, 64, 64) {
            return Err(invalid_record("repository identity is malformed"));
        }
        component(&format!("{FRESH_STAGE_PREFIX}{repository}.proof"))
    }

    fn fresh_stage_proof_scratch_component(repository: &str) -> io::Result<CString> {
        if !is_hex(repository, 64, 64) {
            return Err(invalid_record("repository identity is malformed"));
        }
        component(&format!("{FRESH_STAGE_PREFIX}{repository}.proof.scratch"))
    }
}

/// A read-only classification. It is not authority to mutate any path.
/// Callers must hold the same per-repository preparation-cache lease from
/// classification through each operation, recheck pinned directory and file
/// identities immediately before mutation, and retain the debt on uncertainty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryStep {
    RenameOriginalStore,
    RemoveRetiredStore,
    CreateFreshStore,
    ClearCompletedDebt,
    Conflict,
}

/// Names that may contain an unfinished rotation. This is only bounded
/// discovery; each candidate must be re-opened and validated under its
/// repository lease before it can authorize recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DebtInventory {
    pub(crate) repositories: Vec<String>,
    pub(crate) blocked_repositories: BTreeSet<String>,
    pub(crate) complete: bool,
    /// An unclassifiable control name might belong to any old legacy-retired
    /// tree, so the caller must withhold all legacy retired cleanup this pass.
    pub(crate) block_legacy_retired: bool,
}

impl DebtInventory {
    fn empty() -> Self {
        Self {
            repositories: Vec::new(),
            blocked_repositories: BTreeSet::new(),
            complete: true,
            block_legacy_retired: false,
        }
    }
}

/// Bounded, read-only signal used by ordinary broker open to notice a crash
/// between publishing rotation debt and requesting the cache-only worker.
/// `Unknown` is deliberately fail-closed: it wakes the existing recovery
/// path but never grants mutation authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DebtProbe {
    NoDebt,
    Pending,
    Unknown,
}

pub(crate) fn probe(cache_root: &Path, max_entries: usize, deadline: Instant) -> DebtProbe {
    if Instant::now() >= deadline {
        return DebtProbe::Unknown;
    }
    match std::fs::symlink_metadata(cache_root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // An absent cache directory under an observed, real parent cannot
            // contain a published witness. Missing or ambiguous ancestors can
            // mean an unavailable mount, so they remain Unknown and keep the
            // recovery request visible. This is a local path observation; it
            // does not prove that the original filesystem is still mounted.
            return match cache_root_absent_under_observed_parent(cache_root, deadline) {
                Ok(true) => DebtProbe::NoDebt,
                Ok(false) => DebtProbe::Unknown,
                Err(_) => DebtProbe::Unknown,
            };
        }
        Err(_) => return DebtProbe::Unknown,
        Ok(_) => {}
    }
    let inventory = match inventory(cache_root, max_entries, deadline) {
        Ok(inventory) => inventory,
        Err(_) => return DebtProbe::Unknown,
    };
    if !inventory.repositories.is_empty() || !inventory.blocked_repositories.is_empty() {
        DebtProbe::Pending
    } else if inventory.complete && !inventory.block_legacy_retired {
        DebtProbe::NoDebt
    } else {
        DebtProbe::Unknown
    }
}

fn open_real_directory_chain(path: &Path, deadline: Instant) -> io::Result<File> {
    let mut directory = open_directory(Path::new("/"))?;
    let mut saw_root = false;
    for component in path.components() {
        if Instant::now() >= deadline {
            return Err(deadline_error());
        }
        match component {
            Component::RootDir if !saw_root => saw_root = true,
            Component::Normal(name) if saw_root => {
                let name = CString::new(name.as_bytes())
                    .map_err(|_| invalid_record("cache parent path contains NUL"))?;
                directory = open_directory_at(directory.as_raw_fd(), &name)?;
            }
            _ => return Err(invalid_record("cache parent path is not canonical")),
        }
    }
    if !saw_root {
        return Err(invalid_record("cache parent path is not absolute"));
    }
    Ok(directory)
}

/// Return true only when the cache-root entry is absent under a pinned, real
/// parent directory. A missing or ambiguous ancestor is an error, not proof
/// that the cache is clear; in particular, a detached or unavailable mount
/// must keep recovery pending.
pub(crate) fn cache_root_absent_under_observed_parent(
    cache_root: &Path,
    deadline: Instant,
) -> io::Result<bool> {
    if Instant::now() >= deadline {
        return Err(deadline_error());
    }
    let parent = cache_root
        .parent()
        .ok_or_else(|| invalid_record("cache root has no parent directory"))?;
    let name = cache_root
        .file_name()
        .ok_or_else(|| invalid_record("cache root has no final component"))?;
    let parent = open_real_directory_chain(parent, deadline)?;
    let name = CString::new(name.as_bytes())
        .map_err(|_| invalid_record("cache root component contains NUL"))?;
    Ok(!entry_exists(parent.as_raw_fd(), &name)?)
}

/// Exact progress from one repository's debt recovery. Rotation recovery does
/// not guess bytes from paths; `bytes_reclaimed` remains zero unless a caller
/// has an independent measured count. Entry counts come from successful
/// unlink operations and are retained on interruption.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RecoveryProgress {
    pub(crate) found_debt: bool,
    pub(crate) complete: bool,
    pub(crate) bytes_reclaimed: u64,
    pub(crate) entries_reclaimed: usize,
}

#[derive(Debug)]
pub(crate) struct RecoveryError {
    pub(crate) source: io::Error,
    pub(crate) progress: RecoveryProgress,
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for RecoveryError {}

/// A durable debt record together with the inode of the exact sidecar file
/// that was opened without following links.
#[derive(Debug, Clone)]
pub(crate) struct RetainedRotationDebt {
    pub(crate) debt: RotationDebt,
    marker: FileIdentity,
}

/// Exact in-tree witness that lets the pinned common deleter remove the
/// retired store while retaining the witness until the final unlink.
#[derive(Debug, Clone)]
pub(crate) struct RetiredTreeWitness {
    component: String,
    bytes: Vec<u8>,
    directory: DirectoryIdentity,
}

impl RetiredTreeWitness {
    pub(crate) fn component(&self) -> io::Result<&str> {
        Ok(&self.component)
    }

    pub(crate) fn validate_bytes(&self, actual: &[u8]) -> io::Result<()> {
        if actual == self.bytes {
            Ok(())
        } else {
            Err(invalid_record(
                "retired cache witness does not match its debt",
            ))
        }
    }

    pub(crate) fn matches_pinned_identity(
        &self,
        device: u64,
        inode: u64,
        filesystem_identity: &[u8],
    ) -> io::Result<bool> {
        Ok(self.directory.device == device
            && self.directory.inode == inode
            && self.directory.filesystem_id == encode_hex(filesystem_identity))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

/// Classify the only crash states that may be resumed automatically.
/// Callers must obtain all three identities with no-follow opens while holding
/// the repository's preparation-cache lease. `fresh_store_is_empty` must be
/// established with a bounded descriptor-relative check.
fn classify_recovery(
    debt: &RotationDebt,
    current_store: Option<&DirectoryIdentity>,
    retired_store: Option<&DirectoryIdentity>,
    fresh_store_is_empty: bool,
) -> RecoveryStep {
    let original_at_current =
        current_store.is_some_and(|identity| identity.same_directory(&debt.original_store));
    let original_at_retired =
        retired_store.is_some_and(|identity| identity.same_directory(&debt.original_store));
    let current_is_same_volume =
        current_store.is_none_or(|identity| identity.same_filesystem(&debt.cache_root));
    let retired_is_same_volume =
        retired_store.is_none_or(|identity| identity.same_filesystem(&debt.cache_root));

    if !current_is_same_volume || !retired_is_same_volume {
        return RecoveryStep::Conflict;
    }
    match (
        original_at_current,
        original_at_retired,
        current_store,
        retired_store,
    ) {
        (true, false, Some(_), None) => RecoveryStep::RenameOriginalStore,
        (false, true, None, Some(_)) => RecoveryStep::RemoveRetiredStore,
        (false, false, None, None) => RecoveryStep::CreateFreshStore,
        (false, false, Some(current), None)
            if fresh_store_is_empty && !current.same_directory(&debt.original_store) =>
        {
            RecoveryStep::ClearCompletedDebt
        }
        _ => RecoveryStep::Conflict,
    }
}

/// Recover one repository's rotation only after publishing or validating
/// its external journal row. The caller holds the same repository lease for
/// this whole function; every journal transaction finishes before filesystem
/// mutation starts.
#[cfg(test)]
pub(crate) fn recover_with_journal(
    cache_root: &Path,
    resource_db: &Path,
    repository: &str,
    deadline: Instant,
) -> Result<RecoveryProgress, RecoveryError> {
    recover_with_journal_and_root_guard(cache_root, resource_db, repository, deadline, || Ok(()))
}

/// Recover one repository's rotation while revalidating the broker's captured
/// cache-root binding immediately before every recovery mutation. The caller
/// holds the repository-scoped preparation-cache lease for the full operation.
pub(crate) fn recover_with_journal_and_root_guard(
    cache_root: &Path,
    resource_db: &Path,
    repository: &str,
    deadline: Instant,
    mut validate_root: impl FnMut() -> io::Result<()>,
) -> Result<RecoveryProgress, RecoveryError> {
    let mut progress = RecoveryProgress::default();
    validate_root().map_err(|source| recovery_error(progress, source))?;
    if Instant::now() >= deadline {
        return Err(recovery_error(progress, deadline_error()));
    }
    let root_identity = match DirectoryIdentity::capture(cache_root) {
        Ok(identity) => identity,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            let root_is_absent = match cache_root_absent_under_observed_parent(cache_root, deadline)
            {
                Ok(absent) => absent,
                Err(source) => return Err(recovery_error(progress, source)),
            };
            if !root_is_absent {
                return Err(recovery_error(
                    progress,
                    invalid_record("cache root changed while verifying its absence"),
                ));
            }
            match super::rotation_journal::probe(resource_db, cache_root, None) {
                Ok(None) | Ok(Some(super::rotation_journal::JournalProbe::NoDebt)) => {
                    return Ok(RecoveryProgress {
                        complete: true,
                        ..RecoveryProgress::default()
                    });
                }
                Ok(Some(super::rotation_journal::JournalProbe::Pending))
                | Ok(Some(super::rotation_journal::JournalProbe::Unknown)) => {
                    return Err(recovery_error(
                        progress,
                        invalid_record(
                            "cache root is absent while durable rotation debt is pending",
                        ),
                    ));
                }
                Err(source) => return Err(recovery_error(progress, source)),
            }
        }
        Err(source) => return Err(recovery_error(progress, source)),
    };
    let root_identity_bytes = root_identity
        .encode()
        .map_err(|source| recovery_error(progress, source))?;

    let mut journal_record = super::rotation_journal::load_record(
        resource_db,
        cache_root,
        &root_identity_bytes,
        repository,
    )
    .map_err(|source| recovery_error(progress, source))?;
    if journal_record.is_none() {
        let retained =
            read(cache_root, repository).map_err(|source| recovery_error(progress, source))?;
        let had_sidecar = retained.is_some();
        let debt = match retained {
            Some(retained) => Some(retained.debt),
            None => stage_transaction_debt(cache_root, repository)
                .map_err(|source| recovery_error(progress, source))?,
        };
        let Some(debt) = debt else {
            if has_uncommitted_scratch(cache_root, repository)
                .map_err(|source| recovery_error(progress, source))?
            {
                return Err(recovery_error(
                    progress,
                    invalid_record("unpublished rotation scratch has no durable journal row"),
                ));
            }
            return recover_inner_with_root_guard(
                cache_root,
                repository,
                deadline,
                false,
                &mut validate_root,
            );
        };
        let payload = encode(&debt).map_err(|source| recovery_error(progress, source))?;
        validate_root().map_err(|source| recovery_error(progress, source))?;
        if had_sidecar {
            super::rotation_journal::publish_legacy(
                resource_db,
                cache_root,
                &root_identity_bytes,
                repository,
                &payload,
            )
        } else {
            super::rotation_journal::publish(
                resource_db,
                cache_root,
                &root_identity_bytes,
                repository,
                &payload,
            )
        }
        .map_err(|source| recovery_error(progress, source))?;
        journal_record = super::rotation_journal::load_record(
            resource_db,
            cache_root,
            &root_identity_bytes,
            repository,
        )
        .map_err(|source| recovery_error(progress, source))?;
    }
    let journal_record = journal_record.ok_or_else(|| {
        recovery_error(progress, invalid_record("rotation journal row disappeared"))
    })?;
    let protocol_version = journal_record.protocol_version;
    let fresh_store_identity = journal_record.fresh_store_identity;
    let payload = journal_record.payload;
    let debt = RotationDebt::from_journal_payload(cache_root, repository, &payload)
        .map_err(|source| recovery_error(progress, source))?;

    let proof = match fresh_store_identity.as_deref() {
        Some(expected_identity) => {
            let root =
                open_cache_root(cache_root).map_err(|source| recovery_error(progress, source))?;
            let proof = read_fresh_stage_proof(&root, repository)
                .map_err(|source| recovery_error(progress, source))?
                .ok_or_else(|| {
                    recovery_error(
                        progress,
                        invalid_record("journaled fresh-store identity has no root proof"),
                    )
                })?;
            if proof
                .fresh_store
                .encode()
                .map_err(|source| recovery_error(progress, source))?
                != expected_identity
            {
                return Err(recovery_error(
                    progress,
                    invalid_record("journaled fresh-store identity differs from its root proof"),
                ));
            }
            proof
                .validate(&debt)
                .map_err(|source| recovery_error(progress, source))?;
            validate_fresh_stage_state(&root, &proof)
                .map_err(|source| recovery_error(progress, source))?;
            proof
        }
        None => {
            let proof = ensure_fresh_stage_with_root_guard(
                cache_root,
                &debt,
                protocol_version == 2,
                &mut validate_root,
            )
            .map_err(|source| recovery_error(progress, source))?;
            let identity = proof
                .fresh_store
                .encode()
                .map_err(|source| recovery_error(progress, source))?;
            validate_root().map_err(|source| recovery_error(progress, source))?;
            super::rotation_journal::record_fresh_store_identity(
                resource_db,
                cache_root,
                &root_identity_bytes,
                repository,
                &payload,
                &identity,
            )
            .map_err(|source| recovery_error(progress, source))?;
            proof
        }
    };

    let retained =
        read(cache_root, repository).map_err(|source| recovery_error(progress, source))?;
    if let Some(retained) = retained {
        if retained.debt != debt {
            return Err(recovery_error(
                progress,
                invalid_record("in-cache rotation witness differs from its journal row"),
            ));
        }
    } else if recovery_step(cache_root, &debt).map_err(|source| recovery_error(progress, source))?
        != RecoveryStep::ClearCompletedDebt
    {
        restore_from_journal_with_root_guard(cache_root, &debt, &mut validate_root)
            .map_err(|source| recovery_error(progress, source))?;
    }

    progress =
        recover_inner_with_root_guard(cache_root, repository, deadline, true, &mut validate_root)?;
    if progress.complete {
        progress.found_debt = true;
        if let Err(source) = validate_root() {
            progress.complete = false;
            return Err(recovery_error(progress, source));
        }
        if let Err(source) = super::rotation_journal::complete_verified(
            resource_db,
            cache_root,
            &root_identity_bytes,
            repository,
            &payload,
            &proof
                .fresh_store
                .encode()
                .map_err(|source| recovery_error(progress, source))?,
        ) {
            progress.complete = false;
            return Err(recovery_error(progress, source));
        }
        if let Err(source) =
            clear_fresh_stage_proof_with_root_guard(cache_root, &proof, &mut validate_root)
        {
            progress.complete = false;
            return Err(recovery_error(progress, source));
        }
    }
    Ok(progress)
}

/// Detect an uncommitted witness scratch. Without the external journal row it
/// is deliberately not promoted, because recovery must not mutate a published
/// rotation that the host-state journal cannot rediscover after another crash.
pub(crate) fn has_uncommitted_scratch(cache_root: &Path, repository: &str) -> io::Result<bool> {
    let root = open_cache_root(cache_root)?;
    let name = RotationDebt::scratch_component(repository)?;
    Ok(read_component(root.as_raw_fd(), &name)?.is_some()
        || read_component(
            root.as_raw_fd(),
            &RotationDebt::fresh_stage_intent_scratch_component(repository)?,
        )?
        .is_some()
        || read_component(
            root.as_raw_fd(),
            &RotationDebt::fresh_stage_proof_scratch_component(repository)?,
        )?
        .is_some())
}

/// Inspect the current and retired names with descriptor-relative no-follow
/// opens while the caller holds the per-repository preparation-cache lease.
///
/// This is classification only. A caller must finish any committed debt
/// before creating a store, running a command, or treating a storage-budget
/// refusal as permission to skip recovery. Mutations must revalidate the
/// pinned root and exact component identities immediately before acting.
pub(crate) fn recovery_step(cache_root: &Path, debt: &RotationDebt) -> io::Result<RecoveryStep> {
    debt.validate()?;
    let root = open_cache_root(cache_root)?;
    recovery_step_at(&root, debt)
}

/// Enumerate rotation witnesses and fixed retired-store names without
/// following entries. The listing is only a hint: `recover` opens and checks
/// every sidecar and tree again while holding the matching repository lease.
/// An incomplete inventory blocks legacy retired-tree cleanup for this pass,
/// because an unseen witness could own one of those trees.
pub(crate) fn inventory(
    cache_root: &Path,
    max_entries: usize,
    deadline: Instant,
) -> io::Result<DebtInventory> {
    if Instant::now() >= deadline {
        let mut inventory = DebtInventory::empty();
        inventory.complete = false;
        inventory.block_legacy_retired = true;
        return Ok(inventory);
    }
    let root = match open_cache_root(cache_root) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if cache_root_absent_under_observed_parent(cache_root, deadline)? {
                return Ok(DebtInventory::empty());
            }
            return Err(invalid_record(
                "preparation cache root changed while absence was being verified",
            ));
        }
        Err(error) => return Err(error),
    };
    let (names, complete) = directory_names(&root, max_entries, deadline)?;
    let mut repositories = BTreeSet::new();
    let mut inventory = DebtInventory {
        repositories: Vec::new(),
        blocked_repositories: BTreeSet::new(),
        complete,
        block_legacy_retired: !complete,
    };

    for name in names {
        if Instant::now() >= deadline {
            inventory.complete = false;
            inventory.block_legacy_retired = true;
            break;
        }
        let bytes = name.as_os_str().as_bytes();
        if let Some(rest) = bytes.strip_prefix(FRESH_STAGE_PREFIX.as_bytes()) {
            let Some(repository_bytes) = rest.get(..64) else {
                inventory.complete = false;
                inventory.block_legacy_retired = true;
                continue;
            };
            let Ok(repository) = std::str::from_utf8(repository_bytes) else {
                inventory.complete = false;
                inventory.block_legacy_retired = true;
                continue;
            };
            if !is_hex(repository, 64, 64) {
                inventory.complete = false;
                inventory.block_legacy_retired = true;
                continue;
            }
            let suffix = &rest[64..];
            if suffix != b""
                && suffix != b".intent"
                && suffix != b".intent.scratch"
                && suffix != b".proof"
                && suffix != b".proof.scratch"
            {
                inventory.complete = false;
                inventory.block_legacy_retired = true;
            }
            let repository = repository.to_owned();
            repositories.insert(repository.clone());
            inventory.blocked_repositories.insert(repository);
            continue;
        }
        if let Some(rest) = bytes.strip_prefix(RECORD_PREFIX.as_bytes()) {
            let Some(repository_bytes) = rest.get(..64) else {
                inventory.complete = false;
                inventory.block_legacy_retired = true;
                continue;
            };
            let Ok(repository) = std::str::from_utf8(repository_bytes) else {
                inventory.complete = false;
                inventory.block_legacy_retired = true;
                continue;
            };
            if !is_hex(repository, 64, 64) {
                inventory.complete = false;
                inventory.block_legacy_retired = true;
                continue;
            }
            let repository = repository.to_owned();
            repositories.insert(repository.clone());
            inventory.blocked_repositories.insert(repository);
            let suffix = &rest[64..];
            if suffix != b".json" && suffix != SCRATCH_SUFFIX.as_bytes() {
                inventory.complete = false;
                inventory.block_legacy_retired = true;
            }
            continue;
        }

        if let Some(repository_bytes) = bytes.strip_suffix(b".retired") {
            let Ok(repository) = std::str::from_utf8(repository_bytes) else {
                continue;
            };
            if is_hex(repository, 64, 64) {
                repositories.insert(repository.to_owned());
                // A fixed retired tree without a validated sidecar is an
                // unresolved ownership state, never a legacy cleanup target.
                inventory.blocked_repositories.insert(repository.to_owned());
            }
        }
    }

    inventory.repositories = repositories.into_iter().collect();
    Ok(inventory)
}

/// Establish an inode-reuse-safe replacement before the old cache can move.
/// A durable root-level intent precedes the fixed staging directory; the
/// versioned proof binds the exact debt transaction to the staged directory.
/// Both records use no-follow, descriptor-relative, no-replace publication.
#[cfg(test)]
fn ensure_fresh_stage(
    cache_root: &Path,
    debt: &RotationDebt,
    allow_legacy_adoption: bool,
) -> io::Result<FreshStageProof> {
    ensure_fresh_stage_with_root_guard(cache_root, debt, allow_legacy_adoption, &mut || Ok(()))
}

fn ensure_fresh_stage_with_root_guard(
    cache_root: &Path,
    debt: &RotationDebt,
    allow_legacy_adoption: bool,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<FreshStageProof> {
    debt.validate()?;
    validate_root()?;
    let root = open_cache_root(cache_root)?;
    let root_identity = identity_for_directory(&root)?;
    if !root_identity.same_directory(&debt.cache_root) {
        return Err(invalid_record(
            "cache root changed before stage preparation",
        ));
    }

    let stage_name = RotationDebt::fresh_stage_component(&debt.repository)?;
    let intent_name = RotationDebt::fresh_stage_intent_component(&debt.repository)?;
    let intent_scratch_name = RotationDebt::fresh_stage_intent_scratch_component(&debt.repository)?;
    let proof_name = RotationDebt::fresh_stage_proof_component(&debt.repository)?;
    let proof_scratch_name = RotationDebt::fresh_stage_proof_scratch_component(&debt.repository)?;

    if let Some(proof_file) = read_component(root.as_raw_fd(), &proof_name)? {
        let proof = decode_fresh_stage_proof(proof_file, debt)?;
        validate_fresh_stage_state(&root, &proof)?;
        if let Some(intent_file) = read_component(root.as_raw_fd(), &intent_name)? {
            let intent = decode_fresh_stage_intent(intent_file, debt)?;
            if intent.source != proof.source
                || intent.adopted_identity.as_ref().is_some_and(|id| {
                    proof.source != FreshStageSource::LegacyAdoption || id != &proof.fresh_store
                })
            {
                return Err(invalid_record("fresh-stage intent differs from its proof"));
            }
            validate_root()?;
            remove_control_file(
                &root,
                &intent_name,
                &serde_json::to_vec(&intent).map_err(|error| {
                    invalid_record(&format!("could not encode fresh-stage intent: {error}"))
                })?,
            )?;
        }
        if read_component(root.as_raw_fd(), &proof_scratch_name)?.is_some() {
            return Err(invalid_record(
                "fresh-stage proof has an unpublished scratch",
            ));
        }
        return Ok(proof);
    }

    if let Some(proof_scratch) = read_component(root.as_raw_fd(), &proof_scratch_name)? {
        let intent_file = read_component(root.as_raw_fd(), &intent_name)?
            .ok_or_else(|| invalid_record("fresh-stage proof scratch has no durable intent"))?;
        let intent = decode_fresh_stage_intent(intent_file, debt)?;
        validate_fresh_stage_source(&root, &intent)?;
        match intent.source {
            FreshStageSource::LegacyAdoption => {}
            FreshStageSource::CanonicalOriginal | FreshStageSource::RetiredOriginal => {
                let stage = directory_identity_at(root.as_raw_fd(), &stage_name)?
                    .ok_or_else(|| invalid_record("fresh-stage proof scratch has no stage"))?;
                if !stage.same_filesystem(&debt.cache_root) {
                    return Err(invalid_record("fresh-stage proof scratch stage is foreign"));
                }
                let directory = open_directory_at(root.as_raw_fd(), &stage_name)?;
                if !is_owned_cache_directory(&directory.metadata()?)
                    || !directory_is_empty(&directory)?
                {
                    return Err(invalid_record(
                        "fresh-stage proof scratch stage is not an empty private directory",
                    ));
                }
            }
        }
        validate_root()?;
        remove_scratch(root.as_raw_fd(), &proof_scratch_name, &proof_scratch)?;
        root.sync_all()?;
    }
    if let Some(intent_scratch) = read_component(root.as_raw_fd(), &intent_scratch_name)? {
        // Intent publication happens before stage creation. With no committed
        // intent or proof, this private partial record cannot authorize any
        // filesystem transition and may be removed only while the stage name
        // is still absent.
        if entry_exists(root.as_raw_fd(), &stage_name)?
            || entry_exists(root.as_raw_fd(), &intent_name)?
        {
            return Err(invalid_record(
                "fresh-stage intent scratch conflicts with a staged directory",
            ));
        }
        validate_root()?;
        remove_scratch(root.as_raw_fd(), &intent_scratch_name, &intent_scratch)?;
        root.sync_all()?;
    }

    let intent = match read_component(root.as_raw_fd(), &intent_name)? {
        Some(file) => decode_fresh_stage_intent(file, debt)?,
        None => {
            if entry_exists(root.as_raw_fd(), &stage_name)? {
                return Err(invalid_record(
                    "unowned fresh-stage directory has no durable intent",
                ));
            }
            let intent = select_fresh_stage_intent(&root, debt, allow_legacy_adoption)?;
            validate_fresh_stage_source(&root, &intent)?;
            let bytes = encode_fresh_stage_intent(&intent)?;
            validate_root()?;
            publish_control_file_with_root_guard(
                &root,
                &intent_name,
                &intent_scratch_name,
                &bytes,
                validate_root,
            )?;
            intent
        }
    };
    validate_fresh_stage_source(&root, &intent)?;

    let fresh_store = match (&intent.source, &intent.adopted_identity) {
        (FreshStageSource::LegacyAdoption, Some(identity)) => {
            if entry_exists(root.as_raw_fd(), &stage_name)? {
                return Err(invalid_record(
                    "legacy adopted cache conflicts with a staging directory",
                ));
            }
            identity.clone()
        }
        (FreshStageSource::CanonicalOriginal | FreshStageSource::RetiredOriginal, None) => {
            let stage = match directory_identity_at(root.as_raw_fd(), &stage_name)? {
                Some(identity) => {
                    let directory = open_directory_at(root.as_raw_fd(), &stage_name)?;
                    if !is_owned_cache_directory(&directory.metadata()?)
                        || !directory_is_empty(&directory)?
                    {
                        return Err(invalid_record(
                            "staged replacement is not a private empty cache directory",
                        ));
                    }
                    identity
                }
                None => {
                    let mode = 0o700 as libc::mode_t;
                    validate_root()?;
                    // SAFETY: root is a live directory descriptor and stage_name is a
                    // validated single component. mkdirat cannot replace an entry.
                    if unsafe { libc::mkdirat(root.as_raw_fd(), stage_name.as_ptr(), mode) } != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    root.sync_all()?;
                    let directory = open_directory_at(root.as_raw_fd(), &stage_name)?;
                    if !is_owned_cache_directory(&directory.metadata()?)
                        || !directory_is_empty(&directory)?
                    {
                        return Err(invalid_record(
                            "new staged replacement is not a private empty cache directory",
                        ));
                    }
                    identity_for_directory(&directory)?
                }
            };
            if stage.same_directory(&debt.original_store)
                || !stage.same_filesystem(&debt.cache_root)
            {
                return Err(invalid_record(
                    "staged replacement does not have a distinct recorded identity",
                ));
            }
            stage
        }
        _ => return Err(invalid_record("fresh-stage intent has an invalid source")),
    };

    validate_fresh_stage_source(&root, &intent)?;
    let proof = FreshStageProof::new(&intent, fresh_store)?;
    let proof_bytes = proof.encode()?;
    validate_root()?;
    publish_control_file_with_root_guard(
        &root,
        &proof_name,
        &proof_scratch_name,
        &proof_bytes,
        validate_root,
    )?;
    validate_fresh_stage_state(&root, &proof)?;
    if let Some(intent_file) = read_component(root.as_raw_fd(), &intent_name)? {
        let current = decode_fresh_stage_intent(intent_file, debt)?;
        if current != intent {
            return Err(invalid_record("fresh-stage intent changed before removal"));
        }
        validate_root()?;
        remove_control_file(&root, &intent_name, &encode_fresh_stage_intent(&intent)?)?;
    }
    Ok(proof)
}

fn select_fresh_stage_intent(
    root: &File,
    debt: &RotationDebt,
    allow_legacy_adoption: bool,
) -> io::Result<FreshStageIntent> {
    let current_name = component(&debt.repository)?;
    let retired_name = component(debt.retired_component())?;
    let current = directory_identity_at(root.as_raw_fd(), &current_name)?;
    let retired = directory_identity_at(root.as_raw_fd(), &retired_name)?;
    let source = match (&current, &retired) {
        (Some(current), None)
            if current.same_directory(&debt.original_store)
                && current.same_filesystem(&debt.cache_root) =>
        {
            if allow_legacy_adoption && !has_exact_tree_witness(root, debt)? {
                return Err(invalid_record(
                    "legacy same-identity cache has no committed original-tree proof",
                ));
            }
            FreshStageSource::CanonicalOriginal
        }
        (None, Some(retired))
            if retired.same_directory(&debt.original_store)
                && retired.same_filesystem(&debt.cache_root) =>
        {
            FreshStageSource::RetiredOriginal
        }
        (Some(current), None)
            if allow_legacy_adoption
                && !current.same_directory(&debt.original_store)
                && current.same_filesystem(&debt.cache_root) =>
        {
            let directory = open_directory_at(root.as_raw_fd(), &current_name)?;
            if !directory_is_empty(&directory)? {
                return Err(invalid_record(
                    "legacy replacement cache is not an empty directory",
                ));
            }
            let intent = FreshStageIntent::new(
                debt,
                FreshStageSource::LegacyAdoption,
                Some(current.clone()),
            );
            intent.validate(debt)?;
            return Ok(intent);
        }
        _ => {
            return Err(invalid_record(
                "rotation state is ambiguous before fresh-stage intent",
            ));
        }
    };
    let intent = FreshStageIntent::new(debt, source, None);
    intent.validate(debt)?;
    Ok(intent)
}

fn has_exact_tree_witness(root: &File, debt: &RotationDebt) -> io::Result<bool> {
    let current_name = component(&debt.repository)?;
    let current = open_directory_at(root.as_raw_fd(), &current_name)?;
    let witness_name = debt.tree_witness_component()?;
    let expected = encode(debt)?;
    Ok(read_component(current.as_raw_fd(), &witness_name)?
        .is_some_and(|witness| witness.bytes == expected))
}

fn validate_fresh_stage_source(root: &File, intent: &FreshStageIntent) -> io::Result<()> {
    intent.validate(&intent.debt)?;
    let stage_name = RotationDebt::fresh_stage_component(&intent.debt.repository)?;
    let current_name = component(&intent.debt.repository)?;
    let retired_name = component(intent.debt.retired_component())?;
    let current = directory_identity_at(root.as_raw_fd(), &current_name)?;
    let retired = directory_identity_at(root.as_raw_fd(), &retired_name)?;
    match (&intent.source, &intent.adopted_identity, current, retired) {
        (FreshStageSource::CanonicalOriginal, None, Some(current), None)
            if current.same_directory(&intent.debt.original_store) => {}
        (FreshStageSource::RetiredOriginal, None, None, Some(retired))
            if retired.same_directory(&intent.debt.original_store) => {}
        (FreshStageSource::LegacyAdoption, Some(expected), Some(current), None)
            if current.same_directory(expected) =>
        {
            let directory = open_directory_at(root.as_raw_fd(), &current_name)?;
            if !directory_is_empty(&directory)? {
                return Err(invalid_record(
                    "legacy replacement cache is no longer empty",
                ));
            }
            if entry_exists(root.as_raw_fd(), &stage_name)? {
                return Err(invalid_record("legacy replacement has an unexpected stage"));
            }
        }
        _ => {
            return Err(invalid_record(
                "fresh-stage predecessor changed before staging",
            ));
        }
    }
    Ok(())
}

fn validate_fresh_stage_state(root: &File, proof: &FreshStageProof) -> io::Result<()> {
    proof.validate(&proof.debt)?;
    let stage_name = RotationDebt::fresh_stage_component(&proof.debt.repository)?;
    let current_name = component(&proof.debt.repository)?;
    let retired_name = component(proof.debt.retired_component())?;
    let current = directory_identity_at(root.as_raw_fd(), &current_name)?;
    let retired = directory_identity_at(root.as_raw_fd(), &retired_name)?;
    let stage = directory_identity_at(root.as_raw_fd(), &stage_name)?;
    match proof.source {
        FreshStageSource::LegacyAdoption => {
            if stage.is_some()
                || retired.is_some()
                || !current
                    .as_ref()
                    .is_some_and(|identity| identity.same_directory(&proof.fresh_store))
            {
                return Err(invalid_record(
                    "adopted fresh-store proof no longer matches",
                ));
            }
            let directory = open_directory_at(root.as_raw_fd(), &current_name)?;
            if !directory_is_empty(&directory)? {
                return Err(invalid_record("adopted fresh store is no longer empty"));
            }
        }
        FreshStageSource::CanonicalOriginal | FreshStageSource::RetiredOriginal => {
            match (current.as_ref(), retired.as_ref(), stage.as_ref()) {
                (Some(current), None, Some(stage))
                    if current.same_directory(&proof.debt.original_store)
                        && stage.same_directory(&proof.fresh_store) =>
                {
                    let directory = open_directory_at(root.as_raw_fd(), &stage_name)?;
                    if !directory_is_empty(&directory)? {
                        return Err(invalid_record("staged replacement is not empty"));
                    }
                }
                (None, Some(retired), Some(stage))
                    if retired.same_directory(&proof.debt.original_store)
                        && stage.same_directory(&proof.fresh_store) =>
                {
                    let directory = open_directory_at(root.as_raw_fd(), &stage_name)?;
                    if !directory_is_empty(&directory)? {
                        return Err(invalid_record("staged replacement is not empty"));
                    }
                }
                (None, None, Some(stage)) if stage.same_directory(&proof.fresh_store) => {
                    let directory = open_directory_at(root.as_raw_fd(), &stage_name)?;
                    if !directory_is_empty(&directory)? {
                        return Err(invalid_record("staged replacement is not empty"));
                    }
                }
                (Some(current), None, None) if current.same_directory(&proof.fresh_store) => {
                    let directory = open_directory_at(root.as_raw_fd(), &current_name)?;
                    if !directory_is_empty(&directory)? {
                        return Err(invalid_record("installed fresh cache is not empty"));
                    }
                }
                _ => {
                    return Err(invalid_record(
                        "staged fresh-store proof conflicts with paths",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn read_fresh_stage_intent(root: &File, repository: &str) -> io::Result<Option<FreshStageIntent>> {
    let name = RotationDebt::fresh_stage_intent_component(repository)?;
    read_component(root.as_raw_fd(), &name)?
        .map(|file| {
            let intent: FreshStageIntent =
                serde_json::from_slice(&file.bytes).map_err(|error| {
                    invalid_record(&format!("fresh-stage intent is malformed: {error}"))
                })?;
            intent.validate(&intent.debt)?;
            if encode_fresh_stage_intent(&intent)? != file.bytes {
                return Err(invalid_record("fresh-stage intent is not canonical"));
            }
            Ok(intent)
        })
        .transpose()
}

fn read_fresh_stage_proof(root: &File, repository: &str) -> io::Result<Option<FreshStageProof>> {
    let name = RotationDebt::fresh_stage_proof_component(repository)?;
    read_component(root.as_raw_fd(), &name)?
        .map(|file| {
            let proof: FreshStageProof = serde_json::from_slice(&file.bytes).map_err(|error| {
                invalid_record(&format!("fresh-stage proof is malformed: {error}"))
            })?;
            proof.validate(&proof.debt)?;
            if proof.encode()? != file.bytes {
                return Err(invalid_record("fresh-stage proof is not canonical"));
            }
            Ok(proof)
        })
        .transpose()
}

fn decode_fresh_stage_intent(
    file: ReadComponent,
    debt: &RotationDebt,
) -> io::Result<FreshStageIntent> {
    let intent: FreshStageIntent = serde_json::from_slice(&file.bytes)
        .map_err(|error| invalid_record(&format!("fresh-stage intent is malformed: {error}")))?;
    intent.validate(debt)?;
    if encode_fresh_stage_intent(&intent)? != file.bytes {
        return Err(invalid_record("fresh-stage intent is not canonical"));
    }
    Ok(intent)
}

fn decode_fresh_stage_proof(
    file: ReadComponent,
    debt: &RotationDebt,
) -> io::Result<FreshStageProof> {
    let proof: FreshStageProof = serde_json::from_slice(&file.bytes)
        .map_err(|error| invalid_record(&format!("fresh-stage proof is malformed: {error}")))?;
    proof.validate(debt)?;
    if proof.encode()? != file.bytes {
        return Err(invalid_record("fresh-stage proof is not canonical"));
    }
    Ok(proof)
}

fn encode_fresh_stage_intent(intent: &FreshStageIntent) -> io::Result<Vec<u8>> {
    intent.validate(&intent.debt)?;
    let bytes = serde_json::to_vec(intent).map_err(|error| {
        invalid_record(&format!("could not encode fresh-stage intent: {error}"))
    })?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid_record("fresh-stage intent exceeds its size limit"));
    }
    Ok(bytes)
}

#[cfg(test)]
fn publish_control_file(
    root: &File,
    final_name: &CStr,
    scratch_name: &CStr,
    bytes: &[u8],
) -> io::Result<FileIdentity> {
    publish_control_file_with_root_guard(root, final_name, scratch_name, bytes, &mut || Ok(()))
}

fn publish_control_file_with_root_guard(
    root: &File,
    final_name: &CStr,
    scratch_name: &CStr,
    bytes: &[u8],
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<FileIdentity> {
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid_record(
            "fresh-stage control record exceeds its size limit",
        ));
    }
    if let Some(existing) = read_component(root.as_raw_fd(), final_name)? {
        if existing.bytes != bytes {
            return Err(invalid_record("fresh-stage control record already differs"));
        }
        return Ok(existing.identity);
    }
    if let Some(scratch) = read_component(root.as_raw_fd(), scratch_name)? {
        if scratch.bytes == bytes {
            ensure_component_identity(root.as_raw_fd(), scratch_name, scratch.identity)?;
        } else {
            validate_root()?;
            remove_scratch(root.as_raw_fd(), scratch_name, &scratch)?;
            root.sync_all()?;
            return Err(invalid_record("fresh-stage control scratch was incomplete"));
        }
    } else {
        validate_root()?;
        let mut scratch = create_regular_exclusive(root.as_raw_fd(), scratch_name)?;
        validate_root()?;
        scratch.write_all(bytes)?;
        scratch.sync_all()?;
        let metadata = scratch.metadata()?;
        if !is_private_owned_file(&metadata) || metadata.len() != bytes.len() as u64 {
            return Err(invalid_record("fresh-stage control scratch is invalid"));
        }
        drop(scratch);
    }
    let scratch = read_component(root.as_raw_fd(), scratch_name)?
        .ok_or_else(|| invalid_record("fresh-stage control scratch disappeared"))?;
    if scratch.bytes != bytes {
        return Err(invalid_record("fresh-stage control scratch changed"));
    }
    ensure_component_identity(root.as_raw_fd(), scratch_name, scratch.identity)?;
    root.sync_all()?;
    validate_root()?;
    rename_noreplace(root.as_raw_fd(), scratch_name, root.as_raw_fd(), final_name)?;
    root.sync_all()?;
    let published = read_component(root.as_raw_fd(), final_name)?
        .ok_or_else(|| invalid_record("fresh-stage control record disappeared"))?;
    if published.bytes != bytes || published.identity != scratch.identity {
        return Err(invalid_record("fresh-stage control record was replaced"));
    }
    Ok(published.identity)
}

fn remove_control_file(root: &File, name: &CStr, expected: &[u8]) -> io::Result<()> {
    let current = read_component(root.as_raw_fd(), name)?
        .ok_or_else(|| invalid_record("fresh-stage control record disappeared"))?;
    if current.bytes != expected {
        return Err(invalid_record("fresh-stage control record changed"));
    }
    ensure_component_identity(root.as_raw_fd(), name, current.identity)?;
    unlink_component(root.as_raw_fd(), name)?;
    root.sync_all()
}

fn clear_fresh_stage_proof_with_root_guard(
    cache_root: &Path,
    proof: &FreshStageProof,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    validate_root()?;
    proof.validate(&proof.debt)?;
    let root = open_cache_root(cache_root)?;
    let root_identity = identity_for_directory(&root)?;
    if !root_identity.same_directory(&proof.debt.cache_root) {
        return Err(invalid_record(
            "cache root changed before stage-proof removal",
        ));
    }
    validate_fresh_stage_state(&root, proof)?;
    let stage_name = RotationDebt::fresh_stage_component(&proof.debt.repository)?;
    let intent_name = RotationDebt::fresh_stage_intent_component(&proof.debt.repository)?;
    let intent_scratch_name =
        RotationDebt::fresh_stage_intent_scratch_component(&proof.debt.repository)?;
    let proof_name = RotationDebt::fresh_stage_proof_component(&proof.debt.repository)?;
    let proof_scratch_name =
        RotationDebt::fresh_stage_proof_scratch_component(&proof.debt.repository)?;
    let current_name = component(&proof.debt.repository)?;
    let retired_name = component(proof.debt.retired_component())?;
    if entry_exists(root.as_raw_fd(), &stage_name)?
        || entry_exists(root.as_raw_fd(), &retired_name)?
        || entry_exists(root.as_raw_fd(), &intent_name)?
        || entry_exists(root.as_raw_fd(), &intent_scratch_name)?
        || entry_exists(root.as_raw_fd(), &proof_scratch_name)?
        || read_component(
            root.as_raw_fd(),
            &RotationDebt::record_component(&proof.debt.repository)?,
        )?
        .is_some()
        || read_component(
            root.as_raw_fd(),
            &RotationDebt::scratch_component(&proof.debt.repository)?,
        )?
        .is_some()
    {
        return Err(invalid_record(
            "fresh-store proof is not ready for final removal",
        ));
    }
    let current = open_directory_at(root.as_raw_fd(), &current_name)?;
    let current_identity = identity_for_directory(&current)?;
    if !current_identity.same_directory(&proof.fresh_store) || !directory_is_empty(&current)? {
        return Err(invalid_record(
            "installed fresh cache changed before proof removal",
        ));
    }
    let proof_bytes = proof.encode()?;
    let existing = read_component(root.as_raw_fd(), &proof_name)?
        .ok_or_else(|| invalid_record("fresh-store proof disappeared before removal"))?;
    if existing.bytes != proof_bytes {
        return Err(invalid_record("fresh-store proof changed before removal"));
    }
    ensure_component_identity(root.as_raw_fd(), &proof_name, existing.identity)?;
    validate_root()?;
    unlink_component(root.as_raw_fd(), &proof_name)?;
    root.sync_all()
}

fn stage_transaction_debt(cache_root: &Path, repository: &str) -> io::Result<Option<RotationDebt>> {
    let root = open_cache_root(cache_root)?;
    let root_identity = identity_for_directory(&root)?;
    if read_component(
        root.as_raw_fd(),
        &RotationDebt::fresh_stage_intent_scratch_component(repository)?,
    )?
    .is_some()
        || read_component(
            root.as_raw_fd(),
            &RotationDebt::fresh_stage_proof_scratch_component(repository)?,
        )?
        .is_some()
    {
        return Err(invalid_record(
            "unpublished fresh-stage scratch has no durable journal row",
        ));
    }
    if let Some(proof) = read_fresh_stage_proof(&root, repository)? {
        if !proof.debt.cache_root.same_directory(&root_identity)
            || proof.debt.repository != repository
        {
            return Err(invalid_record(
                "fresh-stage proof does not match its location",
            ));
        }
        validate_fresh_stage_state(&root, &proof)?;
        return Ok(Some(proof.debt));
    }
    if let Some(intent) = read_fresh_stage_intent(&root, repository)? {
        if !intent.debt.cache_root.same_directory(&root_identity)
            || intent.debt.repository != repository
        {
            return Err(invalid_record(
                "fresh-stage intent does not match its location",
            ));
        }
        return Ok(Some(intent.debt));
    }
    let stage_name = RotationDebt::fresh_stage_component(repository)?;
    if entry_exists(root.as_raw_fd(), &stage_name)? {
        return Err(invalid_record(
            "unowned fresh-stage directory has no durable intent",
        ));
    }
    Ok(None)
}

/// Finish a committed rotation under the repository's existing exclusive
/// preparation-cache lease. This operation is deliberately cache-only and
/// does not consult current worktree liveness: the writer published the debt
/// before it could start any command that used the store.
#[cfg(test)]
pub(crate) fn recover(
    cache_root: &Path,
    repository: &str,
    deadline: Instant,
) -> Result<RecoveryProgress, RecoveryError> {
    recover_with_root_guard(cache_root, repository, deadline, || Ok(()))
}

#[cfg(test)]
pub(crate) fn recover_with_root_guard(
    cache_root: &Path,
    repository: &str,
    deadline: Instant,
    mut validate_root: impl FnMut() -> io::Result<()>,
) -> Result<RecoveryProgress, RecoveryError> {
    recover_inner_with_root_guard(cache_root, repository, deadline, false, &mut validate_root)
}

fn recover_inner_with_root_guard(
    cache_root: &Path,
    repository: &str,
    deadline: Instant,
    retain_fresh_proof_for_journal: bool,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> Result<RecoveryProgress, RecoveryError> {
    let mut progress = RecoveryProgress::default();
    if !is_hex(repository, 64, 64) {
        return Err(recovery_error(
            progress,
            invalid_record("repository identity is malformed"),
        ));
    }
    if Instant::now() >= deadline {
        return Err(recovery_error(progress, deadline_error()));
    }

    validate_root().map_err(|source| recovery_error(progress, source))?;
    let retained =
        match read_or_promote_scratch_with_root_guard(cache_root, repository, validate_root) {
            Ok(Some(retained)) => retained,
            Ok(None) => {
                match std::fs::symlink_metadata(cache_root) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        progress.complete = true;
                        return Ok(progress);
                    }
                    Err(error) => return Err(recovery_error(progress, error)),
                    Ok(_) => {}
                }
                if let Some(debt) = stage_transaction_debt(cache_root, repository)
                    .map_err(|source| recovery_error(progress, source))?
                {
                    let root = open_cache_root(cache_root)
                        .map_err(|source| recovery_error(progress, source))?;
                    let proof = read_fresh_stage_proof(&root, repository)
                        .map_err(|source| recovery_error(progress, source))?;
                    if let Some(proof) = proof {
                        validate_fresh_stage_state(&root, &proof)
                            .map_err(|source| recovery_error(progress, source))?;
                        let step = recovery_step_at(&root, &debt)
                            .map_err(|source| recovery_error(progress, source))?;
                        if step == RecoveryStep::ClearCompletedDebt {
                            progress.found_debt = true;
                            progress.complete = true;
                            if !retain_fresh_proof_for_journal {
                                clear_fresh_stage_proof_with_root_guard(
                                    cache_root,
                                    &proof,
                                    validate_root,
                                )
                                .map_err(|source| recovery_error(progress, source))?;
                            }
                            return Ok(progress);
                        }
                    }
                    ensure_fresh_stage_with_root_guard(cache_root, &debt, true, validate_root)
                        .map_err(|source| recovery_error(progress, source))?;
                    restore_from_journal_with_root_guard(cache_root, &debt, validate_root)
                        .map_err(|source| recovery_error(progress, source))?
                } else {
                    if has_fixed_retired_store(cache_root, repository)
                        .map_err(|source| recovery_error(progress, source))?
                    {
                        return Err(recovery_error(
                            progress,
                            invalid_record("fixed retired cache has no valid rotation witness"),
                        ));
                    }
                    progress.complete = true;
                    return Ok(progress);
                }
            }
            Err(source) => return Err(recovery_error(progress, source)),
        };
    progress.found_debt = true;
    let fresh_proof =
        ensure_fresh_stage_with_root_guard(cache_root, &retained.debt, true, validate_root)
            .map_err(|source| recovery_error(progress, source))?;

    // A valid transition takes at most four steps. The larger hard bound also
    // makes unexpected state oscillation fail closed without an unbounded
    // loop while the caller holds a host resource lease.
    for _ in 0..8 {
        if Instant::now() >= deadline {
            return Err(recovery_error(progress, deadline_error()));
        }
        let step = recovery_step(cache_root, &retained.debt)
            .map_err(|source| recovery_error(progress, source))?;
        match step {
            RecoveryStep::RenameOriginalStore => {
                retire_original_store_with_root_guard(cache_root, &retained, validate_root)
                    .map_err(|source| recovery_error(progress, source))?
            }
            RecoveryStep::RemoveRetiredStore => {
                let witness = prepare_retired_store_removal_with_root_guard(
                    cache_root,
                    &retained,
                    validate_root,
                )
                .map_err(|source| recovery_error(progress, source))?;
                let retired_path = cache_root.join(retained.debt.retired_component());
                let pinned = crate::gc::PinnedTreeRemoval::open(&retired_path)
                    .map_err(|source| recovery_error(progress, source))?;
                let identity = pinned
                    .object_identity()
                    .map_err(|source| recovery_error(progress, source))?;
                let filesystem_identity = pinned
                    .filesystem_identity()
                    .map_err(|source| recovery_error(progress, source))?;
                let matches = witness
                    .matches_pinned_identity(identity.device, identity.inode, &filesystem_identity)
                    .map_err(|source| recovery_error(progress, source))?;
                if !matches {
                    return Err(recovery_error(
                        progress,
                        invalid_record("pinned retired cache does not match its debt witness"),
                    ));
                }
                let witness_name = witness
                    .component()
                    .map_err(|source| recovery_error(progress, source))?
                    .to_owned();
                validate_root().map_err(|source| recovery_error(progress, source))?;
                let root_guard = std::cell::RefCell::new(&mut *validate_root);
                match pinned.remove_verified_witness_with_progress_and_guard_refusing_special_nodes(
                    &witness_name,
                    Some(deadline),
                    |bytes| {
                        let mut validate = root_guard.borrow_mut();
                        validate()?;
                        witness.validate_bytes(bytes)
                    },
                    || {
                        let mut validate = root_guard.borrow_mut();
                        validate()
                    },
                ) {
                    Ok((crate::gc::TreeRemoval::Complete, entries)) => {
                        progress.entries_reclaimed =
                            progress.entries_reclaimed.saturating_add(entries);
                    }
                    Ok((crate::gc::TreeRemoval::Interrupted, entries)) => {
                        progress.entries_reclaimed =
                            progress.entries_reclaimed.saturating_add(entries);
                        return Err(recovery_error(progress, deadline_error()));
                    }
                    Err(error) => {
                        progress.entries_reclaimed = progress
                            .entries_reclaimed
                            .saturating_add(error.entries_reclaimed);
                        return Err(recovery_error(progress, error.source));
                    }
                }
            }
            RecoveryStep::CreateFreshStore => {
                create_fresh_store_with_root_guard(cache_root, &retained, validate_root)
                    .map_err(|source| recovery_error(progress, source))?;
            }
            RecoveryStep::ClearCompletedDebt => {
                clear_completed_with_root_guard(cache_root, &retained, validate_root)
                    .map_err(|source| recovery_error(progress, source))?;
                progress.complete = true;
                if !retain_fresh_proof_for_journal {
                    clear_fresh_stage_proof_with_root_guard(
                        cache_root,
                        &fresh_proof,
                        validate_root,
                    )
                    .map_err(|source| recovery_error(progress, source))?;
                }
                return Ok(progress);
            }
            RecoveryStep::Conflict => {
                return Err(recovery_error(
                    progress,
                    invalid_record("rotation debt conflicts with current cache state"),
                ));
            }
        }
    }
    Err(recovery_error(
        progress,
        invalid_record("rotation recovery exceeded its transition limit"),
    ))
}

fn recovery_error(progress: RecoveryProgress, source: io::Error) -> RecoveryError {
    RecoveryError { source, progress }
}

fn deadline_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "preparation-cache rotation recovery deadline elapsed",
    )
}

/// Publish the in-tree removal witness and atomically move the exact original
/// cache to its retired name. The durable sidecar must already have been
/// published, and the caller must hold the repository-scoped cache lease.
///
/// A crash before the rename leaves the original store and sidecar intact. A
/// crash after the rename leaves the same witness inside the retired tree, so
/// retry can verify the original inode before handing it to the common pinned
/// deleter.
#[cfg(test)]
pub(crate) fn retire_original_store(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
) -> io::Result<()> {
    retire_original_store_with_root_guard(cache_root, retained, &mut || Ok(()))
}

fn retire_original_store_with_root_guard(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    validate_root()?;
    if recovery_step(cache_root, &retained.debt)? != RecoveryStep::RenameOriginalStore {
        return Err(invalid_record("original cache is not ready to be retired"));
    }
    let witness = prepare_tree_witness(cache_root, retained, validate_root)?;
    let root = open_cache_root(cache_root)?;
    verify_retained_sidecar(&root, retained)?;
    if recovery_step_at(&root, &retained.debt)? != RecoveryStep::RenameOriginalStore {
        return Err(invalid_record("original cache is not ready to be retired"));
    }

    let current_name = component(&retained.debt.repository)?;
    let retired_name = component(&retained.debt.retired_component)?;
    let current_directory = open_directory_at(root.as_raw_fd(), &current_name)?;
    let current_identity = identity_for_directory(&current_directory)?;
    if !current_identity.same_directory(&retained.debt.original_store)
        || !current_identity.same_filesystem(&retained.debt.cache_root)
    {
        return Err(invalid_record(
            "original cache identity changed before retirement",
        ));
    }
    verify_tree_witness(&current_directory, retained, &witness)?;
    let named_identity = directory_identity_at(root.as_raw_fd(), &current_name)?
        .ok_or_else(|| invalid_record("original cache disappeared before retirement"))?;
    if !named_identity.same_directory(&current_identity) {
        return Err(invalid_record(
            "original cache name changed before retirement",
        ));
    }
    if entry_exists(root.as_raw_fd(), &retired_name)? {
        return Err(invalid_record("retired cache path already exists"));
    }
    verify_retained_sidecar(&root, retained)?;
    if recovery_step_at(&root, &retained.debt)? != RecoveryStep::RenameOriginalStore {
        return Err(invalid_record(
            "rotation state changed before cache retirement",
        ));
    }
    let final_source = directory_identity_at(root.as_raw_fd(), &current_name)?
        .ok_or_else(|| invalid_record("original cache disappeared before retirement"))?;
    if !final_source.same_directory(&current_identity) {
        return Err(invalid_record(
            "original cache name changed before retirement",
        ));
    }
    verify_tree_witness(&current_directory, retained, &witness)?;
    validate_root()?;
    rename_noreplace(
        root.as_raw_fd(),
        &current_name,
        root.as_raw_fd(),
        &retired_name,
    )?;
    root.sync_all()?;

    let moved = open_directory_at(root.as_raw_fd(), &retired_name)?;
    let moved_identity = identity_for_directory(&moved)?;
    if !moved_identity.same_directory(&retained.debt.original_store)
        || !moved_identity.same_filesystem(&retained.debt.cache_root)
    {
        return Err(invalid_record(
            "retired cache identity changed after rename",
        ));
    }
    verify_tree_witness(&moved, retained, &witness)
}

/// Ensure the exact in-tree witness is ready for `PinnedTreeRemoval` and
/// return the identity and bytes that the common deleter must verify.
///
/// If a previous deletion removed the witness but crashed before removing the
/// now-empty retired directory, this restores the witness only after a
/// descriptor-relative emptiness check and exact root/filesystem validation.
/// A missing witness in a nonempty retired tree is ambiguous and is refused.
#[cfg(test)]
pub(crate) fn prepare_retired_store_removal(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
) -> io::Result<RetiredTreeWitness> {
    prepare_retired_store_removal_with_root_guard(cache_root, retained, &mut || Ok(()))
}

fn prepare_retired_store_removal_with_root_guard(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<RetiredTreeWitness> {
    validate_root()?;
    if recovery_step(cache_root, &retained.debt)? != RecoveryStep::RemoveRetiredStore {
        return Err(invalid_record(
            "no retired cache store is ready for removal",
        ));
    }
    prepare_tree_witness(cache_root, retained, validate_root)
}

/// Create the replacement cache directory only after the original retired
/// store is absent. The caller must keep the repository-scoped lease through
/// the creation and subsequent debt clearing.
#[cfg(test)]
pub(crate) fn create_fresh_store(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
) -> io::Result<DirectoryIdentity> {
    create_fresh_store_with_root_guard(cache_root, retained, &mut || Ok(()))
}

fn create_fresh_store_with_root_guard(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<DirectoryIdentity> {
    validate_root()?;
    retained.debt.validate()?;
    let root = open_cache_root(cache_root)?;
    verify_retained_sidecar(&root, retained)?;
    if recovery_step_at(&root, &retained.debt)? != RecoveryStep::CreateFreshStore {
        return Err(invalid_record(
            "rotation is not ready for a fresh cache store",
        ));
    }
    let proof = read_fresh_stage_proof(&root, &retained.debt.repository)?
        .ok_or_else(|| invalid_record("fresh replacement has no durable stage proof"))?;
    validate_fresh_stage_state(&root, &proof)?;
    let stage_name = RotationDebt::fresh_stage_component(&retained.debt.repository)?;
    let current_name = component(&retained.debt.repository)?;
    let retired_name = component(retained.debt.retired_component())?;
    if entry_exists(root.as_raw_fd(), &current_name)?
        || entry_exists(root.as_raw_fd(), &retired_name)?
    {
        return Err(invalid_record(
            "fresh replacement paths changed before installation",
        ));
    }
    let stage = open_directory_at(root.as_raw_fd(), &stage_name)?;
    let stage_identity = identity_for_directory(&stage)?;
    if !stage_identity.same_directory(&proof.fresh_store)
        || !stage_identity.same_filesystem(&retained.debt.cache_root)
        || !directory_is_empty(&stage)?
    {
        return Err(invalid_record(
            "staged replacement changed before installation",
        ));
    }
    validate_fresh_stage_state(&root, &proof)?;
    validate_root()?;
    rename_noreplace(
        root.as_raw_fd(),
        &stage_name,
        root.as_raw_fd(),
        &current_name,
    )?;
    root.sync_all()?;

    let fresh = open_directory_at(root.as_raw_fd(), &current_name)?;
    let identity = identity_for_directory(&fresh)?;
    if !identity.same_directory(&proof.fresh_store)
        || !identity.same_filesystem(&retained.debt.cache_root)
        || !directory_is_empty(&fresh)?
        || entry_exists(root.as_raw_fd(), &stage_name)?
    {
        return Err(invalid_record(
            "installed cache store does not match its staged identity",
        ));
    }
    if recovery_step_at(&root, &retained.debt)? != RecoveryStep::ClearCompletedDebt {
        return Err(invalid_record(
            "fresh cache store changed before debt clearing",
        ));
    }
    Ok(identity)
}

fn recovery_step_at(root: &File, debt: &RotationDebt) -> io::Result<RecoveryStep> {
    debt.validate()?;
    let root_identity = identity_for_directory(root)?;
    if !root_identity.same_directory(&debt.cache_root) {
        return Err(invalid_record(
            "cache root changed during rotation recovery",
        ));
    }
    let current_name = component(&debt.repository)?;
    let retired_name = component(&debt.retired_component)?;
    let current = directory_identity_at(root.as_raw_fd(), &current_name)?;
    let retired = directory_identity_at(root.as_raw_fd(), &retired_name)?;
    if let Some(proof) = read_fresh_stage_proof(root, &debt.repository)? {
        validate_fresh_stage_state(root, &proof)?;
        if current
            .as_ref()
            .is_some_and(|identity| identity.same_directory(&proof.fresh_store))
            && retired.is_none()
        {
            return Ok(RecoveryStep::ClearCompletedDebt);
        }
        if current.is_none()
            && retired.is_none()
            && directory_identity_at(
                root.as_raw_fd(),
                &RotationDebt::fresh_stage_component(&debt.repository)?,
            )?
            .is_some()
        {
            return Ok(RecoveryStep::CreateFreshStore);
        }
    }
    let fresh_empty = match (&current, &retired) {
        (Some(identity), None)
            if !identity.same_directory(&debt.original_store)
                && identity.same_filesystem(&debt.cache_root) =>
        {
            directory_is_empty_at(root.as_raw_fd(), &current_name)?
        }
        _ => false,
    };
    Ok(classify_recovery(
        debt,
        current.as_ref(),
        retired.as_ref(),
        fresh_empty,
    ))
}

fn prepare_tree_witness(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<RetiredTreeWitness> {
    validate_root()?;
    retained.debt.validate()?;
    let root = open_cache_root(cache_root)?;
    verify_retained_sidecar(&root, retained)?;
    let step = recovery_step_at(&root, &retained.debt)?;
    let (store_name, is_original_at_current) = match step {
        RecoveryStep::RenameOriginalStore => (component(&retained.debt.repository)?, true),
        RecoveryStep::RemoveRetiredStore => (component(&retained.debt.retired_component)?, false),
        _ => return Err(invalid_record("rotation has no removable original tree")),
    };
    let directory = open_directory_at(root.as_raw_fd(), &store_name)?;
    let directory_identity = identity_for_directory(&directory)?;
    if !directory_identity.same_directory(&retained.debt.original_store)
        || !directory_identity.same_filesystem(&retained.debt.cache_root)
    {
        return Err(invalid_record(
            "rotation tree identity changed before witness setup",
        ));
    }

    let witness_name = retained.debt.tree_witness_component()?;
    let scratch_name = retained.debt.tree_witness_scratch_component()?;
    let bytes = encode(&retained.debt)?;
    let witness = read_component(directory.as_raw_fd(), &witness_name)?;
    let scratch = read_component(directory.as_raw_fd(), &scratch_name)?;

    if let Some(witness) = witness {
        if scratch.is_some() || witness.bytes != bytes {
            return Err(invalid_record(
                "retired cache has an ambiguous or mismatched debt witness",
            ));
        }
    } else {
        let scratch = match scratch {
            Some(scratch) => {
                if !is_original_at_current
                    && !directory_contains_only(&directory, Some(&scratch_name))?
                {
                    return Err(invalid_record(
                        "missing retired-tree witness is not the only remaining entry",
                    ));
                }
                if scratch.bytes != bytes {
                    verify_named_rotation_tree(
                        &root,
                        retained,
                        step,
                        &store_name,
                        &directory_identity,
                    )?;
                    validate_root()?;
                    remove_scratch(directory.as_raw_fd(), &scratch_name, &scratch)?;
                    directory.sync_all()?;
                    None
                } else {
                    Some(scratch)
                }
            }
            None => None,
        };

        if let Some(scratch) = scratch {
            promote_tree_witness(
                TreeWitnessTarget {
                    root: &root,
                    retained,
                    step,
                    store_name: &store_name,
                    directory: &directory,
                    directory_identity: &directory_identity,
                },
                &scratch_name,
                &witness_name,
                &bytes,
                &scratch,
                validate_root,
            )?;
        } else {
            if !is_original_at_current && !directory_is_empty(&directory)? {
                return Err(invalid_record(
                    "missing witness in a nonempty retired cache tree",
                ));
            }
            publish_tree_witness(
                TreeWitnessTarget {
                    root: &root,
                    retained,
                    step,
                    store_name: &store_name,
                    directory: &directory,
                    directory_identity: &directory_identity,
                },
                &scratch_name,
                &witness_name,
                &bytes,
                validate_root,
            )?;
        }
    }

    verify_named_rotation_tree(&root, retained, step, &store_name, &directory_identity)?;
    let final_witness = read_component(directory.as_raw_fd(), &witness_name)?
        .ok_or_else(|| invalid_record("retired cache debt witness disappeared"))?;
    if final_witness.bytes != bytes
        || read_component(directory.as_raw_fd(), &scratch_name)?.is_some()
    {
        return Err(invalid_record("retired cache debt witness changed"));
    }
    Ok(RetiredTreeWitness {
        component: witness_name
            .to_str()
            .map_err(|_| invalid_record("witness component is not UTF-8"))?
            .to_owned(),
        bytes,
        directory: directory_identity,
    })
}

#[derive(Clone, Copy)]
struct TreeWitnessTarget<'a> {
    root: &'a File,
    retained: &'a RetainedRotationDebt,
    step: RecoveryStep,
    store_name: &'a CStr,
    directory: &'a File,
    directory_identity: &'a DirectoryIdentity,
}

fn publish_tree_witness(
    target: TreeWitnessTarget<'_>,
    scratch_name: &CStr,
    witness_name: &CStr,
    bytes: &[u8],
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let TreeWitnessTarget {
        root,
        retained,
        step,
        store_name,
        directory,
        directory_identity,
    } = target;
    verify_named_rotation_tree(root, retained, step, store_name, directory_identity)?;
    validate_root()?;
    let mut scratch = create_regular_exclusive(directory.as_raw_fd(), scratch_name)?;
    validate_root()?;
    scratch.write_all(bytes)?;
    scratch.sync_all()?;
    let metadata = scratch.metadata()?;
    if !is_private_owned_file(&metadata) || metadata.len() != bytes.len() as u64 {
        return Err(invalid_record(
            "new retired-tree witness scratch is invalid",
        ));
    }
    let identity = FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    drop(scratch);

    verify_named_rotation_tree(root, retained, step, store_name, directory_identity)?;
    let checked = read_component(directory.as_raw_fd(), scratch_name)?
        .ok_or_else(|| invalid_record("retired-tree witness scratch disappeared"))?;
    if checked.identity != identity || checked.bytes != bytes {
        return Err(invalid_record("retired-tree witness scratch changed"));
    }
    ensure_component_identity(directory.as_raw_fd(), scratch_name, identity)?;
    directory.sync_all()?;
    validate_root()?;
    rename_noreplace(
        directory.as_raw_fd(),
        scratch_name,
        directory.as_raw_fd(),
        witness_name,
    )?;
    directory.sync_all()?;
    verify_tree_witness_bytes(directory, witness_name, bytes)
}

fn promote_tree_witness(
    target: TreeWitnessTarget<'_>,
    scratch_name: &CStr,
    witness_name: &CStr,
    bytes: &[u8],
    expected: &ReadComponent,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let TreeWitnessTarget {
        root,
        retained,
        step,
        store_name,
        directory,
        directory_identity,
    } = target;
    verify_named_rotation_tree(root, retained, step, store_name, directory_identity)?;
    let scratch = read_component(directory.as_raw_fd(), scratch_name)?
        .ok_or_else(|| invalid_record("retired-tree witness scratch disappeared"))?;
    if scratch.identity != expected.identity || scratch.bytes != bytes {
        return Err(invalid_record("retired-tree witness scratch is ambiguous"));
    }
    ensure_component_identity(directory.as_raw_fd(), scratch_name, scratch.identity)?;
    directory.sync_all()?;
    validate_root()?;
    rename_noreplace(
        directory.as_raw_fd(),
        scratch_name,
        directory.as_raw_fd(),
        witness_name,
    )?;
    directory.sync_all()?;
    verify_tree_witness_bytes(directory, witness_name, bytes)
}

fn verify_tree_witness(
    directory: &File,
    retained: &RetainedRotationDebt,
    expected: &RetiredTreeWitness,
) -> io::Result<()> {
    let name = retained.debt.tree_witness_component()?;
    if expected.component.as_bytes() != name.to_bytes()
        || !expected
            .directory
            .same_directory(&retained.debt.original_store)
    {
        return Err(invalid_record(
            "retired-tree witness binding is inconsistent",
        ));
    }
    verify_tree_witness_bytes(directory, &name, &expected.bytes)
}

fn verify_tree_witness_bytes(directory: &File, name: &CStr, expected: &[u8]) -> io::Result<()> {
    let file = read_component(directory.as_raw_fd(), name)?
        .ok_or_else(|| invalid_record("retired-tree witness disappeared"))?;
    if file.bytes != expected {
        return Err(invalid_record("retired-tree witness payload changed"));
    }
    Ok(())
}

fn verify_retained_sidecar(root: &File, retained: &RetainedRotationDebt) -> io::Result<()> {
    retained.debt.validate()?;
    let root_identity = identity_for_directory(root)?;
    if !root_identity.same_directory(&retained.debt.cache_root) {
        return Err(invalid_record(
            "cache root changed during rotation recovery",
        ));
    }
    let committed_name = RotationDebt::record_component(&retained.debt.repository)?;
    let scratch_name = RotationDebt::scratch_component(&retained.debt.repository)?;
    let committed = read_component(root.as_raw_fd(), &committed_name)?
        .ok_or_else(|| invalid_record("committed rotation debt disappeared"))?;
    if committed.identity != retained.marker || committed.bytes != encode(&retained.debt)? {
        return Err(invalid_record("committed rotation debt changed"));
    }
    if read_component(root.as_raw_fd(), &scratch_name)?.is_some() {
        return Err(invalid_record(
            "unpublished sidecar scratch conflicts with committed rotation debt",
        ));
    }
    ensure_component_identity(root.as_raw_fd(), &committed_name, retained.marker)
}

fn verify_named_rotation_tree(
    root: &File,
    retained: &RetainedRotationDebt,
    expected_step: RecoveryStep,
    store_name: &CStr,
    expected_identity: &DirectoryIdentity,
) -> io::Result<()> {
    verify_retained_sidecar(root, retained)?;
    if recovery_step_at(root, &retained.debt)? != expected_step {
        return Err(invalid_record(
            "rotation state changed during witness setup",
        ));
    }
    let named = directory_identity_at(root.as_raw_fd(), store_name)?
        .ok_or_else(|| invalid_record("rotation tree disappeared during witness setup"))?;
    if !named.same_directory(expected_identity) {
        return Err(invalid_record(
            "rotation tree name changed during witness setup",
        ));
    }
    Ok(())
}

/// Read the committed witness. A scratch file is not authority: publication
/// becomes real only when the fully-synced scratch is atomically renamed to
/// the committed name.
pub(crate) fn read(
    cache_root: &Path,
    repository: &str,
) -> io::Result<Option<RetainedRotationDebt>> {
    let root = open_cache_root(cache_root)?;
    let root_identity = identity_for_directory(&root)?;
    let committed_name = RotationDebt::record_component(repository)?;
    let Some(file) = read_component(root.as_raw_fd(), &committed_name)? else {
        // A scratch file has no authority until atomic publication.
        return Ok(None);
    };
    let scratch_name = RotationDebt::scratch_component(repository)?;
    if read_component(root.as_raw_fd(), &scratch_name)?.is_some() {
        return Err(invalid_record(
            "committed rotation witness conflicts with an unpublished scratch",
        ));
    }
    decode_retained(file, repository, &root_identity).map(Some)
}

/// Read a committed debt, or promote a fully validated pre-publication
/// scratch record. A malformed scratch may only be discarded by `publish`
/// after it proves the original store is still at its exact source inode and
/// no retired or committed state exists.
fn read_or_promote_scratch_with_root_guard(
    cache_root: &Path,
    repository: &str,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<Option<RetainedRotationDebt>> {
    match std::fs::symlink_metadata(cache_root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(invalid_record(
                "preparation cache root is not a real directory",
            ));
        }
        Ok(_) => {}
    }
    if let Some(retained) = read(cache_root, repository)? {
        return Ok(Some(retained));
    }

    let root = open_cache_root(cache_root)?;
    let root_identity = identity_for_directory(&root)?;
    let scratch_name = RotationDebt::scratch_component(repository)?;
    let Some(scratch) = read_component(root.as_raw_fd(), &scratch_name)? else {
        return Ok(None);
    };
    match serde_json::from_slice::<RotationDebt>(&scratch.bytes) {
        Ok(debt) => {
            debt.validate()?;
            if debt.repository != repository
                || !debt.cache_root.same_directory(&root_identity)
                || scratch.bytes != encode(&debt)?
            {
                return Err(invalid_record(
                    "scratch rotation witness does not match its cache root and repository",
                ));
            }
            validate_root()?;
            publish_with_root_guard(cache_root, debt, validate_root).map(Some)
        }
        Err(_) => {
            let store_name = component(repository)?;
            let original_store = directory_identity_at(root.as_raw_fd(), &store_name)?
                .ok_or_else(|| invalid_record("malformed scratch has no original cache store"))?;
            let proposed = RotationDebt::new(
                repository,
                &new_transaction_id(),
                root_identity,
                original_store,
            )?;
            validate_root()?;
            publish_with_root_guard(cache_root, proposed, validate_root).map(Some)
        }
    }
}

fn has_fixed_retired_store(cache_root: &Path, repository: &str) -> io::Result<bool> {
    match std::fs::symlink_metadata(cache_root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(invalid_record(
                "preparation cache root is not a real directory",
            ));
        }
        Ok(_) => {}
    }
    let root = open_cache_root(cache_root)?;
    entry_exists(
        root.as_raw_fd(),
        &component(&format!("{repository}.retired"))?,
    )
}

fn directory_names(
    directory: &File,
    max_entries: usize,
    deadline: Instant,
) -> io::Result<(Vec<OsString>, bool)> {
    let dot = CString::new(".").expect("constant is a valid C string");
    // SAFETY: directory is live; openat returns an independent descriptor for
    // the same pinned directory without traversing any child component.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), dot.as_ptr(), DIRECTORY_FLAGS) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is readable and uniquely transferred to fdopendir on success.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not take ownership of fd.
        unsafe { libc::close(fd) };
        return Err(error);
    }

    let mut names = Vec::new();
    loop {
        if Instant::now() >= deadline || names.len() >= max_entries {
            // SAFETY: stream is the unique live DIR* returned above.
            unsafe { libc::closedir(stream) };
            return Ok((names, false));
        }
        // SAFETY: errno is thread-local and writable before the readdir call.
        unsafe { *directory_errno_location() = 0 };
        // SAFETY: stream remains live and is closed only after copying the name.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            // SAFETY: errno is thread-local and readable on this thread.
            let errno = unsafe { *directory_errno_location() };
            // SAFETY: stream is uniquely owned and closed exactly once.
            unsafe { libc::closedir(stream) };
            if errno != 0 {
                return Err(io::Error::from_raw_os_error(errno));
            }
            return Ok((names, true));
        }
        // SAFETY: d_name is NUL-terminated and valid until the next readdir;
        // copy it before advancing the stream.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
}

/// Publish a debt record by writing an exclusive scratch file, syncing it,
/// and atomically publishing it with a no-replace rename. A crash before that
/// rename leaves only non-authoritative scratch. A valid scratch is promoted
/// on retry; an incomplete one may be discarded only while the original store
/// remains at its source name and no retired or committed witness exists.
///
/// The caller must hold the same repository-scoped preparation-cache lease
/// across source validation, publication, and rotation.
#[cfg(test)]
pub(crate) fn publish(
    cache_root: &Path,
    proposed: RotationDebt,
) -> io::Result<RetainedRotationDebt> {
    publish_with_root_guard(cache_root, proposed, &mut || Ok(()))
}

pub(crate) fn publish_with_root_guard(
    cache_root: &Path,
    proposed: RotationDebt,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<RetainedRotationDebt> {
    proposed.validate()?;
    validate_root()?;
    let root = open_cache_root(cache_root)?;
    let root_identity = identity_for_directory(&root)?;
    if !root_identity.same_directory(&proposed.cache_root) {
        return Err(invalid_record("cache root changed before debt publication"));
    }
    let current_name = component(&proposed.repository)?;
    let current_store =
        directory_identity_at(root.as_raw_fd(), &current_name)?.ok_or_else(|| {
            invalid_record("original cache store disappeared before debt publication")
        })?;
    if !current_store.same_directory(&proposed.original_store) {
        return Err(invalid_record(
            "original cache store changed before debt publication",
        ));
    }

    let committed_name = RotationDebt::record_component(&proposed.repository)?;
    let scratch_name = RotationDebt::scratch_component(&proposed.repository)?;
    let retired_name = component(&proposed.retired_component)?;
    if entry_exists(root.as_raw_fd(), &retired_name)? {
        return Err(invalid_record("retired cache path already exists"));
    }

    let committed = read_component(root.as_raw_fd(), &committed_name)?;
    let scratch = read_component(root.as_raw_fd(), &scratch_name)?;
    if let Some(existing) = committed {
        let retained = decode_retained(existing, &proposed.repository, &root_identity)?;
        if retained.debt.original_store != proposed.original_store {
            return Err(invalid_record(
                "a different rotation debt is already recorded",
            ));
        }
        if scratch.is_some() {
            return Err(invalid_record(
                "committed rotation witness conflicts with an unpublished scratch",
            ));
        }
        return Ok(retained);
    }

    if let Some(scratch) = scratch {
        match serde_json::from_slice::<RotationDebt>(&scratch.bytes) {
            Ok(debt) => {
                debt.validate()?;
                if debt.repository != proposed.repository
                    || !debt.cache_root.same_directory(&root_identity)
                    || debt.original_store != proposed.original_store
                    || scratch.bytes != encode(&debt)?
                {
                    return Err(invalid_record(
                        "scratch rotation witness does not match the unchanged source store",
                    ));
                }
                ensure_original_source_unchanged(
                    root.as_raw_fd(),
                    &proposed,
                    &current_name,
                    &retired_name,
                    &committed_name,
                )?;
                ensure_fresh_stage_with_root_guard(cache_root, &proposed, false, validate_root)?;
                ensure_original_source_unchanged(
                    root.as_raw_fd(),
                    &proposed,
                    &current_name,
                    &retired_name,
                    &committed_name,
                )?;
                ensure_component_identity(root.as_raw_fd(), &scratch_name, scratch.identity)?;
                validate_root()?;
                rename_noreplace(
                    root.as_raw_fd(),
                    &scratch_name,
                    root.as_raw_fd(),
                    &committed_name,
                )?;
                root.sync_all()?;
                let file = read_component(root.as_raw_fd(), &committed_name)?
                    .ok_or_else(|| invalid_record("published rotation witness disappeared"))?;
                if file.identity != scratch.identity || file.bytes != scratch.bytes {
                    return Err(invalid_record("published rotation witness was replaced"));
                }
                return decode_retained(file, &proposed.repository, &root_identity);
            }
            Err(_) => {
                // A syntactically incomplete scratch was never published, so
                // rotation could not have started. Revalidate source and all
                // publication names before reclaiming this exact private file.
                ensure_original_source_unchanged(
                    root.as_raw_fd(),
                    &proposed,
                    &current_name,
                    &retired_name,
                    &committed_name,
                )?;
                validate_root()?;
                remove_scratch(root.as_raw_fd(), &scratch_name, &scratch)?;
                root.sync_all()?;
            }
        }
    }

    ensure_original_source_unchanged(
        root.as_raw_fd(),
        &proposed,
        &current_name,
        &retired_name,
        &committed_name,
    )?;
    ensure_fresh_stage_with_root_guard(cache_root, &proposed, false, validate_root)?;
    ensure_original_source_unchanged(
        root.as_raw_fd(),
        &proposed,
        &current_name,
        &retired_name,
        &committed_name,
    )?;
    let bytes = encode(&proposed)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid_record("rotation debt exceeds its size limit"));
    }
    validate_root()?;
    let mut scratch = create_regular_exclusive(root.as_raw_fd(), &scratch_name)?;
    validate_root()?;
    scratch.write_all(&bytes)?;
    scratch.sync_all()?;
    let metadata = scratch.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || !is_private_owned_file(&metadata)
        || metadata.len() != bytes.len() as u64
    {
        return Err(invalid_record(
            "scratch rotation witness changed while writing",
        ));
    }
    let identity = FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    drop(scratch);
    ensure_original_source_unchanged(
        root.as_raw_fd(),
        &proposed,
        &current_name,
        &retired_name,
        &committed_name,
    )?;
    ensure_component_identity(root.as_raw_fd(), &scratch_name, identity)?;
    validate_root()?;
    rename_noreplace(
        root.as_raw_fd(),
        &scratch_name,
        root.as_raw_fd(),
        &committed_name,
    )?;
    root.sync_all()?;
    let file = read_component(root.as_raw_fd(), &committed_name)?
        .ok_or_else(|| invalid_record("published rotation witness disappeared"))?;
    if file.identity != identity || file.bytes != bytes {
        return Err(invalid_record("published rotation witness was replaced"));
    }
    decode_retained(file, &proposed.repository, &root_identity)
}

/// Restore the cache-root witness from a previously committed external
/// journal row. The external row is the durable publication authority when a
/// crash occurred before the in-cache witness was published or after it was
/// cleared. The caller holds the repository's exclusive preparation lease.
fn restore_from_journal_with_root_guard(
    cache_root: &Path,
    debt: &RotationDebt,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<RetainedRotationDebt> {
    debt.validate()?;
    validate_root()?;
    let root = open_cache_root(cache_root)?;
    let root_identity = identity_for_directory(&root)?;
    if !root_identity.same_directory(&debt.cache_root) {
        return Err(invalid_record("cache root changed before journal recovery"));
    }
    match recovery_step_at(&root, debt)? {
        RecoveryStep::ClearCompletedDebt => {}
        RecoveryStep::Conflict => {
            return Err(invalid_record(
                "journal rotation debt conflicts with current cache state",
            ));
        }
        RecoveryStep::RenameOriginalStore
        | RecoveryStep::RemoveRetiredStore
        | RecoveryStep::CreateFreshStore => {}
    }

    let committed_name = RotationDebt::record_component(&debt.repository)?;
    let scratch_name = RotationDebt::scratch_component(&debt.repository)?;
    let bytes = encode(debt)?;
    if let Some(existing) = read_component(root.as_raw_fd(), &committed_name)? {
        let retained = decode_retained(existing, &debt.repository, &root_identity)?;
        if retained.debt != *debt || read_component(root.as_raw_fd(), &scratch_name)?.is_some() {
            return Err(invalid_record(
                "journal rotation witness conflicts with in-cache state",
            ));
        }
        return Ok(retained);
    }

    if let Some(scratch) = read_component(root.as_raw_fd(), &scratch_name)? {
        if scratch.bytes == bytes
            && decode_retained(
                ReadComponent {
                    bytes: scratch.bytes.clone(),
                    identity: scratch.identity,
                },
                &debt.repository,
                &root_identity,
            )
            .is_ok()
        {
            ensure_component_identity(root.as_raw_fd(), &scratch_name, scratch.identity)?;
            if recovery_step_at(&root, debt)? == RecoveryStep::Conflict {
                return Err(invalid_record(
                    "journal recovery state changed before scratch promotion",
                ));
            }
            validate_root()?;
            rename_noreplace(
                root.as_raw_fd(),
                &scratch_name,
                root.as_raw_fd(),
                &committed_name,
            )?;
            root.sync_all()?;
            let file = read_component(root.as_raw_fd(), &committed_name)?
                .ok_or_else(|| invalid_record("restored rotation witness disappeared"))?;
            if file.identity != scratch.identity || file.bytes != bytes {
                return Err(invalid_record("restored rotation witness was replaced"));
            }
            return decode_retained(file, &debt.repository, &root_identity);
        }
        // The external row is the exact published intent. A private
        // incomplete scratch file has no authority and can be discarded only
        // after the root and expected recovery state still match.
        if recovery_step_at(&root, debt)? == RecoveryStep::Conflict {
            return Err(invalid_record(
                "journal recovery state changed before scratch cleanup",
            ));
        }
        validate_root()?;
        remove_scratch(root.as_raw_fd(), &scratch_name, &scratch)?;
        root.sync_all()?;
    }

    let step = recovery_step_at(&root, debt)?;
    if matches!(
        step,
        RecoveryStep::Conflict | RecoveryStep::ClearCompletedDebt
    ) {
        return Err(invalid_record(
            "journal recovery state changed before witness creation",
        ));
    }
    validate_root()?;
    let mut scratch = create_regular_exclusive(root.as_raw_fd(), &scratch_name)?;
    validate_root()?;
    scratch.write_all(&bytes)?;
    scratch.sync_all()?;
    let metadata = scratch.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || !is_private_owned_file(&metadata)
        || metadata.len() != bytes.len() as u64
    {
        return Err(invalid_record(
            "restored rotation scratch changed while writing",
        ));
    }
    let identity = FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    drop(scratch);
    if recovery_step_at(&root, debt)? != step {
        return Err(invalid_record(
            "journal recovery state changed before witness publication",
        ));
    }
    ensure_component_identity(root.as_raw_fd(), &scratch_name, identity)?;
    validate_root()?;
    rename_noreplace(
        root.as_raw_fd(),
        &scratch_name,
        root.as_raw_fd(),
        &committed_name,
    )?;
    root.sync_all()?;
    let file = read_component(root.as_raw_fd(), &committed_name)?
        .ok_or_else(|| invalid_record("restored rotation witness disappeared"))?;
    if file.identity != identity || file.bytes != bytes {
        return Err(invalid_record("restored rotation witness was replaced"));
    }
    decode_retained(file, &debt.repository, &root_identity)
}

/// Remove only the exact witness after a descriptor-relative recovery check
/// proves the old tree absent and the fresh store empty. The caller must hold
/// the same per-repository preparation-cache lease used by every preparation
/// writer and rotation recovery operation.
///
/// Classification alone is not mutation authority: this function pins and
/// verifies the cache-root descriptor, repeats the completed-state check,
/// re-reads the exact canonical witness, and checks its inode immediately
/// before descriptor-relative unlink. There is deliberately no rename-to-
/// tombstone step: after the old tree is gone, a crash after unlink but before
/// directory sync can at worst leave an unnecessary witness, never hide old
/// cache data or strand unbounded removal tombstones.
#[cfg(test)]
pub(crate) fn clear_completed(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
) -> io::Result<()> {
    clear_completed_with_root_guard(cache_root, retained, &mut || Ok(()))
}

fn clear_completed_with_root_guard(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    clear_completed_with_sync_and_root_guard(cache_root, retained, File::sync_all, validate_root)
}

#[cfg(test)]
fn clear_completed_with_sync(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
    sync_root: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    clear_completed_with_sync_and_root_guard(cache_root, retained, sync_root, &mut || Ok(()))
}

fn clear_completed_with_sync_and_root_guard(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
    sync_root: impl FnOnce(&File) -> io::Result<()>,
    validate_root: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    validate_root()?;
    retained.debt.validate()?;
    let root = open_cache_root(cache_root)?;
    let root_identity = identity_for_directory(&root)?;
    if !root_identity.same_directory(&retained.debt.cache_root) {
        return Err(invalid_record("cache root changed before debt removal"));
    }
    let name = RotationDebt::record_component(&retained.debt.repository)?;
    let scratch_name = RotationDebt::scratch_component(&retained.debt.repository)?;
    if read_component(root.as_raw_fd(), &scratch_name)?.is_some() {
        return Err(invalid_record(
            "unpublished scratch prevents clearing completed rotation debt",
        ));
    }
    if recovery_step_at(&root, &retained.debt)? != RecoveryStep::ClearCompletedDebt {
        return Err(invalid_record("rotation debt is not in a clearable state"));
    }
    let current = read_component(root.as_raw_fd(), &name)?
        .ok_or_else(|| invalid_record("rotation witness disappeared before removal"))?;
    if current.identity != retained.marker || current.bytes != encode(&retained.debt)? {
        return Err(invalid_record("rotation witness changed before removal"));
    }
    if read_component(root.as_raw_fd(), &scratch_name)?.is_some()
        || recovery_step_at(&root, &retained.debt)? != RecoveryStep::ClearCompletedDebt
    {
        return Err(invalid_record(
            "rotation state changed before completed witness removal",
        ));
    }
    let final_witness = read_component(root.as_raw_fd(), &name)?
        .ok_or_else(|| invalid_record("rotation witness disappeared before removal"))?;
    if final_witness.identity != retained.marker || final_witness.bytes != encode(&retained.debt)? {
        return Err(invalid_record("rotation witness changed before removal"));
    }
    ensure_component_identity(root.as_raw_fd(), &name, retained.marker)?;
    validate_root()?;
    unlink_component(root.as_raw_fd(), &name)?;
    sync_root(&root)
}

pub(crate) fn encode(debt: &RotationDebt) -> io::Result<Vec<u8>> {
    serde_json::to_vec(debt)
        .map_err(|error| invalid_record(&format!("could not encode debt: {error}")))
}

fn decode_retained(
    file: ReadComponent,
    repository: &str,
    root_identity: &DirectoryIdentity,
) -> io::Result<RetainedRotationDebt> {
    let debt: RotationDebt = serde_json::from_slice(&file.bytes)
        .map_err(|error| invalid_record(&format!("rotation witness is malformed: {error}")))?;
    debt.validate()?;
    if debt.repository != repository
        || !debt.cache_root.same_directory(root_identity)
        || file.bytes != encode(&debt)?
    {
        return Err(invalid_record(
            "rotation witness does not match its location",
        ));
    }
    Ok(RetainedRotationDebt {
        debt,
        marker: file.identity,
    })
}

struct ReadComponent {
    bytes: Vec<u8>,
    identity: FileIdentity,
}

fn read_component(parent_fd: RawFd, name: &CStr) -> io::Result<Option<ReadComponent>> {
    // SAFETY: parent_fd is a live directory fd and name is a NUL-terminated
    // single component. openat returns a new descriptor on success.
    let fd = unsafe {
        libc::openat(
            parent_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error);
    }
    // SAFETY: fd is the fresh descriptor returned by the successful openat
    // above; ownership is transferred to File exactly once.
    let file = unsafe { File::from_raw_fd(fd) };
    let before = file.metadata()?;
    if !is_private_owned_file(&before) {
        return Err(invalid_record(
            "rotation witness must be a private, owner-controlled regular file",
        ));
    }
    if before.len() > MAX_RECORD_BYTES {
        return Err(invalid_record("rotation witness exceeds its size limit"));
    }
    let identity = FileIdentity {
        device: before.dev(),
        inode: before.ino(),
    };
    let mut reader = file.take(MAX_RECORD_BYTES + 1);
    let mut bytes = Vec::with_capacity(before.len() as usize);
    reader.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid_record("rotation witness exceeds its size limit"));
    }
    let after = reader.get_ref().metadata()?;
    if !is_private_owned_file(&after)
        || after.dev() != identity.device
        || after.ino() != identity.inode
        || after.len() != bytes.len() as u64
        || before.len() != bytes.len() as u64
    {
        return Err(invalid_record("rotation witness changed while being read"));
    }
    Ok(Some(ReadComponent { bytes, identity }))
}

fn create_regular_exclusive(parent_fd: RawFd, name: &CStr) -> io::Result<File> {
    // SAFETY: parent_fd is a live directory fd and name is a NUL-terminated
    // single component. O_EXCL creates a new descriptor without following links.
    let fd = unsafe {
        libc::openat(
            parent_fd,
            name.as_ptr(),
            libc::O_WRONLY
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
            0o600 as libc::mode_t as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is the fresh descriptor returned by the successful openat
    // above; ownership is transferred to File exactly once.
    let file = unsafe { File::from_raw_fd(fd) };
    normalize_created_file_mode(&file, 0o600 as libc::mode_t)?;
    let metadata = file.metadata()?;
    if !is_private_owned_file(&metadata) {
        return Err(invalid_record(
            "new rotation witness is not a private regular file",
        ));
    }
    Ok(file)
}

fn normalize_created_file_mode(file: &File, mode: libc::mode_t) -> io::Result<()> {
    // SAFETY: file is a live descriptor for an entry created by this call.
    if unsafe { libc::fchmod(file.as_raw_fd(), mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let metadata = file.metadata()?;
    if !is_private_owned_file(&metadata) || u64::from(metadata.mode() & 0o777) != u64::from(mode) {
        return Err(invalid_record(
            "new private storage file did not retain its required owner mode",
        ));
    }
    Ok(())
}

fn normalize_created_directory_mode(
    parent_fd: RawFd,
    name: &CStr,
    mode: libc::mode_t,
) -> io::Result<()> {
    let before = stat_component(parent_fd, name)?;
    if before.st_mode & libc::S_IFMT != libc::S_IFDIR || before.st_uid != current_effective_uid() {
        return Err(invalid_record(
            "new preparation-cache namespace is not an owner-controlled directory",
        ));
    }
    // SAFETY: parent_fd is pinned and name is a single component. The
    // no-follow flag prevents changing a replacement symlink target.
    if unsafe { libc::fchmodat(parent_fd, name.as_ptr(), mode, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let after = stat_component(parent_fd, name)?;
    if after.st_mode & libc::S_IFMT != libc::S_IFDIR
        || after.st_uid != current_effective_uid()
        || after.st_dev != before.st_dev
        || after.st_ino != before.st_ino
        || after.st_mode & 0o777 != mode
    {
        return Err(invalid_record(
            "new preparation-cache namespace identity or owner mode changed",
        ));
    }
    Ok(())
}

fn stat_component(parent_fd: RawFd, name: &CStr) -> io::Result<libc::stat> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: parent_fd is live and name is a single NUL-terminated
    // component; stat points to writable storage.
    if unsafe {
        libc::fstatat(
            parent_fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatat returned success and initialized the result.
    Ok(unsafe { stat.assume_init() })
}

fn open_cache_root(path: &Path) -> io::Result<File> {
    let directory = open_directory(path)?;
    if !is_owned_cache_directory(&directory.metadata()?) {
        return Err(invalid_record(
            "preparation cache root must be user-owned and not group/world-writable",
        ));
    }
    Ok(directory)
}

fn open_directory(path: &Path) -> io::Result<File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| invalid_record("cache root path contains NUL"))?;
    // SAFETY: path is a live NUL-terminated CString and open returns a new fd.
    let fd = unsafe { libc::open(path.as_ptr(), DIRECTORY_FLAGS) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is the fresh descriptor returned by successful open above.
    let directory = unsafe { File::from_raw_fd(fd) };
    if !directory.metadata()?.is_dir() {
        return Err(invalid_record("cache root is not a directory"));
    }
    Ok(directory)
}

fn open_directory_at(parent_fd: RawFd, name: &CStr) -> io::Result<File> {
    // SAFETY: parent_fd is a live directory fd and name is a NUL-terminated
    // single component. openat returns a new fd on success.
    let fd = unsafe { libc::openat(parent_fd, name.as_ptr(), DIRECTORY_FLAGS) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is the fresh descriptor returned by successful openat above.
    let directory = unsafe { File::from_raw_fd(fd) };
    if !directory.metadata()?.is_dir() {
        return Err(invalid_record("cache entry is not a directory"));
    }
    Ok(directory)
}

fn directory_is_empty_at(parent_fd: RawFd, name: &CStr) -> io::Result<bool> {
    let directory = open_directory_at(parent_fd, name)?;
    directory_is_empty(&directory)
}

fn directory_is_empty(directory: &File) -> io::Result<bool> {
    directory_contains_only(directory, None)
}

/// Return true when the pinned directory has no entries except an optional
/// single known component. The directory stream is opened relative to the
/// retained descriptor so path replacement cannot redirect this check.
fn directory_contains_only(directory: &File, allowed: Option<&CStr>) -> io::Result<bool> {
    let allowed = allowed.map(CStr::to_bytes).into_iter().collect::<Vec<_>>();
    directory_contains_only_names(directory, &allowed)
}

fn preparation_root_contains_only_names(
    directory: &File,
    allowed: &[&[u8]],
    deadline: Instant,
) -> io::Result<bool> {
    if allowed.len() > PREPARATION_ROOT_MAX_ENTRIES {
        return Err(invalid_record(
            "preparation-cache root entry allowlist exceeds its bound",
        ));
    }
    let dot = CString::new(".").expect("constant is a valid C string");
    // SAFETY: directory is a live directory fd and dot is one component.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), dot.as_ptr(), DIRECTORY_FLAGS) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fdopendir takes ownership of the successfully opened fd.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume fd.
        unsafe { libc::close(fd) };
        return Err(error);
    }
    let mut found = Vec::<Vec<u8>>::with_capacity(allowed.len());
    loop {
        if Instant::now() >= deadline {
            // SAFETY: stream is still live and is closed exactly once.
            unsafe { libc::closedir(stream) };
            return Err(deadline_error());
        }
        // SAFETY: errno is the current thread's writable error slot.
        unsafe { *directory_errno_location() = 0 };
        // SAFETY: stream is live and readdir returns storage owned by stream.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            // SAFETY: errno is the current thread's readable error slot.
            let errno = unsafe { *directory_errno_location() };
            // SAFETY: stream is live and is closed exactly once.
            let close_result = unsafe { libc::closedir(stream) };
            if errno != 0 {
                return Err(io::Error::from_raw_os_error(errno));
            }
            if close_result != 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(true);
        }
        // SAFETY: d_name is NUL-terminated storage owned by the live stream.
        let entry_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if entry_name == b"." || entry_name == b".." {
            continue;
        }
        if found.len() >= allowed.len()
            || !allowed.contains(&entry_name)
            || found.iter().any(|known| known.as_slice() == entry_name)
        {
            // SAFETY: stream is live and is closed exactly once.
            let close_result = unsafe { libc::closedir(stream) };
            if close_result != 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(false);
        }
        found.push(entry_name.to_vec());
    }
}

fn directory_contains_only_names(directory: &File, allowed: &[&[u8]]) -> io::Result<bool> {
    let dot = CString::new(".").expect("constant is a valid C string");
    // SAFETY: directory is a live directory descriptor and dot is a single
    // NUL-terminated component. openat returns an independent descriptor.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), dot.as_ptr(), DIRECTORY_FLAGS) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a readable directory descriptor transferred from File;
    // fdopendir owns it on success.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir failed and therefore did not consume fd.
        unsafe { libc::close(fd) };
        return Err(error);
    }
    loop {
        // SAFETY: the returned errno pointer is thread-local and writable.
        unsafe { *directory_errno_location() = 0 };
        // SAFETY: stream is the live DIR* returned by fdopendir and is closed
        // only after this entry has been consumed.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            // SAFETY: errno pointer is thread-local and readable on this thread.
            let errno = unsafe { *directory_errno_location() };
            // SAFETY: stream is the live DIR* returned by fdopendir and is
            // closed exactly once here.
            unsafe { libc::closedir(stream) };
            if errno != 0 {
                return Err(io::Error::from_raw_os_error(errno));
            }
            return Ok(true);
        }
        // SAFETY: entry points into stream-owned readdir storage and d_name is
        // NUL-terminated; the bytes are copied/inspected before the next call.
        let entry_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if entry_name != b"." && entry_name != b".." && !allowed.contains(&entry_name) {
            // SAFETY: stream is the live DIR* returned by fdopendir and is
            // closed exactly once here.
            unsafe { libc::closedir(stream) };
            return Ok(false);
        }
    }
}

#[cfg(target_os = "linux")]
fn directory_errno_location() -> *mut libc::c_int {
    // SAFETY: libc returns the current thread's errno slot.
    unsafe { libc::__errno_location() }
}

#[cfg(target_os = "macos")]
fn directory_errno_location() -> *mut libc::c_int {
    // SAFETY: libc returns the current thread's errno slot.
    unsafe { libc::__error() }
}

static NEXT_TRANSACTION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn new_transaction_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default();
    format!(
        "{nanos:016x}{:08x}{:08x}",
        std::process::id(),
        NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed) as u32
    )
}

fn identity_for_directory(directory: &File) -> io::Result<DirectoryIdentity> {
    let metadata = directory.metadata()?;
    if !is_owned_cache_directory(&metadata) {
        return Err(invalid_record(
            "cache directory must be user-owned and not group/world-writable",
        ));
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        filesystem_id: filesystem_id(directory.as_raw_fd())?,
    })
}

fn directory_identity_at(parent_fd: RawFd, name: &CStr) -> io::Result<Option<DirectoryIdentity>> {
    match open_directory_at(parent_fd, name) {
        Ok(directory) => identity_for_directory(&directory).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn filesystem_id(fd: RawFd) -> io::Result<String> {
    let mut stat = MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: fd is a live directory descriptor and stat points to writable
    // storage for the complete statfs result.
    if unsafe { libc::fstatfs(fd, stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatfs returned success and initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    let fsid = std::ptr::addr_of!(stat.f_fsid).cast::<u8>();
    // SAFETY: fsid points to the live f_fsid field and the length is exactly
    // that field's initialized size; the slice is read-only.
    let bytes = unsafe { std::slice::from_raw_parts(fsid, mem::size_of_val(&stat.f_fsid)) };
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(encoded)
}

fn entry_exists(parent_fd: RawFd, name: &CStr) -> io::Result<bool> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: parent_fd is a live directory descriptor; name is a valid
    // component and stat points to writable storage for fstatat's result.
    if unsafe {
        libc::fstatat(
            parent_fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err(error)
    }
}

fn ensure_original_source_unchanged(
    root_fd: RawFd,
    debt: &RotationDebt,
    current_name: &CStr,
    retired_name: &CStr,
    committed_name: &CStr,
) -> io::Result<()> {
    let current = directory_identity_at(root_fd, current_name)?;
    if !current.is_some_and(|identity| identity.same_directory(&debt.original_store))
        || entry_exists(root_fd, retired_name)?
        || entry_exists(root_fd, committed_name)?
    {
        return Err(invalid_record(
            "original cache store changed before rotation debt publication",
        ));
    }
    Ok(())
}

fn current_effective_uid() -> u32 {
    // SAFETY: geteuid has no pointer or memory preconditions.
    unsafe { libc::geteuid() }
}

fn is_private_owned_file(metadata: &std::fs::Metadata) -> bool {
    metadata.is_file()
        && metadata.nlink() == 1
        && metadata.uid() == current_effective_uid()
        && metadata.mode() & 0o077 == 0
}

fn is_owned_cache_directory(metadata: &std::fs::Metadata) -> bool {
    metadata.is_dir() && metadata.uid() == current_effective_uid() && metadata.mode() & 0o022 == 0
}

fn remove_scratch(parent_fd: RawFd, name: &CStr, expected: &ReadComponent) -> io::Result<()> {
    let current = read_component(parent_fd, name)?
        .ok_or_else(|| invalid_record("rotation scratch disappeared before removal"))?;
    if current.identity != expected.identity || current.bytes != expected.bytes {
        return Err(invalid_record("rotation scratch changed before removal"));
    }
    ensure_component_identity(parent_fd, name, expected.identity)?;
    unlink_component(parent_fd, name)
}

fn ensure_component_identity(
    parent_fd: RawFd,
    name: &CStr,
    expected: FileIdentity,
) -> io::Result<()> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: parent_fd is a live directory descriptor; name is a valid
    // component and stat points to writable storage for fstatat's result.
    if unsafe {
        libc::fstatat(
            parent_fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatat returned success and initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    #[cfg(target_os = "linux")]
    let device = stat.st_dev;
    #[cfg(target_os = "macos")]
    let device = u64::try_from(stat.st_dev)
        .map_err(|_| invalid_record("negative device number in rotation witness"))?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_nlink != 1
        || stat.st_uid != current_effective_uid()
        || stat.st_mode & 0o077 != 0
        || device != expected.device
        || stat.st_ino != expected.inode
    {
        return Err(invalid_record("rotation witness identity changed"));
    }
    Ok(())
}

fn rename_noreplace(
    parent_fd: RawFd,
    from: &CStr,
    to_parent_fd: RawFd,
    to: &CStr,
) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    // SAFETY: both directory descriptors are live; from/to are NUL-terminated
    // single components and RENAME_EXCL prevents replacement of the target.
    let result = unsafe {
        libc::renameatx_np(
            parent_fd,
            from.as_ptr(),
            to_parent_fd,
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    // SAFETY: `SYS_renameat2` receives the Linux renameat2 ABI arguments;
    // both directory descriptors are live, `from` and `to` are valid
    // NUL-terminated single components, and RENAME_NOREPLACE forbids replacing
    // an existing target. A syscall failure is returned below without fallback.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent_fd,
            from.as_ptr(),
            to_parent_fd,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn unlink_component(parent_fd: RawFd, name: &CStr) -> io::Result<()> {
    // SAFETY: parent_fd is a live directory descriptor and name is a
    // NUL-terminated single component. Callers revalidate the exact inode and
    // hold the repository-scoped cooperative lease before unlinking.
    if unsafe { libc::unlinkat(parent_fd, name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn component(name: &str) -> io::Result<CString> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(invalid_record("rotation path is not one component"));
    }
    CString::new(name).map_err(|_| invalid_record("rotation path contains NUL"))
}

fn is_hex(value: &str, min_len: usize, max_len: usize) -> bool {
    (min_len..=max_len).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn invalid_record(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        DebtProbe, DirectoryIdentity, RECORD_PREFIX, RecoveryStep, RotationDebt, SCRATCH_SUFFIX,
        classify_recovery, inventory, new_transaction_id, probe, publish, read, recover,
        recovery_step,
    };
    use std::fs;
    use std::path::Path;
    use std::time::Instant;

    const REPOSITORY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const TRANSACTION: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn directory_identity_rejects_a_different_filesystem() {
        let worktree = DirectoryIdentity {
            device: 7,
            inode: 11,
            filesystem_id: "aabb".into(),
        };
        let same_filesystem = DirectoryIdentity {
            device: 7,
            inode: 12,
            filesystem_id: "aabb".into(),
        };
        let different_device = DirectoryIdentity {
            device: 8,
            inode: 12,
            filesystem_id: "aabb".into(),
        };
        let different_filesystem_id = DirectoryIdentity {
            device: 7,
            inode: 12,
            filesystem_id: "ccdd".into(),
        };

        assert!(worktree.same_filesystem(&same_filesystem));
        assert!(!worktree.same_filesystem(&different_device));
        assert!(!worktree.same_filesystem(&different_filesystem_id));
    }

    fn initialize_test_cache_root(
        path: &Path,
        expected_root_identity: &[u8],
        pinned_directory: &std::fs::File,
        journal_has_rows_under_root: impl Fn() -> std::io::Result<bool>,
    ) -> std::io::Result<super::InitializedPreparationCacheRoot> {
        let coordinator_db_path = path
            .parent()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing test parent")
            })?
            .join("host-resources.db");
        super::initialize_preparation_cache_root(
            path,
            expected_root_identity,
            pinned_directory,
            &coordinator_db_path,
            journal_has_rows_under_root,
        )
    }

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, RotationDebt) {
        let temp = tempfile::tempdir().unwrap();
        let cache_root = temp.path().join("preparation-cache");
        fs::create_dir(&cache_root).unwrap();
        let store = cache_root.join(REPOSITORY);
        fs::create_dir(&store).unwrap();
        fs::write(store.join("package-index"), b"cached").unwrap();
        let root = DirectoryIdentity::capture(&cache_root).unwrap();
        let original = DirectoryIdentity::capture(&store).unwrap();
        let debt = RotationDebt::new(REPOSITORY, TRANSACTION, root, original).unwrap();
        (temp, cache_root, debt)
    }

    fn completed_journal_rotation_fixture() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        RotationDebt,
        Vec<u8>,
        Vec<u8>,
    ) {
        use crate::gc::{PinnedTreeRemoval, TreeRemoval};

        let (temp, cache_root, debt) = fixture();
        let resource_db = temp
            .path()
            .canonicalize()
            .unwrap()
            .join("host-resources.db");
        let retained = publish(&cache_root, debt.clone()).unwrap();
        let payload = super::encode(&debt).unwrap();
        let root_identity = debt.cache_root.encode().unwrap();
        crate::preparation::rotation_journal::publish(
            &resource_db,
            &cache_root,
            &root_identity,
            REPOSITORY,
            &payload,
        )
        .unwrap();

        let proof = super::ensure_fresh_stage(&cache_root, &debt, false).unwrap();
        let fresh_store_identity = proof.fresh_store.encode().unwrap();
        crate::preparation::rotation_journal::record_fresh_store_identity(
            &resource_db,
            &cache_root,
            &root_identity,
            REPOSITORY,
            &payload,
            &fresh_store_identity,
        )
        .unwrap();

        super::retire_original_store(&cache_root, &retained).unwrap();
        let witness = super::prepare_retired_store_removal(&cache_root, &retained).unwrap();
        let retired_path = cache_root.join(debt.retired_component());
        let pinned = PinnedTreeRemoval::open(&retired_path).unwrap();
        assert_eq!(
            pinned
                .remove_verified_witness(witness.component().unwrap(), None, |bytes| {
                    witness.validate_bytes(bytes)
                })
                .unwrap(),
            TreeRemoval::Complete
        );
        super::create_fresh_store(&cache_root, &retained).unwrap();
        super::clear_completed(&cache_root, &retained).unwrap();

        assert!(super::read(&cache_root, REPOSITORY).unwrap().is_none());
        assert!(cache_root.join(REPOSITORY).is_dir());
        assert_eq!(
            fs::read_dir(cache_root.join(REPOSITORY)).unwrap().count(),
            0
        );
        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY
            )
            .unwrap()
            .is_some()
        );

        (
            temp,
            cache_root,
            resource_db,
            debt,
            payload,
            fresh_store_identity,
        )
    }

    fn publish_stage_intent(cache_root: &std::path::Path, debt: &RotationDebt) {
        let root = super::open_cache_root(cache_root).unwrap();
        let intent = super::select_fresh_stage_intent(&root, debt, false).unwrap();
        let intent_name = RotationDebt::fresh_stage_intent_component(&debt.repository).unwrap();
        let scratch_name =
            RotationDebt::fresh_stage_intent_scratch_component(&debt.repository).unwrap();
        let bytes = super::encode_fresh_stage_intent(&intent).unwrap();
        super::publish_control_file(&root, &intent_name, &scratch_name, &bytes).unwrap();
    }

    fn assert_rotation_inventory_blocked(cache_root: &std::path::Path) {
        let inventory = inventory(
            cache_root,
            100,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(inventory.complete);
        assert!(inventory.repositories.iter().any(|repo| repo == REPOSITORY));
        assert!(inventory.blocked_repositories.contains(REPOSITORY));
    }

    #[test]
    fn inventory_and_recovery_finish_a_committed_rotation() {
        let (_temp, cache_root, debt) = fixture();
        publish(&cache_root, debt.clone()).unwrap();
        let inventory = inventory(
            &cache_root,
            100,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(inventory.complete);
        assert_eq!(inventory.repositories, vec![REPOSITORY.to_owned()]);
        assert!(inventory.blocked_repositories.contains(REPOSITORY));

        let progress = recover(
            &cache_root,
            REPOSITORY,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(progress.found_debt);
        assert!(progress.complete);
        assert!(progress.entries_reclaimed >= 2);
        assert!(cache_root.join(REPOSITORY).is_dir());
        assert_eq!(
            fs::read_dir(cache_root.join(REPOSITORY)).unwrap().count(),
            0
        );
        assert!(read(&cache_root, REPOSITORY).unwrap().is_none());
    }

    #[test]
    fn journal_recovery_finishes_when_sidecar_was_cleared_before_row_completion() {
        let (_temp, cache_root, resource_db, debt, payload, fresh_identity) =
            completed_journal_rotation_fixture();
        let root_identity = debt.cache_root.encode().unwrap();

        let row = crate::preparation::rotation_journal::load_record(
            &resource_db,
            &cache_root,
            &root_identity,
            REPOSITORY,
        )
        .unwrap()
        .expect("the first crash cut keeps the journal row");
        assert_eq!(row.payload, payload);
        assert_eq!(
            row.fresh_store_identity.as_deref(),
            Some(fresh_identity.as_slice())
        );

        let progress = super::recover_with_journal(
            &cache_root,
            &resource_db,
            REPOSITORY,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();

        assert!(progress.found_debt);
        assert!(progress.complete);
        assert!(
            crate::preparation::rotation_journal::load_record(
                &resource_db,
                &cache_root,
                &root_identity,
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );
        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn journal_recovery_rediscovers_proof_after_row_completion_before_proof_removal() {
        let (_temp, cache_root, resource_db, debt, payload, fresh_identity) =
            completed_journal_rotation_fixture();
        let root_identity = debt.cache_root.encode().unwrap();

        // Model a crash after the external row was removed but before the
        // root-level fresh-store proof could be removed.
        crate::preparation::rotation_journal::complete_verified(
            &resource_db,
            &cache_root,
            &root_identity,
            REPOSITORY,
            &payload,
            &fresh_identity,
        )
        .unwrap();
        assert!(
            crate::preparation::rotation_journal::load_record(
                &resource_db,
                &cache_root,
                &root_identity,
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );
        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY
            )
            .unwrap()
            .is_some()
        );

        let progress = super::recover_with_journal(
            &cache_root,
            &resource_db,
            REPOSITORY,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();

        assert!(progress.found_debt);
        assert!(progress.complete);
        assert!(
            crate::preparation::rotation_journal::load_record(
                &resource_db,
                &cache_root,
                &root_identity,
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );
        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn fresh_stage_proof_is_published_while_the_original_store_is_still_intact() {
        let (_temp, cache_root, debt) = fixture();
        let original_path = cache_root.join(REPOSITORY);
        let original_payload = fs::read(original_path.join("package-index")).unwrap();

        let proof = super::ensure_fresh_stage(&cache_root, &debt, false).unwrap();

        let stage_name = RotationDebt::fresh_stage_component(REPOSITORY).unwrap();
        let stage_path = cache_root.join(stage_name.to_str().unwrap());
        let stage_identity = DirectoryIdentity::capture(&stage_path).unwrap();
        assert!(stage_identity.same_directory(&proof.fresh_store));
        assert!(!proof.fresh_store.same_directory(&debt.original_store));
        assert_eq!(
            DirectoryIdentity::capture(&original_path).unwrap(),
            debt.original_store
        );
        assert_eq!(
            fs::read(original_path.join("package-index")).unwrap(),
            original_payload
        );
        super::validate_fresh_stage_state(&super::open_cache_root(&cache_root).unwrap(), &proof)
            .unwrap();
        assert_rotation_inventory_blocked(&cache_root);
    }

    #[test]
    fn replacement_stage_is_refused_without_deleting_either_directory() {
        let (_temp, cache_root, debt) = fixture();
        let original_path = cache_root.join(REPOSITORY);
        let original_payload = fs::read(original_path.join("package-index")).unwrap();
        let proof = super::ensure_fresh_stage(&cache_root, &debt, false).unwrap();
        let stage_name = RotationDebt::fresh_stage_component(REPOSITORY).unwrap();
        let stage_path = cache_root.join(stage_name.to_str().unwrap());
        let held_stage = fs::File::open(&stage_path).unwrap();
        let old_stage_identity = DirectoryIdentity::capture(&stage_path).unwrap();
        let moved_stage = cache_root.join("held-original-stage");

        fs::rename(&stage_path, &moved_stage).unwrap();
        fs::create_dir(&stage_path).unwrap();
        let replacement_identity = DirectoryIdentity::capture(&stage_path).unwrap();
        assert_ne!(replacement_identity, old_stage_identity);

        assert!(super::ensure_fresh_stage(&cache_root, &debt, false).is_err());
        assert!(
            super::validate_fresh_stage_state(
                &super::open_cache_root(&cache_root).unwrap(),
                &proof,
            )
            .is_err()
        );
        assert_eq!(
            DirectoryIdentity::capture(&stage_path).unwrap(),
            replacement_identity
        );
        assert_eq!(
            DirectoryIdentity::capture(&moved_stage).unwrap(),
            old_stage_identity
        );
        assert_eq!(
            fs::read(original_path.join("package-index")).unwrap(),
            original_payload
        );
        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY,
            )
            .unwrap()
            .is_some()
        );
        assert_rotation_inventory_blocked(&cache_root);
        drop(held_stage);
    }

    #[test]
    fn foreign_transaction_intent_is_refused_without_touching_original_store() {
        let (_temp, cache_root, debt) = fixture();
        let original_path = cache_root.join(REPOSITORY);
        let original_payload = fs::read(original_path.join("package-index")).unwrap();
        let foreign_debt = RotationDebt::new(
            REPOSITORY,
            "1123456789abcdef0123456789abcdef",
            debt.cache_root.clone(),
            debt.original_store.clone(),
        )
        .unwrap();
        publish_stage_intent(&cache_root, &foreign_debt);

        assert!(super::ensure_fresh_stage(&cache_root, &debt, false).is_err());
        let stage_name = RotationDebt::fresh_stage_component(REPOSITORY).unwrap();
        assert!(!cache_root.join(stage_name.to_str().unwrap()).exists());
        assert_eq!(
            fs::read(original_path.join("package-index")).unwrap(),
            original_payload
        );
        let intent = super::read_fresh_stage_intent(
            &super::open_cache_root(&cache_root).unwrap(),
            REPOSITORY,
        )
        .unwrap()
        .unwrap();
        assert_eq!(intent.debt, foreign_debt);
        assert_rotation_inventory_blocked(&cache_root);
    }

    #[test]
    fn foreign_transaction_proof_is_refused_without_touching_original_store() {
        let (_temp, cache_root, debt) = fixture();
        let original_path = cache_root.join(REPOSITORY);
        let original_payload = fs::read(original_path.join("package-index")).unwrap();
        let foreign_debt = RotationDebt::new(
            REPOSITORY,
            "2123456789abcdef0123456789abcdef",
            debt.cache_root.clone(),
            debt.original_store.clone(),
        )
        .unwrap();
        let proof = super::ensure_fresh_stage(&cache_root, &foreign_debt, false).unwrap();
        let stage_name = RotationDebt::fresh_stage_component(REPOSITORY).unwrap();
        let stage_path = cache_root.join(stage_name.to_str().unwrap());
        let stage_identity = DirectoryIdentity::capture(&stage_path).unwrap();

        assert!(super::ensure_fresh_stage(&cache_root, &debt, false).is_err());
        assert_eq!(
            DirectoryIdentity::capture(&stage_path).unwrap(),
            stage_identity
        );
        assert_eq!(
            fs::read(original_path.join("package-index")).unwrap(),
            original_payload
        );
        let retained_proof = super::read_fresh_stage_proof(
            &super::open_cache_root(&cache_root).unwrap(),
            REPOSITORY,
        )
        .unwrap()
        .unwrap();
        assert_eq!(retained_proof, proof);
        assert_rotation_inventory_blocked(&cache_root);
    }

    #[test]
    fn intent_only_crash_is_inventory_visible_and_recoverable() {
        let (_temp, cache_root, debt) = fixture();
        let original_path = cache_root.join(REPOSITORY);
        let original_payload = fs::read(original_path.join("package-index")).unwrap();
        publish_stage_intent(&cache_root, &debt);

        let stage_name = RotationDebt::fresh_stage_component(REPOSITORY).unwrap();
        let stage_path = cache_root.join(stage_name.to_str().unwrap());
        assert!(!stage_path.exists());
        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );
        assert_rotation_inventory_blocked(&cache_root);

        let progress = recover(
            &cache_root,
            REPOSITORY,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();

        assert!(progress.complete);
        assert!(progress.found_debt);
        assert_eq!(fs::read_dir(&original_path).unwrap().count(), 0);
        assert_ne!(
            DirectoryIdentity::capture(&original_path).unwrap(),
            debt.original_store
        );
        assert_ne!(
            fs::read(original_path.join("package-index")).ok(),
            Some(original_payload)
        );
        assert!(!stage_path.exists());
        assert!(super::read(&cache_root, REPOSITORY).unwrap().is_none());
        assert!(
            super::read_fresh_stage_intent(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );
        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn staged_directory_before_proof_is_inventory_visible_and_recoverable() {
        let (_temp, cache_root, debt) = fixture();
        let original_path = cache_root.join(REPOSITORY);
        let original_payload = fs::read(original_path.join("package-index")).unwrap();
        publish_stage_intent(&cache_root, &debt);
        let stage_name = RotationDebt::fresh_stage_component(REPOSITORY).unwrap();
        let stage_path = cache_root.join(stage_name.to_str().unwrap());
        fs::create_dir(&stage_path).unwrap();
        let stage_identity = DirectoryIdentity::capture(&stage_path).unwrap();

        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );
        assert_rotation_inventory_blocked(&cache_root);

        let progress = recover(
            &cache_root,
            REPOSITORY,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();

        assert!(progress.complete);
        assert!(progress.found_debt);
        assert_eq!(fs::read_dir(&original_path).unwrap().count(), 0);
        assert_eq!(
            DirectoryIdentity::capture(&original_path).unwrap(),
            stage_identity
        );
        assert_ne!(
            fs::read(original_path.join("package-index")).ok(),
            Some(original_payload)
        );
        assert!(!stage_path.exists());
        assert!(super::read(&cache_root, REPOSITORY).unwrap().is_none());
    }

    #[test]
    fn legacy_same_identity_empty_cache_without_tree_witness_is_refused() {
        let (_temp, cache_root, debt) = fixture();
        let original_path = cache_root.join(REPOSITORY);
        fs::remove_file(original_path.join("package-index")).unwrap();
        assert_eq!(
            DirectoryIdentity::capture(&original_path).unwrap(),
            debt.original_store
        );

        assert!(super::ensure_fresh_stage(&cache_root, &debt, true).is_err());
        assert!(
            super::read_fresh_stage_intent(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );
        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );
        assert!(fs::read_dir(original_path).unwrap().next().is_none());
    }

    #[test]
    fn inventory_keeps_fixed_retired_store_without_a_witness_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let cache_root = temp.path().join("preparation-cache");
        fs::create_dir(&cache_root).unwrap();
        let retired = cache_root.join(format!("{REPOSITORY}.retired"));
        fs::create_dir(&retired).unwrap();
        fs::write(retired.join("payload"), b"preserve").unwrap();

        let inventory = inventory(
            &cache_root,
            100,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(inventory.complete);
        assert_eq!(inventory.repositories, vec![REPOSITORY.to_owned()]);
        assert!(inventory.blocked_repositories.contains(REPOSITORY));
        let error = recover(
            &cache_root,
            REPOSITORY,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .expect_err("unwitnessed retired bytes are not deletion authority");
        assert!(!error.progress.complete);
        assert_eq!(fs::read(retired.join("payload")).unwrap(), b"preserve");
    }

    #[test]
    fn inventory_requires_a_real_observed_parent_for_absent_cache_root() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let absent = inventory(
            &parent.join("missing-cache"),
            100,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(absent.complete);
        assert!(absent.repositories.is_empty());

        let missing_parent = inventory(
            &parent.join("missing-parent/cache"),
            100,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .expect_err("a missing ancestor cannot prove the cache root is clear");
        assert_eq!(missing_parent.kind(), std::io::ErrorKind::NotFound);

        let alias = parent.join("parent-alias");
        symlink(&parent, &alias).unwrap();
        assert!(
            inventory(
                &alias.join("ambiguous-cache"),
                100,
                Instant::now() + std::time::Duration::from_secs(5),
            )
            .is_err()
        );
    }

    #[test]
    fn bounded_probe_distinguishes_clear_pending_and_unknown_roots() {
        let temp = tempfile::tempdir().unwrap();
        let cache_root = temp.path().join("preparation-cache");
        fs::create_dir(&cache_root).unwrap();
        assert_eq!(
            probe(
                &cache_root,
                100,
                Instant::now() + std::time::Duration::from_secs(5),
            ),
            DebtProbe::NoDebt
        );

        let (_fixture, debt_root, debt) = fixture();
        publish(&debt_root, debt).unwrap();
        assert_eq!(
            probe(
                &debt_root,
                100,
                Instant::now() + std::time::Duration::from_secs(5),
            ),
            DebtProbe::Pending
        );
        assert_eq!(
            probe(
                &debt_root,
                0,
                Instant::now() + std::time::Duration::from_secs(5),
            ),
            DebtProbe::Unknown,
            "an exhausted listing budget must not report a clear cache root"
        );
        assert_eq!(
            probe(&debt_root, 100, Instant::now()),
            DebtProbe::Unknown,
            "an expired probe must not report a clear cache root"
        );
    }

    #[test]
    fn missing_cache_root_is_clear_only_under_an_observed_real_parent() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        assert_eq!(
            probe(
                &parent.join("missing-cache"),
                100,
                Instant::now() + std::time::Duration::from_secs(5),
            ),
            DebtProbe::NoDebt
        );
        assert_eq!(
            probe(
                &parent.join("missing-parent/cache"),
                100,
                Instant::now() + std::time::Duration::from_secs(5),
            ),
            DebtProbe::Unknown
        );

        let alias = parent.join("parent-alias");
        symlink(&parent, &alias).unwrap();
        assert_eq!(
            probe(
                &alias.join("ambiguous-cache"),
                100,
                Instant::now() + std::time::Duration::from_secs(5),
            ),
            DebtProbe::Unknown,
            "a symlinked ancestor cannot establish that the cache root is absent"
        );
    }

    #[test]
    fn debt_witness_is_bounded_canonical_and_never_overwritten() {
        let (_temp, cache_root, debt) = fixture();
        publish(&cache_root, debt.clone()).unwrap();
        assert_eq!(read(&cache_root, REPOSITORY).unwrap().unwrap().debt, debt);

        let different = RotationDebt::new(
            REPOSITORY,
            "fedcba9876543210fedcba9876543210",
            debt.cache_root.clone(),
            debt.original_store.clone(),
        )
        .unwrap();
        let repeated = publish(&cache_root, different).unwrap();
        assert_eq!(repeated.debt, debt);
        assert_eq!(read(&cache_root, REPOSITORY).unwrap().unwrap().debt, debt);
    }

    #[test]
    fn no_replace_rename_preserves_source_and_existing_destination() {
        use std::os::fd::AsRawFd;

        let (_temp, cache_root, _) = fixture();
        let source_path = cache_root.join("rename-source");
        let destination_path = cache_root.join("rename-destination");
        fs::write(&source_path, b"source must remain").unwrap();
        fs::write(&destination_path, b"destination must remain").unwrap();

        let root = super::open_cache_root(&cache_root).unwrap();
        let source = super::component("rename-source").unwrap();
        let destination = super::component("rename-destination").unwrap();
        assert!(
            super::rename_noreplace(root.as_raw_fd(), &source, root.as_raw_fd(), &destination,)
                .is_err()
        );

        assert_eq!(fs::read(source_path).unwrap(), b"source must remain");
        assert_eq!(
            fs::read(destination_path).unwrap(),
            b"destination must remain"
        );
    }

    fn write_private(path: &std::path::Path, bytes: &[u8]) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn group_or_world_writable_cache_root_is_refused_without_publication() {
        use std::os::unix::fs::PermissionsExt;

        let (_temp, cache_root, debt) = fixture();
        let previous = fs::metadata(&cache_root).unwrap().permissions();
        fs::set_permissions(&cache_root, fs::Permissions::from_mode(0o777)).unwrap();

        assert!(publish(&cache_root, debt).is_err());
        assert!(fs::read_dir(&cache_root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(RECORD_PREFIX)
        }));

        fs::set_permissions(&cache_root, previous).unwrap();
    }

    #[test]
    fn complete_scratch_is_atomically_promoted_on_retry() {
        let (_temp, cache_root, debt) = fixture();
        let scratch_path = cache_root.join(format!("{RECORD_PREFIX}{REPOSITORY}{SCRATCH_SUFFIX}"));
        write_private(&scratch_path, &super::encode(&debt).unwrap());

        let retained = publish(&cache_root, debt.clone()).unwrap();

        assert_eq!(retained.debt, debt);
        assert!(!scratch_path.exists());
        assert_eq!(
            read(&cache_root, REPOSITORY).unwrap().unwrap().debt,
            retained.debt
        );
    }

    #[test]
    fn committed_witness_and_scratch_are_an_ambiguous_state() {
        let (_temp, cache_root, debt) = fixture();
        publish(&cache_root, debt.clone()).unwrap();
        let scratch_path = cache_root.join(format!("{RECORD_PREFIX}{REPOSITORY}{SCRATCH_SUFFIX}"));
        write_private(&scratch_path, b"{");

        assert!(read(&cache_root, REPOSITORY).is_err());
        assert!(publish(&cache_root, debt).is_err());
        assert!(scratch_path.exists());
    }

    #[test]
    fn scratch_hard_link_is_preserved_without_touching_its_target() {
        let (_temp, cache_root, debt) = fixture();
        let scratch_path = cache_root.join(format!("{RECORD_PREFIX}{REPOSITORY}{SCRATCH_SUFFIX}"));
        let target = cache_root.join("scratch-target");
        write_private(&target, b"incomplete");
        fs::hard_link(&target, &scratch_path).unwrap();

        assert!(publish(&cache_root, debt).is_err());
        assert!(scratch_path.exists());
        assert_eq!(fs::read(&target).unwrap(), b"incomplete");
    }

    #[test]
    fn incomplete_scratch_is_reclaimed_only_before_rotation_mutates_the_store() {
        let (_temp, cache_root, debt) = fixture();
        let scratch_path = cache_root.join(format!("{RECORD_PREFIX}{REPOSITORY}{SCRATCH_SUFFIX}"));
        write_private(&scratch_path, b"{");
        assert!(read(&cache_root, REPOSITORY).unwrap().is_none());

        let retained = publish(&cache_root, debt.clone()).unwrap();
        assert_eq!(retained.debt, debt);
        assert!(!scratch_path.exists());
        assert_eq!(read(&cache_root, REPOSITORY).unwrap().unwrap().debt, debt);

        let (_temp, cache_root, debt) = fixture();
        let scratch_path = cache_root.join(format!("{RECORD_PREFIX}{REPOSITORY}{SCRATCH_SUFFIX}"));
        write_private(&scratch_path, b"{");
        let store = cache_root.join(REPOSITORY);
        let retired = cache_root.join(debt.retired_component());
        fs::rename(&store, &retired).unwrap();

        assert!(publish(&cache_root, debt).is_err());
        assert!(scratch_path.exists(), "ambiguous scratch remains untouched");
    }

    #[test]
    fn clear_refuses_to_forget_retired_payload_or_nonempty_fresh_store() {
        let (_temp, cache_root, debt) = fixture();
        let retained = publish(&cache_root, debt.clone()).unwrap();
        let store = cache_root.join(REPOSITORY);
        let retired = cache_root.join(debt.retired_component());
        fs::rename(&store, &retired).unwrap();

        assert!(super::clear_completed(&cache_root, &retained).is_err());
        assert_eq!(read(&cache_root, REPOSITORY).unwrap().unwrap().debt, debt);
        assert_eq!(fs::read(retired.join("package-index")).unwrap(), b"cached");

        fs::remove_dir_all(&retired).unwrap();
        super::create_fresh_store(&cache_root, &retained).unwrap();
        fs::write(store.join("unexpected"), b"preserve me").unwrap();
        assert!(super::clear_completed(&cache_root, &retained).is_err());
        assert_eq!(read(&cache_root, REPOSITORY).unwrap().unwrap().debt, debt);
        assert_eq!(fs::read(store.join("unexpected")).unwrap(), b"preserve me");

        fs::remove_file(store.join("unexpected")).unwrap();
        super::clear_completed(&cache_root, &retained).unwrap();
        assert!(read(&cache_root, REPOSITORY).unwrap().is_none());
        let progress = recover(
            &cache_root,
            REPOSITORY,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(progress.complete);
        assert!(progress.found_debt);
        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn retired_store_witness_is_verified_by_the_common_pinned_deleter() {
        use crate::gc::{PinnedTreeRemoval, TreeRemoval};

        let (_temp, cache_root, debt) = fixture();
        let retained = publish(&cache_root, debt.clone()).unwrap();
        super::retire_original_store(&cache_root, &retained).unwrap();
        let witness = super::prepare_retired_store_removal(&cache_root, &retained).unwrap();
        let retired = cache_root.join(debt.retired_component());
        let pinned = PinnedTreeRemoval::open(&retired).unwrap();
        let object = pinned.object_identity().unwrap();
        let filesystem = pinned.filesystem_identity().unwrap();
        assert!(
            witness
                .matches_pinned_identity(object.device, object.inode, &filesystem)
                .unwrap()
        );

        assert_eq!(
            pinned
                .remove_verified_witness(witness.component().unwrap(), None, |bytes| {
                    witness.validate_bytes(bytes)
                })
                .unwrap(),
            TreeRemoval::Complete
        );
        assert!(!retired.exists());
        assert_eq!(
            super::recovery_step(&cache_root, &debt).unwrap(),
            RecoveryStep::CreateFreshStore
        );
        super::create_fresh_store(&cache_root, &retained).unwrap();
        super::clear_completed(&cache_root, &retained).unwrap();
        assert!(read(&cache_root, REPOSITORY).unwrap().is_none());
    }

    #[test]
    fn missing_retired_witness_is_restored_only_after_descriptor_relative_empty_check() {
        use crate::gc::{PinnedTreeRemoval, TreeRemoval};

        let (_temp, cache_root, debt) = fixture();
        let retained = publish(&cache_root, debt.clone()).unwrap();
        super::retire_original_store(&cache_root, &retained).unwrap();
        let retired = cache_root.join(debt.retired_component());
        let first_witness = super::prepare_retired_store_removal(&cache_root, &retained).unwrap();

        // Model the common deleter's crash after its final witness unlink but
        // before rmdir: all payload and the marker are gone, while the exact
        // retired directory inode remains.
        fs::remove_file(retired.join("package-index")).unwrap();
        fs::remove_file(retired.join(first_witness.component().unwrap())).unwrap();
        assert!(fs::read_dir(&retired).unwrap().next().is_none());

        let restored = super::prepare_retired_store_removal(&cache_root, &retained).unwrap();
        assert_eq!(restored.bytes, super::encode(&debt).unwrap());
        assert!(fs::read_dir(&retired).unwrap().all(|entry| {
            entry.unwrap().file_name().to_string_lossy() == restored.component().unwrap()
        }));
        let pinned = PinnedTreeRemoval::open(&retired).unwrap();
        let object = pinned.object_identity().unwrap();
        let filesystem = pinned.filesystem_identity().unwrap();
        assert!(
            restored
                .matches_pinned_identity(object.device, object.inode, &filesystem)
                .unwrap()
        );
        assert_eq!(
            pinned
                .remove_verified_witness(restored.component().unwrap(), None, |bytes| {
                    restored.validate_bytes(bytes)
                })
                .unwrap(),
            TreeRemoval::Complete
        );
        assert!(!retired.exists());
    }

    #[test]
    fn missing_witness_on_nonempty_retired_store_is_preserved_and_refused() {
        let (_temp, cache_root, debt) = fixture();
        let retained = publish(&cache_root, debt.clone()).unwrap();
        super::retire_original_store(&cache_root, &retained).unwrap();
        let retired = cache_root.join(debt.retired_component());
        let witness = super::prepare_retired_store_removal(&cache_root, &retained).unwrap();
        fs::remove_file(retired.join(witness.component().unwrap())).unwrap();

        assert!(super::prepare_retired_store_removal(&cache_root, &retained).is_err());
        assert!(!retired.join(witness.component().unwrap()).exists());
        assert_eq!(fs::read(retired.join("package-index")).unwrap(), b"cached");
        assert_eq!(read(&cache_root, REPOSITORY).unwrap().unwrap().debt, debt);
    }

    #[test]
    fn partial_original_tree_witness_scratch_is_rebuilt_on_restart() {
        let (_temp, cache_root, debt) = fixture();
        let retained = publish(&cache_root, debt.clone()).unwrap();
        let store = cache_root.join(REPOSITORY);
        let scratch = store.join(
            debt.tree_witness_scratch_component()
                .unwrap()
                .to_string_lossy()
                .as_ref(),
        );
        write_private(&scratch, b"{");

        super::retire_original_store(&cache_root, &retained).unwrap();

        let retired = cache_root.join(debt.retired_component());
        let witness = super::prepare_retired_store_removal(&cache_root, &retained).unwrap();
        assert_eq!(witness.bytes, super::encode(&debt).unwrap());
        assert_eq!(fs::read(retired.join("package-index")).unwrap(), b"cached");
        assert!(!retired.join(scratch.file_name().unwrap()).exists());
    }

    #[test]
    fn partial_empty_retired_tree_witness_scratch_is_rebuilt_on_restart() {
        let (_temp, cache_root, debt) = fixture();
        let retained = publish(&cache_root, debt.clone()).unwrap();
        super::retire_original_store(&cache_root, &retained).unwrap();
        let retired = cache_root.join(debt.retired_component());
        let first_witness = super::prepare_retired_store_removal(&cache_root, &retained).unwrap();
        fs::remove_file(retired.join("package-index")).unwrap();
        fs::remove_file(retired.join(first_witness.component().unwrap())).unwrap();

        let scratch_name = debt.tree_witness_scratch_component().unwrap();
        let scratch_path = retired.join(scratch_name.to_string_lossy().as_ref());
        write_private(&scratch_path, b"{");

        let rebuilt = super::prepare_retired_store_removal(&cache_root, &retained).unwrap();
        assert_eq!(rebuilt.bytes, super::encode(&debt).unwrap());
        assert!(fs::read_dir(&retired).unwrap().all(|entry| {
            entry.unwrap().file_name().to_string_lossy() == rebuilt.component().unwrap()
        }));
    }

    #[test]
    fn retired_root_replacement_after_witness_preparation_fails_identity_check() {
        use crate::gc::PinnedTreeRemoval;

        let (_temp, cache_root, debt) = fixture();
        let retained = publish(&cache_root, debt.clone()).unwrap();
        super::retire_original_store(&cache_root, &retained).unwrap();
        let retired = cache_root.join(debt.retired_component());
        let witness = super::prepare_retired_store_removal(&cache_root, &retained).unwrap();

        let displaced = cache_root.join("displaced-retired-cache");
        fs::rename(&retired, &displaced).unwrap();
        fs::create_dir(&retired).unwrap();
        fs::write(retired.join("replacement-content"), b"must remain").unwrap();

        let pinned = PinnedTreeRemoval::open(&retired).unwrap();
        let object = pinned.object_identity().unwrap();
        let filesystem = pinned.filesystem_identity().unwrap();
        assert!(
            !witness
                .matches_pinned_identity(object.device, object.inode, &filesystem)
                .unwrap()
        );
        assert_eq!(
            fs::read(retired.join("replacement-content")).unwrap(),
            b"must remain"
        );
        assert_eq!(
            fs::read(displaced.join("package-index")).unwrap(),
            b"cached"
        );
    }

    #[test]
    fn restart_after_unlink_before_directory_sync_cannot_hide_old_payload() {
        let (_temp, cache_root, debt) = fixture();
        let retained = publish(&cache_root, debt.clone()).unwrap();
        let store = cache_root.join(REPOSITORY);
        let retired = cache_root.join(debt.retired_component());
        fs::rename(&store, &retired).unwrap();
        fs::remove_dir_all(&retired).unwrap();
        super::create_fresh_store(&cache_root, &retained).unwrap();

        let injected = super::clear_completed_with_sync(&cache_root, &retained, |_| {
            Err(std::io::Error::other(
                "simulated interruption before directory sync",
            ))
        });
        assert!(injected.is_err());

        // Reopen the cache root as a restarted caller would. No old tree is
        // recoverable or hidden: it was removed before the witness was cleared.
        assert!(read(&cache_root, REPOSITORY).unwrap().is_none());
        assert!(!retired.exists());
        assert!(!store.join("package-index").exists());
        assert!(fs::read_dir(&store).unwrap().next().is_none());

        let progress = recover(
            &cache_root,
            REPOSITORY,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(progress.complete);
        assert!(progress.found_debt);
        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );

        let next_debt = RotationDebt::new(
            REPOSITORY,
            new_transaction_id().as_str(),
            DirectoryIdentity::capture(&cache_root).unwrap(),
            DirectoryIdentity::capture(&store).unwrap(),
        )
        .unwrap();
        assert_eq!(
            publish(&cache_root, next_debt.clone()).unwrap().debt,
            next_debt
        );
    }

    #[test]
    fn fresh_store_and_retired_deletion_transitions_are_recoverable() {
        let (_temp, cache_root, debt) = fixture();
        let retained = publish(&cache_root, debt.clone()).unwrap();
        let store = cache_root.join(REPOSITORY);
        let retired = cache_root.join(debt.retired_component());
        fs::rename(&store, &retired).unwrap();
        assert_eq!(
            recovery_step(&cache_root, &debt).unwrap(),
            RecoveryStep::RemoveRetiredStore
        );
        fs::remove_dir_all(&retired).unwrap();
        assert_eq!(
            recovery_step(&cache_root, &debt).unwrap(),
            RecoveryStep::CreateFreshStore
        );
        super::create_fresh_store(&cache_root, &retained).unwrap();
        assert_eq!(
            recovery_step(&cache_root, &debt).unwrap(),
            RecoveryStep::ClearCompletedDebt
        );
        super::clear_completed(&cache_root, &retained).unwrap();
        assert!(read(&cache_root, REPOSITORY).unwrap().is_none());
        let progress = recover(
            &cache_root,
            REPOSITORY,
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(progress.complete);
        assert!(progress.found_debt);
        assert!(
            super::read_fresh_stage_proof(
                &super::open_cache_root(&cache_root).unwrap(),
                REPOSITORY,
            )
            .unwrap()
            .is_none()
        );
        assert!(fs::read_dir(&cache_root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".remove-")
        }));
    }

    #[test]
    fn transaction_ids_are_fixed_length_lowercase_hex() {
        let first = new_transaction_id();
        let second = new_transaction_id();
        assert_eq!(first.len(), 32);
        assert!(
            first
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        assert_ne!(first, second);
    }

    #[test]
    fn recovery_classifier_fails_closed_for_ambiguous_or_recreated_data() {
        let (_temp, cache_root, debt) = fixture();
        let original = debt.original_store.clone();
        assert_eq!(
            classify_recovery(&debt, Some(&original), None, false),
            RecoveryStep::RenameOriginalStore
        );
        assert_eq!(
            classify_recovery(&debt, None, Some(&original), false),
            RecoveryStep::RemoveRetiredStore
        );
        assert_eq!(
            classify_recovery(&debt, None, None, false),
            RecoveryStep::CreateFreshStore
        );

        let fresh_path = cache_root.join(REPOSITORY);
        fs::rename(&fresh_path, cache_root.join("original")).unwrap();
        fs::create_dir(&fresh_path).unwrap();
        let fresh = DirectoryIdentity::capture(&fresh_path).unwrap();
        assert_eq!(
            classify_recovery(&debt, Some(&fresh), None, true),
            RecoveryStep::ClearCompletedDebt
        );
        fs::write(fresh_path.join("unexpected"), b"preserve me").unwrap();
        assert_eq!(
            classify_recovery(&debt, Some(&fresh), None, false),
            RecoveryStep::Conflict
        );

        let mut foreign_fsid = fresh.filesystem_id.clone();
        let replacement = if &foreign_fsid[..2] == "00" {
            "01"
        } else {
            "00"
        };
        foreign_fsid.replace_range(0..2, replacement);
        let foreign = DirectoryIdentity {
            device: fresh.device,
            inode: fresh.inode,
            filesystem_id: foreign_fsid,
        };
        assert_eq!(
            classify_recovery(&debt, Some(&foreign), None, true),
            RecoveryStep::Conflict
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_rejects_hard_links_and_group_or_world_accessible_witnesses() {
        use std::os::unix::fs::PermissionsExt;

        let (_temp, cache_root, debt) = fixture();
        let record = cache_root.join(format!("{RECORD_PREFIX}{REPOSITORY}.json"));
        let target = cache_root.join("outside");
        write_private(&target, b"keep");
        fs::hard_link(&target, &record).unwrap();
        assert!(read(&cache_root, REPOSITORY).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        fs::remove_file(&record).unwrap();

        write_private(&record, &super::encode(&debt).unwrap());
        fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(&cache_root, REPOSITORY).is_err());
        assert_eq!(fs::read(&record).unwrap(), super::encode(&debt).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn read_rejects_symlink_and_oversized_debt_without_following_it() {
        use std::os::unix::fs::symlink;

        let (_temp, cache_root, debt) = fixture();
        let record_name = format!("{RECORD_PREFIX}{REPOSITORY}.json");
        let target = cache_root.join("outside");
        fs::write(&target, b"keep").unwrap();
        symlink(&target, cache_root.join(&record_name)).unwrap();
        assert!(read(&cache_root, REPOSITORY).is_err());
        assert_eq!(fs::read(target).unwrap(), b"keep");

        fs::remove_file(cache_root.join(record_name)).unwrap();
        write_private(
            &cache_root.join(format!("{RECORD_PREFIX}{REPOSITORY}.json")),
            &vec![b'x'; 8193],
        );
        assert!(read(&cache_root, REPOSITORY).is_err());
        assert_eq!(debt.repository, REPOSITORY);
    }

    #[test]
    fn cache_root_capture_is_read_only_and_writable_init_binds_a_nonce_child() {
        let temporary = tempfile::tempdir().unwrap();
        let temporary_root = temporary.path().canonicalize().unwrap();
        let root = temporary_root.as_path().join("separate-cache");
        fs::create_dir(&root).unwrap();
        let captured = super::capture_preparation_cache_root(&root).unwrap();
        assert!(matches!(
            captured.state,
            super::PreparationCacheRootCaptureState::Uninitialized
        ));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let root_identity = captured.identity.clone();

        let initialized =
            initialize_test_cache_root(&root, &root_identity, &captured.directory, || Ok(false))
                .unwrap();
        assert_eq!(initialized.root_identity, root_identity);
        assert_eq!(initialized.nonce.len(), 64);
        let child = root.join(format!(".aethyme-preparation-cache-{}", initialized.nonce));
        assert!(child.is_dir());
        assert!(root.join(".aethyme-preparation-cache-root-v2").is_file());
        assert!(root.join(".aethyme-preparation-cache-root.lock").is_file());

        let reopened = super::capture_preparation_cache_root(&root).unwrap();
        match reopened.state {
            super::PreparationCacheRootCaptureState::Marked {
                coordinator_db_path: _,
                nonce,
                cache_root_identity,
            } => {
                assert_eq!(nonce, initialized.nonce);
                assert_eq!(
                    cache_root_identity,
                    Some(initialized.cache_root_identity.clone())
                );
            }
            super::PreparationCacheRootCaptureState::Uninitialized => {
                panic!("writable initialization must publish a marker")
            }
        }
        let retried =
            initialize_test_cache_root(&root, &root_identity, &captured.directory, || {
                panic!("fully initialized root must not probe debt")
            })
            .unwrap();
        assert_eq!(retried, initialized);
    }

    #[test]
    fn cache_root_initialization_refuses_unowned_entries_and_prior_journal_rows() {
        let temporary = tempfile::tempdir().unwrap();
        let temporary_root = temporary.path().canonicalize().unwrap();
        let unowned_root = temporary_root.as_path().join("unowned-cache");
        fs::create_dir(&unowned_root).unwrap();
        let capture = super::capture_preparation_cache_root(&unowned_root).unwrap();
        let identity = capture.identity.clone();
        let payload = unowned_root.join("keep-me");
        fs::write(&payload, b"unowned bytes").unwrap();
        assert!(
            initialize_test_cache_root(&unowned_root, &identity, &capture.directory, || Ok(false),)
                .is_err()
        );
        assert_eq!(fs::read(&payload).unwrap(), b"unowned bytes");
        assert!(
            !unowned_root
                .join(".aethyme-preparation-cache-root-v2")
                .exists()
        );
        assert_eq!(fs::read_dir(&unowned_root).unwrap().count(), 2);

        let journal_root = temporary_root.as_path().join("journal-cache");
        fs::create_dir(&journal_root).unwrap();
        let journal_capture = super::capture_preparation_cache_root(&journal_root).unwrap();
        let journal_identity = journal_capture.identity.clone();
        assert!(
            initialize_test_cache_root(
                &journal_root,
                &journal_identity,
                &journal_capture.directory,
                || Ok(true),
            )
            .is_err()
        );
        assert!(
            !journal_root
                .join(".aethyme-preparation-cache-root-v2")
                .exists()
        );
        assert_eq!(fs::read_dir(&journal_root).unwrap().count(), 1);
        assert!(matches!(
            super::capture_preparation_cache_root(&journal_root)
                .unwrap()
                .state,
            super::PreparationCacheRootCaptureState::Uninitialized
        ));
    }

    #[test]
    fn missing_nonce_child_is_completed_only_after_journal_preflight() {
        let temporary = tempfile::tempdir().unwrap();
        let temporary_root = temporary.path().canonicalize().unwrap();
        let root = temporary_root.as_path().join("cache-root");
        fs::create_dir(&root).unwrap();
        let captured = super::capture_preparation_cache_root(&root).unwrap();
        let identity = captured.identity.clone();
        let initialized =
            initialize_test_cache_root(&root, &identity, &captured.directory, || Ok(false))
                .unwrap();
        let child = root.join(format!(".aethyme-preparation-cache-{}", initialized.nonce));
        fs::remove_dir(&child).unwrap();
        let marker_before = fs::read(root.join(".aethyme-preparation-cache-root-v2")).unwrap();
        match super::capture_preparation_cache_root(&root).unwrap().state {
            super::PreparationCacheRootCaptureState::Marked {
                coordinator_db_path: _,
                nonce,
                cache_root_identity,
            } => {
                assert_eq!(nonce, initialized.nonce);
                assert_eq!(cache_root_identity, None);
            }
            super::PreparationCacheRootCaptureState::Uninitialized => {
                panic!("the committed marker remains authoritative")
            }
        }
        assert!(
            initialize_test_cache_root(&root, &identity, &captured.directory, || Ok(true),)
                .is_err()
        );
        assert!(!child.exists());
        assert_eq!(
            fs::read(root.join(".aethyme-preparation-cache-root-v2")).unwrap(),
            marker_before
        );
        let completed =
            initialize_test_cache_root(&root, &identity, &captured.directory, || Ok(false))
                .unwrap();
        assert_eq!(completed.nonce, initialized.nonce);
        assert!(child.is_dir());
    }

    #[test]
    fn cache_root_capture_rejects_partial_marker_and_root_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let temporary_root = temporary.path().canonicalize().unwrap();
        let malformed_root = temporary_root.as_path().join("malformed-cache");
        fs::create_dir(&malformed_root).unwrap();
        fs::write(
            malformed_root.join(".aethyme-preparation-cache-root-v2"),
            b"{partial",
        )
        .unwrap();
        assert!(super::capture_preparation_cache_root(&malformed_root).is_err());
        assert_eq!(
            fs::read(malformed_root.join(".aethyme-preparation-cache-root-v2")).unwrap(),
            b"{partial"
        );

        let root = temporary_root.as_path().join("replaceable-cache");
        fs::create_dir(&root).unwrap();
        let captured = super::capture_preparation_cache_root(&root).unwrap();
        let identity = captured.identity.clone();
        let old_root = temporary_root.as_path().join("old-cache");
        fs::rename(&root, &old_root).unwrap();
        fs::create_dir(&root).unwrap();
        assert!(
            initialize_test_cache_root(&root, &identity, &captured.directory, || Ok(false),)
                .is_err()
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&old_root).unwrap().count(), 0);
    }
}
