//! Bounded, no-follow inventory receipts for one finite broker-owned tree.
//!
//! This scanner reports OS-reported allocated blocks while the caller holds
//! the exact preparation-cache exclusive key. It is a bounded cooperative
//! accounting input, not a filesystem quota, exact physical-extent measure,
//! or constraint on non-broker writers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{
    HostLeaseState, HostResourceCoordinator, HostResourceError, HostResourceGrant,
    HostResourceLease, ManagedStorageClaim, ManagedStorageClaimRequest,
    ManagedStorageReservationResult, managed_storage_refusal,
    validate_managed_storage_claim_request,
};

const OWNER_TTL_MIN_SECONDS: u64 = 15;
const OWNER_TTL_MAX_SECONDS: u64 = 86_400;
const SCAN_MAX_ENTRIES: usize = 100_000;
const SCAN_MAX_DEPTH: usize = 64;
const SCAN_MAX_DURATION: Duration = Duration::from_secs(3);

/// A new reservation grants one materialization attempt. Existing rows are
/// returned as status and never grant another attempt.
#[derive(Debug)]
pub(crate) enum ManagedStorageAdmission {
    Materialize(ManagedStorageGrowthPermit),
    Existing(ManagedStorageClaim),
}

/// Capability for one newly inserted durable claim. It is not cloneable and is
/// consumed by either reconciliation or uncertainty recording.
#[derive(Debug)]
pub(crate) struct ManagedStorageGrowthPermit {
    request: ManagedStorageClaimRequest,
    lease_id: String,
    generation: u64,
}

/// Holds the exact preparation-cache exclusive lease while a finite tree is
/// admitted, inventoried, and settled. Dropping without finish deliberately
/// leaves the durable host lease to expire; it does not silently report a
/// successful release.
pub(crate) struct ManagedStorageOwner<'a> {
    coordinator: &'a mut HostResourceCoordinator,
    grant: HostResourceGrant,
    repository: String,
    root: RootBinding,
    ttl_seconds: u64,
}

impl<'a> ManagedStorageOwner<'a> {
    pub(super) fn new(
        coordinator: &'a mut HostResourceCoordinator,
        mut grant: HostResourceGrant,
        repository: &str,
        preparation_cache_root: &Path,
        materialization: &str,
        ttl_seconds: u64,
    ) -> Result<Self, HostResourceError> {
        if !(OWNER_TTL_MIN_SECONDS..=OWNER_TTL_MAX_SECONDS).contains(&ttl_seconds) {
            return Err(HostResourceError::InvalidRequest(format!(
                "managed-storage owner TTL must be {OWNER_TTL_MIN_SECONDS}..={OWNER_TTL_MAX_SECONDS} seconds"
            )));
        }
        validate_lower_hex_identifier("repository", repository, 64)?;
        validate_lower_hex_identifier("materialization", materialization, 12)?;
        if grant.lease.state != HostLeaseState::Active || grant.lease.repository != repository {
            return Err(managed_storage_refusal(
                "managed_storage_owner_mismatch",
                "owner grant must be active and belong to the requested repository",
            ));
        }
        let expected_key = crate::preparation::preparation_cache_lease_name(repository);
        let matching = grant
            .lease
            .allocations
            .iter()
            .filter(|allocation| {
                allocation.kind == "exclusive_key"
                    && allocation.key == "preparation_cache"
                    && allocation.value == expected_key
            })
            .count();
        if matching != 1 || grant.lease.allocations.len() != 1 {
            return Err(managed_storage_refusal(
                "managed_storage_owner_missing",
                "an exact preparation-cache exclusive-key grant is required",
            ));
        }

        let root = RootBinding::new(preparation_cache_root, repository, materialization)?;
        grant.lease = coordinator.renew(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
            ttl_seconds,
        )?;
        Ok(Self {
            coordinator,
            grant,
            repository: repository.into(),
            root,
            ttl_seconds,
        })
    }

    pub(crate) fn filesystem_id(&self) -> &str {
        &self.root.filesystem_id
    }

    /// A path-derived opaque identity suitable for a ManagedStorageClaimRequest.
    /// The absolute path itself is never written to the host-resource ledger.
    pub(crate) fn root_identity(&self) -> &str {
        &self.root.identity
    }

    /// Exact retries return status only. A new permit is returned only after a
    /// fresh-root check and insertion of a new durable reservation.
    pub(crate) fn reserve(
        &mut self,
        request: &ManagedStorageClaimRequest,
    ) -> Result<ManagedStorageAdmission, HostResourceError> {
        self.validate_request(request)?;
        self.renew()?;
        if let Some(existing) = self.coordinator.managed_storage_claim(
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )? {
            if existing.request != *request {
                return Err(HostResourceError::IdempotencyMismatch(
                    request.claim_id.clone(),
                ));
            }
            return Ok(ManagedStorageAdmission::Existing(existing));
        }

        self.renew()?;
        let inventory = scan_complete(&self.root, &self.grant, request)?;
        self.renew()?;
        let free_bytes =
            crate::disk_headroom::available_bytes(&self.root.parent).ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_free_bytes_unknown",
                    "filesystem free space could not be measured; reservation is refused",
                )
            })?;
        if inventory.root_present || inventory.allocated_bytes != 0 {
            return Err(managed_storage_refusal(
                "managed_storage_root_not_fresh",
                "a new storage claim requires its exact materialization root to be absent; existing bytes need a separate backfill proof",
            ));
        }

        match self
            .coordinator
            .reserve_managed_storage(request, free_bytes)?
        {
            ManagedStorageReservationResult::Created(_) => Ok(
                ManagedStorageAdmission::Materialize(ManagedStorageGrowthPermit {
                    request: request.clone(),
                    lease_id: self.grant.lease.lease_id.clone(),
                    generation: self.grant.lease.generation,
                }),
            ),
            ManagedStorageReservationResult::Existing(claim) => {
                // A caller that bypassed the exclusive-key protocol may have
                // won the idempotency race; it gets status, not a permit.
                Ok(ManagedStorageAdmission::Existing(claim))
            }
        }
    }

    /// Reconcile one new materialization. The complete receipt never escapes
    /// the owner and is consumed immediately after a post-scan lease renewal.
    pub(crate) fn reconcile_new(
        &mut self,
        permit: ManagedStorageGrowthPermit,
    ) -> Result<ManagedStorageClaim, HostResourceError> {
        self.validate_permit(&permit)?;
        self.renew()?;
        let inventory = scan_complete(&self.root, &self.grant, &permit.request)?;
        self.renew()?;
        self.consume_inventory(&permit.request, inventory)
    }

    /// Recover a reserved or uncertain attempt after restart. This inventories
    /// and settles only; it never grants permission to start another write.
    pub(crate) fn reconcile_existing(
        &mut self,
        request: &ManagedStorageClaimRequest,
    ) -> Result<ManagedStorageClaim, HostResourceError> {
        self.validate_request(request)?;
        let existing = self
            .coordinator
            .managed_storage_claim(
                &request.repository,
                &request.filesystem_id,
                &request.claim_id,
            )?
            .ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_claim_missing",
                    "there is no durable reservation to recover",
                )
            })?;
        if existing.request != *request {
            return Err(HostResourceError::IdempotencyMismatch(
                request.claim_id.clone(),
            ));
        }
        if !matches!(
            existing.state,
            super::ManagedStorageClaimState::Reserved | super::ManagedStorageClaimState::Uncertain
        ) {
            return Err(managed_storage_refusal(
                "managed_storage_claim_state",
                "recovery inventory is status-only unless the exact claim remains reserved or uncertain",
            ));
        }

        self.renew()?;
        let inventory = scan_complete(&self.root, &self.grant, request)?;
        self.renew()?;
        self.consume_inventory(request, inventory)
    }

    /// Retain the reservation as uncertain after failed or incomplete work.
    pub(crate) fn mark_uncertain(
        &mut self,
        permit: ManagedStorageGrowthPermit,
    ) -> Result<ManagedStorageClaim, HostResourceError> {
        self.validate_permit(&permit)?;
        self.renew()?;
        self.coordinator
            .mark_managed_storage_claim_uncertain(&permit.request)
    }

    pub(crate) fn finish(self) -> Result<HostResourceLease, HostResourceError> {
        self.coordinator.release(
            &self.grant.lease.lease_id,
            self.grant.lease.generation,
            &self.grant.ownership_token,
        )
    }

    fn validate_request(
        &self,
        request: &ManagedStorageClaimRequest,
    ) -> Result<(), HostResourceError> {
        validate_managed_storage_claim_request(request)?;
        if request.repository != self.repository
            || request.materialization != self.root.materialization
            || request.filesystem_id != self.root.filesystem_id
            || request.root_identity != self.root.identity
        {
            return Err(managed_storage_refusal(
                "managed_storage_owner_mismatch",
                "claim repository, filesystem, and root must match the held owner",
            ));
        }
        Ok(())
    }

    fn validate_permit(
        &self,
        permit: &ManagedStorageGrowthPermit,
    ) -> Result<(), HostResourceError> {
        self.validate_request(&permit.request)?;
        if permit.lease_id != self.grant.lease.lease_id
            || permit.generation != self.grant.lease.generation
        {
            return Err(managed_storage_refusal(
                "managed_storage_permit_owner_mismatch",
                "materialization permit belongs to another exclusive owner generation",
            ));
        }
        Ok(())
    }

    fn renew(&mut self) -> Result<(), HostResourceError> {
        self.grant.lease = self.coordinator.renew(
            &self.grant.lease.lease_id,
            self.grant.lease.generation,
            &self.grant.ownership_token,
            self.ttl_seconds,
        )?;
        Ok(())
    }

    fn consume_inventory(
        &mut self,
        request: &ManagedStorageClaimRequest,
        inventory: CompleteManagedStorageInventory,
    ) -> Result<ManagedStorageClaim, HostResourceError> {
        self.validate_request(request)?;
        inventory.validate_for(request, &self.grant)?;
        self.coordinator
            .reconcile_managed_storage_claim(request, inventory.allocated_bytes)
    }
}

#[derive(Debug)]
struct RootBinding {
    parent: PathBuf,
    leaf: std::ffi::OsString,
    materialization: String,
    parent_device: u64,
    parent_inode: u64,
    identity: String,
    filesystem_id: String,
}

#[derive(Debug)]
struct CompleteManagedStorageInventory {
    repository: String,
    filesystem_id: String,
    claim_id: String,
    request_digest: String,
    root_identity: String,
    lease_id: String,
    generation: u64,
    allocated_bytes: u64,
    entry_count: usize,
    inventory_digest: String,
    root_present: bool,
}

impl CompleteManagedStorageInventory {
    fn validate_for(
        &self,
        request: &ManagedStorageClaimRequest,
        grant: &HostResourceGrant,
    ) -> Result<(), HostResourceError> {
        let request_digest = super::managed_storage_claim_digest(request)?;
        if self.repository != request.repository
            || self.filesystem_id != request.filesystem_id
            || self.claim_id != request.claim_id
            || self.request_digest != request_digest
            || self.root_identity != request.root_identity
            || self.lease_id != grant.lease.lease_id
            || self.generation != grant.lease.generation
            || self.inventory_digest.is_empty()
            || self.entry_count == 0
        {
            return Err(managed_storage_refusal(
                "managed_storage_inventory_mismatch",
                "complete inventory receipt does not match this claim and exact owner generation",
            ));
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl RootBinding {
    fn new(
        preparation_cache_root: &Path,
        repository: &str,
        materialization: &str,
    ) -> Result<Self, HostResourceError> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::ffi::OsStrExt as _;

        let path = preparation_cache_root
            .join(repository)
            .join(materialization);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|source| HostResourceError::Io {
                    path: PathBuf::from("."),
                    source,
                })?
                .join(path)
        };
        let leaf = absolute.file_name().ok_or_else(|| {
            managed_storage_refusal(
                "managed_storage_root_invalid",
                "managed root must name a directory below a parent",
            )
        })?;
        if leaf == "." || leaf == ".." {
            return Err(managed_storage_refusal(
                "managed_storage_root_invalid",
                "managed root leaf cannot be dot or dot-dot",
            ));
        }
        let canonical_store = std::fs::canonicalize(preparation_cache_root).map_err(|source| {
            HostResourceError::Io {
                path: preparation_cache_root.to_path_buf(),
                source,
            }
        })?;
        let store_fd = open_absolute_directory_nofollow(&canonical_store)?;
        let store_stat = fstat(store_fd.as_raw_fd(), &canonical_store)?;
        if file_type(&store_stat) != libc::S_IFDIR {
            return Err(managed_storage_refusal(
                "managed_storage_root_invalid",
                "preparation cache root must resolve to a directory",
            ));
        }
        let repository_name = std::ffi::CString::new(repository)
            .map_err(|_| scan_refusal("repository identity contained an invalid NUL"))?;
        let repository_stat = match stat_at(
            store_fd.as_raw_fd(),
            &repository_name,
            libc::AT_SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) if file_type(&stat) == libc::S_IFDIR && stat.st_dev == store_stat.st_dev => {
                stat
            }
            Ok(_) => {
                return Err(managed_storage_refusal(
                    "managed_storage_repository_root_invalid",
                    "repository cache root must be a non-symlink directory on the preparation-cache filesystem",
                ));
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                return Err(managed_storage_refusal(
                    "managed_storage_repository_root_missing",
                    "repository cache root must be created under its exact exclusive lease before reserving a materialization",
                ));
            }
            Err(source) => {
                return Err(HostResourceError::Io {
                    path: canonical_store.join(repository),
                    source,
                });
            }
        };
        let repository_fd = openat_directory(
            store_fd.as_raw_fd(),
            &repository_name,
            &canonical_store.join(repository),
        )?;
        let opened_repository_stat =
            fstat(repository_fd.as_raw_fd(), &canonical_store.join(repository))?;
        if !same_identity(&repository_stat, &opened_repository_stat) {
            return Err(scan_refusal(
                "repository cache root changed while it was being opened",
            ));
        }
        let canonical_parent = canonical_store.join(repository);
        let parent_fd = open_absolute_directory_nofollow(&canonical_parent)?;
        let parent_stat = fstat(parent_fd.as_raw_fd(), &canonical_parent)?;
        if !same_identity(&opened_repository_stat, &parent_stat) {
            return Err(scan_refusal(
                "repository cache root changed while its parent binding was established",
            ));
        }
        let leaf_string = leaf.to_os_string();
        let leaf_c = cstring_from_os(&leaf_string)?;
        match stat_at(parent_fd.as_raw_fd(), &leaf_c, libc::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) if file_type(&stat) == libc::S_IFDIR && stat.st_dev == parent_stat.st_dev => {}
            Ok(_) => {
                return Err(managed_storage_refusal(
                    "managed_storage_root_invalid",
                    "existing managed root must be a non-symlink directory on its parent filesystem",
                ));
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            Err(source) => {
                return Err(HostResourceError::Io {
                    path: canonical_parent.join(&leaf_string),
                    source,
                });
            }
        }

        let mut path_bytes = canonical_parent.as_os_str().as_bytes().to_vec();
        path_bytes.push(b'/');
        path_bytes.extend_from_slice(leaf_string.as_bytes());
        let mut hash = Sha256::new();
        hash.update(b"aethyme-managed-storage-root-v1\0");
        hash.update(&path_bytes);
        let identity = format!("path-v1:{:x}", hash.finalize());
        let filesystem_id = filesystem_identity(parent_stat.st_dev as u64);
        Ok(Self {
            parent: canonical_parent,
            leaf: leaf_string,
            materialization: materialization.to_owned(),
            parent_device: parent_stat.st_dev as u64,
            parent_inode: parent_stat.st_ino as u64,
            identity,
            filesystem_id,
        })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl RootBinding {
    fn new(
        _preparation_cache_root: &Path,
        _repository: &str,
        _materialization: &str,
    ) -> Result<Self, HostResourceError> {
        Err(managed_storage_refusal(
            "managed_storage_inventory_unsupported",
            "bounded managed-storage inventory is currently supported only on macOS and Linux",
        ))
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn scan_complete(
    root: &RootBinding,
    grant: &HostResourceGrant,
    request: &ManagedStorageClaimRequest,
) -> Result<CompleteManagedStorageInventory, HostResourceError> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    let parent_fd = open_absolute_directory_nofollow(&root.parent)?;
    let leaf_c = cstring_from_os(&root.leaf)?;
    let parent_before = fstat(parent_fd.as_raw_fd(), &root.parent)?;
    let leaf_path = root.parent.join(&root.leaf);
    let root_stat = match stat_at(parent_fd.as_raw_fd(), &leaf_c, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => Some(stat),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => None,
        Err(source) => {
            return Err(HostResourceError::Io {
                path: leaf_path.clone(),
                source,
            });
        }
    };
    let expected_device = parse_filesystem_identity(&root.filesystem_id)?;
    if parent_before.st_dev as u64 != expected_device
        || parent_before.st_dev as u64 != root.parent_device
        || parent_before.st_ino as u64 != root.parent_inode
    {
        return Err(managed_storage_refusal(
            "managed_storage_filesystem_changed",
            "managed root parent identity or filesystem changed after owner creation",
        ));
    }

    let mut scanner = Scanner::new(expected_device, Instant::now() + SCAN_MAX_DURATION);
    let root_present = if let Some(root_stat) = root_stat {
        if file_type(&root_stat) != libc::S_IFDIR || root_stat.st_dev as u64 != expected_device {
            return Err(managed_storage_refusal(
                "managed_storage_root_invalid",
                "managed root must remain a directory on the owner's filesystem",
            ));
        }
        let root_fd = openat_directory(parent_fd.as_raw_fd(), &leaf_c, &leaf_path)?;
        let opened_stat = fstat(root_fd.as_raw_fd(), &leaf_path)?;
        if !same_identity(&root_stat, &opened_stat) {
            return Err(scan_refusal(
                "managed root changed while it was being opened",
            ));
        }
        scanner.visit_directory(root_fd.as_raw_fd(), &leaf_path, 0, Some(&opened_stat))?;
        let named_after = stat_at(parent_fd.as_raw_fd(), &leaf_c, libc::AT_SYMLINK_NOFOLLOW)
            .map_err(|source| HostResourceError::Io {
                path: leaf_path.clone(),
                source,
            })?;
        if !same_identity(&opened_stat, &named_after) {
            return Err(scan_refusal("managed root name changed during inventory"));
        }
        true
    } else {
        false
    };

    let parent_after = fstat(parent_fd.as_raw_fd(), &root.parent)?;
    let named_parent_fd = open_absolute_directory_nofollow(&root.parent)?;
    let named_parent_after = fstat(named_parent_fd.as_raw_fd(), &root.parent)?;
    if !same_identity(&parent_before, &parent_after)
        || !same_identity(&parent_before, &named_parent_after)
    {
        return Err(scan_refusal("managed root parent changed during inventory"));
    }
    if !root_present {
        match stat_at(parent_fd.as_raw_fd(), &leaf_c, libc::AT_SYMLINK_NOFOLLOW) {
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            _ => {
                return Err(scan_refusal(
                    "managed root appeared during absent-root inventory",
                ));
            }
        }
    }

    let mut receipt_hash = scanner.digest;
    receipt_hash.update(root.parent.as_os_str().as_bytes());
    receipt_hash.update(root.leaf.as_bytes());
    receipt_hash.update(scanner.allocated_bytes.to_le_bytes());
    receipt_hash.update((scanner.entry_count as u64).to_le_bytes());
    Ok(CompleteManagedStorageInventory {
        repository: request.repository.clone(),
        filesystem_id: root.filesystem_id.clone(),
        claim_id: request.claim_id.clone(),
        request_digest: super::managed_storage_claim_digest(request)?,
        root_identity: root.identity.clone(),
        lease_id: grant.lease.lease_id.clone(),
        generation: grant.lease.generation,
        allocated_bytes: scanner.allocated_bytes,
        entry_count: scanner.entry_count.max(1),
        inventory_digest: format!("{:x}", receipt_hash.finalize()),
        root_present,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn scan_complete(
    _root: &RootBinding,
    _grant: &HostResourceGrant,
    _request: &ManagedStorageClaimRequest,
) -> Result<CompleteManagedStorageInventory, HostResourceError> {
    Err(managed_storage_refusal(
        "managed_storage_inventory_unsupported",
        "bounded managed-storage inventory is currently supported only on macOS and Linux",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Scanner {
    device: u64,
    deadline: Instant,
    entry_count: usize,
    allocated_bytes: u64,
    seen_inodes: std::collections::HashSet<(u64, u64)>,
    digest: Sha256,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Scanner {
    fn new(device: u64, deadline: Instant) -> Self {
        Self {
            device,
            deadline,
            entry_count: 0,
            allocated_bytes: 0,
            seen_inodes: std::collections::HashSet::new(),
            digest: Sha256::new(),
        }
    }

    fn visit_directory(
        &mut self,
        fd: libc::c_int,
        path: &Path,
        depth: usize,
        expected: Option<&libc::stat>,
    ) -> Result<(), HostResourceError> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::ffi::OsStrExt as _;

        if depth > SCAN_MAX_DEPTH {
            return Err(scan_refusal("directory depth limit exceeded"));
        }
        self.check_deadline(path)?;
        let dir_stat = fstat(fd, path)?;
        if dir_stat.st_dev as u64 != self.device || file_type(&dir_stat) != libc::S_IFDIR {
            return Err(scan_refusal("directory crossed filesystem or changed type"));
        }
        if let Some(expected) = expected {
            if !same_identity(expected, &dir_stat) {
                return Err(scan_refusal(
                    "directory identity changed before enumeration",
                ));
            }
        }
        self.count_stat(&dir_stat, b".")?;

        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate < 0 {
            return Err(io_failure(path));
        }
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            unsafe { libc::close(duplicate) };
            return Err(io_failure(path));
        }
        let stream = DirectoryStream(stream);
        let mut names = Vec::<Vec<u8>>::new();
        loop {
            set_errno(0);
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                if get_errno() != 0 {
                    return Err(io_failure(path));
                }
                break;
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if names.len() % 256 == 0 {
                self.check_deadline(path)?;
            }
            if self.entry_count.saturating_add(names.len()) >= SCAN_MAX_ENTRIES {
                return Err(scan_refusal("entry limit exceeded"));
            }
            names.push(name.to_vec());
        }
        names.sort();

        for name_bytes in names {
            self.check_deadline(path)?;
            let name = std::ffi::CString::new(name_bytes)
                .map_err(|_| scan_refusal("directory entry contained an invalid NUL"))?;
            let child_stat = stat_at(fd, &name, libc::AT_SYMLINK_NOFOLLOW).map_err(|_| {
                scan_refusal("entry metadata could not be read without following links")
            })?;
            if child_stat.st_dev as u64 != self.device {
                return Err(scan_refusal("entry crossed onto another filesystem"));
            }
            let child_path = path.join(std::ffi::OsStr::from_bytes(name.as_bytes()));
            if file_type(&child_stat) == libc::S_IFDIR {
                let child = openat_directory(fd, &name, &child_path)?;
                let opened_stat = fstat(child.as_raw_fd(), &child_path)?;
                if !same_identity(&child_stat, &opened_stat) {
                    return Err(scan_refusal("directory entry changed while opening"));
                }
                self.visit_directory(
                    child.as_raw_fd(),
                    &child_path,
                    depth + 1,
                    Some(&opened_stat),
                )?;
            } else {
                self.count_stat(&child_stat, name.as_bytes())?;
                if file_type(&child_stat) == libc::S_IFREG {
                    let child = openat_regular(fd, &name, &child_path)?;
                    let opened_stat = fstat(child.as_raw_fd(), &child_path)?;
                    if !same_identity(&child_stat, &opened_stat) {
                        return Err(scan_refusal("regular file changed while opening"));
                    }
                }
            }
        }

        let after = fstat(fd, path)?;
        if !same_identity(&dir_stat, &after) {
            return Err(scan_refusal("directory changed during inventory"));
        }
        Ok(())
    }

    fn count_stat(&mut self, stat: &libc::stat, name: &[u8]) -> Result<(), HostResourceError> {
        if self
            .seen_inodes
            .insert((stat.st_dev as u64, stat.st_ino as u64))
        {
            let blocks = u64::try_from(stat.st_blocks)
                .map_err(|_| scan_refusal("negative allocated block count"))?;
            let bytes = blocks
                .checked_mul(512)
                .ok_or_else(|| scan_refusal("allocated byte count overflowed"))?;
            self.allocated_bytes = self
                .allocated_bytes
                .checked_add(bytes)
                .ok_or_else(|| scan_refusal("total allocated byte count overflowed"))?;
        }
        self.entry_count = self
            .entry_count
            .checked_add(1)
            .ok_or_else(|| scan_refusal("entry count overflowed"))?;
        self.digest.update((name.len() as u64).to_le_bytes());
        self.digest.update(name);
        self.digest.update((stat.st_dev as u64).to_le_bytes());
        self.digest.update((stat.st_ino as u64).to_le_bytes());
        self.digest.update((stat.st_mode as u64).to_le_bytes());
        self.digest.update((stat.st_size as i64).to_le_bytes());
        self.digest.update((stat.st_blocks as i64).to_le_bytes());
        if self.entry_count > SCAN_MAX_ENTRIES {
            return Err(scan_refusal("entry limit exceeded"));
        }
        Ok(())
    }

    fn check_deadline(&self, path: &Path) -> Result<(), HostResourceError> {
        if Instant::now() >= self.deadline {
            return Err(scan_refusal(&format!(
                "inventory exceeded its bounded deadline at {}",
                path.display()
            )));
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct DirectoryStream(*mut libc::DIR);

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for DirectoryStream {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0);
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_absolute_directory_nofollow(
    path: &Path,
) -> Result<std::os::fd::OwnedFd, HostResourceError> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;

    let root = std::ffi::CString::new("/").expect("static path has no NUL");
    let raw = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw < 0 {
        return Err(io_failure(Path::new("/")));
    }
    let mut current = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    for component in path.components() {
        let std::path::Component::Normal(name) = component else {
            if matches!(
                component,
                std::path::Component::RootDir | std::path::Component::CurDir
            ) {
                continue;
            }
            return Err(scan_refusal(
                "canonical parent contains an unexpected path component",
            ));
        };
        let name = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| scan_refusal("directory component contained an invalid NUL"))?;
        let next = unsafe {
            libc::openat(
                current.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if next < 0 {
            return Err(io_failure(path));
        }
        current = unsafe { std::os::fd::OwnedFd::from_raw_fd(next) };
    }
    Ok(current)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn openat_directory(
    parent: libc::c_int,
    name: &std::ffi::CStr,
    path: &Path,
) -> Result<std::os::fd::OwnedFd, HostResourceError> {
    use std::os::fd::FromRawFd as _;
    let raw = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw < 0 {
        return Err(io_failure(path));
    }
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn openat_regular(
    parent: libc::c_int,
    name: &std::ffi::CStr,
    path: &Path,
) -> Result<std::fs::File, HostResourceError> {
    use std::os::fd::FromRawFd as _;
    let raw = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if raw < 0 {
        return Err(io_failure(path));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(raw) })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stat_at(
    parent: libc::c_int,
    name: &std::ffi::CStr,
    flags: libc::c_int,
) -> std::io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstatat(parent, name.as_ptr(), stat.as_mut_ptr(), flags) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { stat.assume_init() })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn fstat(fd: libc::c_int, path: &Path) -> Result<libc::stat, HostResourceError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } < 0 {
        return Err(io_failure(path));
    }
    Ok(unsafe { stat.assume_init() })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn cstring_from_os(value: &std::ffi::OsStr) -> Result<std::ffi::CString, HostResourceError> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(value.as_bytes())
        .map_err(|_| scan_refusal("path component contained an invalid NUL"))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn file_type(stat: &libc::stat) -> libc::mode_t {
    stat.st_mode & libc::S_IFMT
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn same_identity(left: &libc::stat, right: &libc::stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && file_type(left) == file_type(right)
        && left.st_mode == right.st_mode
        && left.st_size == right.st_size
        && left.st_blocks == right.st_blocks
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn parse_filesystem_identity(value: &str) -> Result<u64, HostResourceError> {
    let Some(hex) = value.strip_prefix("unix-dev:") else {
        return Err(managed_storage_refusal(
            "managed_storage_filesystem_invalid",
            "filesystem identity is not an operating-system device identity",
        ));
    };
    u64::from_str_radix(hex, 16).map_err(|_| {
        managed_storage_refusal(
            "managed_storage_filesystem_invalid",
            "filesystem identity has an invalid device encoding",
        )
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn filesystem_identity(device: u64) -> String {
    format!("unix-dev:{device:016x}")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn filesystem_identity(_device: u64) -> String {
    "unsupported".into()
}

fn validate_lower_hex_identifier(
    field: &str,
    value: &str,
    expected_length: usize,
) -> Result<(), HostResourceError> {
    if value.len() != expected_length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(managed_storage_refusal(
            "managed_storage_materialization_invalid",
            &format!("{field} must be exactly {expected_length} lowercase hexadecimal characters"),
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn scan_refusal(message: &str) -> HostResourceError {
    managed_storage_refusal("managed_storage_inventory_incomplete", message)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn io_failure(path: &Path) -> HostResourceError {
    HostResourceError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::last_os_error(),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn set_errno(value: libc::c_int) {
    unsafe {
        *errno_location() = value;
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn get_errno() -> libc::c_int {
    unsafe { *errno_location() }
}

#[cfg(target_os = "macos")]
unsafe fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__error() }
}

#[cfg(target_os = "linux")]
unsafe fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}
