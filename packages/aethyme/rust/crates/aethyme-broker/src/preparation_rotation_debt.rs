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
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const SCHEMA: &str = "aethyme.preparation-cache-rotation-debt";
const VERSION: u8 = 1;
const MAX_RECORD_BYTES: u64 = 8 * 1024;
const RECORD_PREFIX: &str = ".aethyme-preparation-rotation-";
const SCRATCH_SUFFIX: &str = ".scratch";
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
pub(crate) fn recover_with_journal(
    cache_root: &Path,
    resource_db: &Path,
    repository: &str,
    deadline: Instant,
) -> Result<RecoveryProgress, RecoveryError> {
    let mut progress = RecoveryProgress::default();
    if Instant::now() >= deadline {
        return Err(recovery_error(progress, deadline_error()));
    }
    let root_identity = DirectoryIdentity::capture(cache_root)
        .map_err(|source| recovery_error(progress, source))?;
    let root_identity_bytes = root_identity
        .encode()
        .map_err(|source| recovery_error(progress, source))?;

    let journal_payload =
        super::rotation_journal::load(resource_db, cache_root, &root_identity_bytes, repository)
            .map_err(|source| recovery_error(progress, source))?;
    let payload = match journal_payload {
        Some(payload) => payload,
        None => {
            let retained =
                read(cache_root, repository).map_err(|source| recovery_error(progress, source))?;
            if let Some(retained) = retained {
                let payload =
                    encode(&retained.debt).map_err(|source| recovery_error(progress, source))?;
                super::rotation_journal::publish(
                    resource_db,
                    cache_root,
                    &root_identity_bytes,
                    repository,
                    &payload,
                )
                .map_err(|source| recovery_error(progress, source))?;
                payload
            } else {
                if has_uncommitted_scratch(cache_root, repository)
                    .map_err(|source| recovery_error(progress, source))?
                {
                    return Err(recovery_error(
                        progress,
                        invalid_record("unpublished rotation scratch has no durable journal row"),
                    ));
                }
                return recover(cache_root, repository, deadline);
            }
        }
    };

    let debt = RotationDebt::from_journal_payload(cache_root, repository, &payload)
        .map_err(|source| recovery_error(progress, source))?;
    match recovery_step(cache_root, &debt).map_err(|source| recovery_error(progress, source))? {
        RecoveryStep::Conflict => {
            return Err(recovery_error(
                progress,
                invalid_record("journal rotation debt conflicts with current cache state"),
            ));
        }
        RecoveryStep::ClearCompletedDebt => {}
        RecoveryStep::RenameOriginalStore
        | RecoveryStep::RemoveRetiredStore
        | RecoveryStep::CreateFreshStore => {
            restore_from_journal(cache_root, &debt)
                .map_err(|source| recovery_error(progress, source))?;
        }
    }

    progress = recover(cache_root, repository, deadline)?;
    if progress.complete {
        progress.found_debt = true;
        if let Err(source) = super::rotation_journal::complete(
            resource_db,
            cache_root,
            &root_identity_bytes,
            repository,
            &payload,
        ) {
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
    Ok(read_component(root.as_raw_fd(), &name)?.is_some())
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

/// Finish a committed rotation under the repository's existing exclusive
/// preparation-cache lease. This operation is deliberately cache-only and
/// does not consult current worktree liveness: the writer published the debt
/// before it could start any command that used the store.
pub(crate) fn recover(
    cache_root: &Path,
    repository: &str,
    deadline: Instant,
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

    let retained = match read_or_promote_scratch(cache_root, repository) {
        Ok(Some(retained)) => retained,
        Ok(None) => {
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
        Err(source) => return Err(recovery_error(progress, source)),
    };
    progress.found_debt = true;

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
            RecoveryStep::RenameOriginalStore => retire_original_store(cache_root, &retained)
                .map_err(|source| recovery_error(progress, source))?,
            RecoveryStep::RemoveRetiredStore => {
                let witness = prepare_retired_store_removal(cache_root, &retained)
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
                match pinned.remove_verified_witness_with_progress(
                    &witness_name,
                    Some(deadline),
                    |bytes| witness.validate_bytes(bytes),
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
                create_fresh_store(cache_root, &retained)
                    .map_err(|source| recovery_error(progress, source))?;
            }
            RecoveryStep::ClearCompletedDebt => {
                clear_completed(cache_root, &retained)
                    .map_err(|source| recovery_error(progress, source))?;
                progress.complete = true;
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
pub(crate) fn retire_original_store(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
) -> io::Result<()> {
    if recovery_step(cache_root, &retained.debt)? != RecoveryStep::RenameOriginalStore {
        return Err(invalid_record("original cache is not ready to be retired"));
    }
    let witness = prepare_tree_witness(cache_root, retained)?;
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
pub(crate) fn prepare_retired_store_removal(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
) -> io::Result<RetiredTreeWitness> {
    if recovery_step(cache_root, &retained.debt)? != RecoveryStep::RemoveRetiredStore {
        return Err(invalid_record(
            "no retired cache store is ready for removal",
        ));
    }
    prepare_tree_witness(cache_root, retained)
}

/// Create the replacement cache directory only after the original retired
/// store is absent. The caller must keep the repository-scoped lease through
/// the creation and subsequent debt clearing.
pub(crate) fn create_fresh_store(
    cache_root: &Path,
    retained: &RetainedRotationDebt,
) -> io::Result<DirectoryIdentity> {
    retained.debt.validate()?;
    let root = open_cache_root(cache_root)?;
    verify_retained_sidecar(&root, retained)?;
    if recovery_step_at(&root, &retained.debt)? != RecoveryStep::CreateFreshStore {
        return Err(invalid_record(
            "rotation is not ready for a fresh cache store",
        ));
    }

    let current_name = component(&retained.debt.repository)?;
    let mode = 0o700 as libc::mode_t;
    // SAFETY: root is a live cache-root directory descriptor and current_name
    // is one validated NUL-terminated component. mkdirat creates only this
    // missing child and cannot replace an existing entry.
    if unsafe { libc::mkdirat(root.as_raw_fd(), current_name.as_ptr(), mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    root.sync_all()?;

    let fresh = open_directory_at(root.as_raw_fd(), &current_name)?;
    let identity = identity_for_directory(&fresh)?;
    if identity.same_directory(&retained.debt.original_store)
        || !identity.same_filesystem(&retained.debt.cache_root)
        || !directory_is_empty(&fresh)?
    {
        return Err(invalid_record(
            "new cache store is not a fresh empty directory on the recorded filesystem",
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
) -> io::Result<RetiredTreeWitness> {
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
                &root,
                retained,
                step,
                &store_name,
                &directory,
                &directory_identity,
                &scratch_name,
                &witness_name,
                &bytes,
                &scratch,
            )?;
        } else {
            if !is_original_at_current && !directory_is_empty(&directory)? {
                return Err(invalid_record(
                    "missing witness in a nonempty retired cache tree",
                ));
            }
            publish_tree_witness(
                &root,
                retained,
                step,
                &store_name,
                &directory,
                &directory_identity,
                &scratch_name,
                &witness_name,
                &bytes,
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

fn publish_tree_witness(
    root: &File,
    retained: &RetainedRotationDebt,
    step: RecoveryStep,
    store_name: &CStr,
    directory: &File,
    directory_identity: &DirectoryIdentity,
    scratch_name: &CStr,
    witness_name: &CStr,
    bytes: &[u8],
) -> io::Result<()> {
    verify_named_rotation_tree(root, retained, step, store_name, directory_identity)?;
    let mut scratch = create_regular_exclusive(directory.as_raw_fd(), scratch_name)?;
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
    root: &File,
    retained: &RetainedRotationDebt,
    step: RecoveryStep,
    store_name: &CStr,
    directory: &File,
    directory_identity: &DirectoryIdentity,
    scratch_name: &CStr,
    witness_name: &CStr,
    bytes: &[u8],
    expected: &ReadComponent,
) -> io::Result<()> {
    verify_named_rotation_tree(root, retained, step, store_name, directory_identity)?;
    let scratch = read_component(directory.as_raw_fd(), scratch_name)?
        .ok_or_else(|| invalid_record("retired-tree witness scratch disappeared"))?;
    if scratch.identity != expected.identity || scratch.bytes != bytes {
        return Err(invalid_record("retired-tree witness scratch is ambiguous"));
    }
    ensure_component_identity(directory.as_raw_fd(), scratch_name, scratch.identity)?;
    directory.sync_all()?;
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
fn read_or_promote_scratch(
    cache_root: &Path,
    repository: &str,
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
            publish(cache_root, debt).map(Some)
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
            publish(cache_root, proposed).map(Some)
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

/// Restore the cache-root witness from a previously committed external
/// journal row. The external row is the durable publication authority when a
/// crash occurred before the in-cache witness was published or after it was
/// cleared. The caller holds the repository's exclusive preparation lease.
pub(crate) fn restore_from_journal(
    cache_root: &Path,
    debt: &RotationDebt,
) -> io::Result<RetainedRotationDebt> {
    debt.validate()?;
    let root = open_cache_root(cache_root)?;
    let root_identity = identity_for_directory(&root)?;
    if !root_identity.same_directory(&debt.cache_root) {
        return Err(invalid_record("cache root changed before journal recovery"));
    }
    match recovery_step_at(&root, debt)? {
        RecoveryStep::ClearCompletedDebt => {
            return Err(invalid_record(
                "completed journal debt does not need an in-cache witness",
            ));
        }
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
    directory_is_empty(&directory)
}

fn directory_is_empty(directory: &File) -> io::Result<bool> {
    directory_contains_only(directory, None)
}

/// Return true when the pinned directory has no entries except an optional
/// single known component. The directory stream is opened relative to the
/// retained descriptor so path replacement cannot redirect this check.
fn directory_contains_only(directory: &File, allowed: Option<&CStr>) -> io::Result<bool> {
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
        if entry_name != b"." && entry_name != b".." {
            let expected = allowed.map(CStr::to_bytes);
            if expected != Some(entry_name) {
                // SAFETY: stream is the live DIR* returned by fdopendir and is
                // closed exactly once here.
                unsafe { libc::closedir(stream) };
                return Ok(false);
            }
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
    use std::time::Instant;

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
        assert_eq!(debt.repository, REPOSITORY);
    }
}
