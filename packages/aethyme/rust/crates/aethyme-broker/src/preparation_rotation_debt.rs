//! Durable, repository-scoped intent for shared preparation-cache rotation.
//!
//! The record is an immutable witness in the cache root. It is published with
//! a no-replace rename and is removed only after the old store is gone and the
//! replacement store is a new, empty directory on the same filesystem. Every
//! mutation requires the caller to hold the repository's preparation-cache
//! lease for its full duration.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::{self, MaybeUninit};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const SCHEMA: &str = "aethyme.preparation-cache-rotation-debt";
const VERSION: u8 = 1;
const MAX_RECORD_BYTES: u64 = 8 * 1024;
const RECORD_PREFIX: &str = ".aethyme-preparation-rotation-";
const SCRATCH_SUFFIX: &str = ".scratch";
const DIRECTORY_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;

/// Confirm that the descriptor-relative, no-follow recovery implementation is
/// available on this platform. The sibling unsupported-platform module returns
/// an explicit `Unsupported` error instead of compiling a partial fallback.
pub(crate) fn ensure_supported() -> io::Result<()> {
    Ok(())
}

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

    fn validate(&self) -> io::Result<()> {
        if !is_hex(&self.filesystem_id, 2, 128) || self.filesystem_id.len() % 2 != 0 {
            return Err(invalid_record("filesystem identity is malformed"));
        }
        Ok(())
    }

    fn same_directory(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.filesystem_id == other.filesystem_id
    }

    fn same_filesystem(&self, other: &Self) -> bool {
        self.device == other.device && self.filesystem_id == other.filesystem_id
    }
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

    pub(crate) fn repository(&self) -> &str {
        &self.repository
    }

    pub(crate) fn retired_component(&self) -> &str {
        &self.retired_component
    }

    pub(crate) fn cache_root(&self) -> &DirectoryIdentity {
        &self.cache_root
    }

    pub(crate) fn original_store(&self) -> &DirectoryIdentity {
        &self.original_store
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

/// A durable debt record together with the inode of the exact sidecar file
/// that was opened without following links.
#[derive(Debug, Clone)]
pub(crate) struct RetainedRotationDebt {
    pub(crate) debt: RotationDebt,
    marker: FileIdentity,
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

/// Publish a debt record by writing an exclusive scratch file, syncing it,
/// and atomically publishing it with a no-replace rename. A crash before that
/// rename leaves only non-authoritative scratch. A valid scratch is promoted
/// on retry; an incomplete one may be discarded only while the original store
/// remains at its source name and no retired or committed witness exists.
///
/// The caller must hold the same repository-scoped preparation-cache lease
/// across source validation, publication, and rotation.
pub(crate) fn publish(
    cache_root: &Path,
    proposed: RotationDebt,
) -> io::Result<RetainedRotationDebt> {
    proposed.validate()?;
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
                ensure_component_identity(root.as_raw_fd(), &scratch_name, scratch.identity)?;
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
    let bytes = encode(&proposed)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid_record("rotation debt exceeds its size limit"));
    }
    let mut scratch = create_regular_exclusive(root.as_raw_fd(), &scratch_name)?;
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
pub(crate) fn clear_completed(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
) -> io::Result<()> {
    clear_completed_with_sync(cache_root, retained, File::sync_all)
}

fn clear_completed_with_sync(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
    sync_root: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
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
    unlink_component(root.as_raw_fd(), &name)?;
    sync_root(&root)
}

fn encode(debt: &RotationDebt) -> io::Result<Vec<u8>> {
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
            0o600 as libc::mode_t,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is the fresh descriptor returned by the successful openat
    // above; ownership is transferred to File exactly once.
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !is_private_owned_file(&metadata) {
        return Err(invalid_record(
            "new rotation witness is not a private regular file",
        ));
    }
    Ok(file)
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
    let fd = directory.into_raw_fd();
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
        if entry_name != b"." && entry_name != b".." {
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
    // SAFETY: both directory descriptors are live; from/to are NUL-terminated
    // single components and RENAME_NOREPLACE prevents replacement of target.
    let result = unsafe {
        libc::renameat2(
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

fn invalid_record(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        DirectoryIdentity, RECORD_PREFIX, RecoveryStep, RotationDebt, SCRATCH_SUFFIX,
        classify_recovery, new_transaction_id, publish, read, recovery_step,
    };
    use std::fs;

    const REPOSITORY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const TRANSACTION: &str = "0123456789abcdef0123456789abcdef";

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

    #[test]
    fn debt_witness_is_bounded_canonical_and_never_overwritten() {
        let (_temp, cache_root, debt) = fixture();
        let published = publish(&cache_root, debt.clone()).unwrap();
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
        fs::create_dir(&store).unwrap();
        fs::write(store.join("unexpected"), b"preserve me").unwrap();
        assert!(super::clear_completed(&cache_root, &retained).is_err());
        assert_eq!(read(&cache_root, REPOSITORY).unwrap().unwrap().debt, debt);
        assert_eq!(fs::read(store.join("unexpected")).unwrap(), b"preserve me");

        fs::remove_file(store.join("unexpected")).unwrap();
        super::clear_completed(&cache_root, &retained).unwrap();
        assert!(read(&cache_root, REPOSITORY).unwrap().is_none());
    }

    #[test]
    fn restart_after_unlink_before_directory_sync_cannot_hide_old_payload() {
        let (_temp, cache_root, debt) = fixture();
        let retained = publish(&cache_root, debt.clone()).unwrap();
        let store = cache_root.join(REPOSITORY);
        let retired = cache_root.join(debt.retired_component());
        fs::rename(&store, &retired).unwrap();
        fs::remove_dir_all(&retired).unwrap();
        fs::create_dir(&store).unwrap();

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
        fs::create_dir(&store).unwrap();
        assert_eq!(
            recovery_step(&cache_root, &debt).unwrap(),
            RecoveryStep::ClearCompletedDebt
        );
        super::clear_completed(&cache_root, &retained).unwrap();
        assert!(read(&cache_root, REPOSITORY).unwrap().is_none());
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
        assert_eq!(debt.repository(), REPOSITORY);
    }
}
