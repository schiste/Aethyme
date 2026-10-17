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

    /// Capture root-instance identity for an existing charged ledger row that
    /// has no lineage metadata yet. This only backfills a charge when a complete
    /// scan exactly matches persisted measured bytes; it never starts a write or
    /// adopts an untracked legacy cache root. Full pre-ledger root adoption needs
    /// a separate complete inventory and claim-creation path before activation.
    pub(crate) fn backfill_current_charge(
        &mut self,
        claim_id: &str,
    ) -> Result<ManagedStorageClaim, HostResourceError> {
        self.renew()?;
        let claim = self
            .coordinator
            .managed_storage_claim(&self.repository, &self.root.filesystem_id, claim_id)?
            .ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_claim_missing",
                    "there is no persisted claim to backfill",
                )
            })?;
        self.validate_request(&claim.request)?;
        if claim.state != super::ManagedStorageClaimState::Charged {
            return Err(managed_storage_refusal(
                "managed_storage_backfill_state",
                "only a measured charged claim can be backfilled",
            ));
        }
        let inventory = scan_complete(&self.root, &self.grant, &claim.request)?;
        self.renew()?;
        inventory.validate_for(&claim.request, &self.grant)?;
        if !inventory.root_present
            || inventory.root_instance_id.is_none()
            || claim.measured_bytes != Some(inventory.allocated_bytes)
        {
            return Err(managed_storage_refusal(
                "managed_storage_backfill_mismatch",
                "complete current inventory must match the exact persisted charge before backfill",
            ));
        }
        self.coordinator.backfill_managed_storage_lineage(
            &claim.request,
            inventory.allocated_bytes,
            inventory.root_instance_id.as_deref().ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_root_instance_missing",
                    "present root has no stable instance identity",
                )
            })?,
        )
    }

    /// Admit a new finite write to a previously charged cache root. The
    /// predecessor charge stays counted while the new claim reserves only its
    /// incremental growth. The same root and baseline are rescanned while the
    /// exact preparation-cache owner is held.
    pub(crate) fn reserve_mutation(
        &mut self,
        request: &ManagedStorageClaimRequest,
        predecessor_claim_id: &str,
    ) -> Result<ManagedStorageAdmission, HostResourceError> {
        self.validate_request(request)?;
        if predecessor_claim_id == request.claim_id {
            return Err(managed_storage_refusal(
                "managed_storage_self_predecessor",
                "a mutation claim cannot name itself as its charged predecessor",
            ));
        }
        self.renew()?;
        if let Some(existing) = self.coordinator.managed_storage_claim(
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )? {
            let lineage = super::load_managed_storage_lineage(
                &self.coordinator.conn,
                &request.repository,
                &request.filesystem_id,
                &request.claim_id,
            )?;
            if existing.request != *request
                || lineage
                    .as_ref()
                    .and_then(|row| row.predecessor_claim_id.as_deref())
                    != Some(predecessor_claim_id)
            {
                return Err(HostResourceError::IdempotencyMismatch(
                    request.claim_id.clone(),
                ));
            }
            return Ok(ManagedStorageAdmission::Existing(existing));
        }

        let predecessor = self
            .coordinator
            .managed_storage_claim(
                &request.repository,
                &request.filesystem_id,
                predecessor_claim_id,
            )?
            .ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_predecessor_missing",
                    "the charged predecessor claim does not exist",
                )
            })?;
        self.validate_request(&predecessor.request)?;
        if predecessor.state != super::ManagedStorageClaimState::Charged
            || predecessor.request.materialization != request.materialization
            || predecessor.request.root_identity != request.root_identity
        {
            return Err(managed_storage_refusal(
                "managed_storage_predecessor_stale",
                "mutation requires the current charged claim for this exact root",
            ));
        }
        let baseline_bytes = predecessor.measured_bytes.ok_or_else(|| {
            managed_storage_refusal(
                "managed_storage_predecessor_unmeasured",
                "predecessor has no measured size",
            )
        })?;
        let lineage = super::load_managed_storage_lineage(
            &self.coordinator.conn,
            &request.repository,
            &request.filesystem_id,
            predecessor_claim_id,
        )?
        .ok_or_else(|| {
            managed_storage_refusal(
                "managed_storage_backfill_required",
                "the predecessor root identity must be backfilled before mutation",
            )
        })?;
        if lineage.superseded_by_claim_id.is_some() || lineage.root_instance_id.is_none() {
            return Err(managed_storage_refusal(
                "managed_storage_predecessor_stale",
                "the predecessor is superseded or has no pinned root instance",
            ));
        }
        let inventory = scan_complete(&self.root, &self.grant, &predecessor.request)?;
        self.renew()?;
        inventory.validate_for(&predecessor.request, &self.grant)?;
        if !inventory.root_present
            || inventory.allocated_bytes != baseline_bytes
            || inventory.root_instance_id != lineage.root_instance_id
        {
            return Err(managed_storage_refusal(
                "managed_storage_mutation_baseline_changed",
                "the exact charged root no longer matches its measured baseline and instance",
            ));
        }
        let free_bytes =
            crate::disk_headroom::available_bytes(&self.root.parent).ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_free_bytes_unknown",
                    "filesystem free space could not be measured; mutation reservation is refused",
                )
            })?;
        let root_instance_id = lineage.root_instance_id.ok_or_else(|| {
            managed_storage_refusal(
                "managed_storage_predecessor_unmeasured",
                "predecessor has no root-instance identity",
            )
        })?;
        let baseline = super::ManagedStorageMutationBaseline {
            predecessor_claim_id: predecessor_claim_id.to_owned(),
            measured_bytes: baseline_bytes,
            root_instance_id,
        };
        match self
            .coordinator
            .reserve_managed_storage_mutation(request, free_bytes, baseline)?
        {
            ManagedStorageReservationResult::Created(_) => Ok(
                ManagedStorageAdmission::Materialize(ManagedStorageGrowthPermit {
                    request: request.clone(),
                    lease_id: self.grant.lease.lease_id.clone(),
                    generation: self.grant.lease.generation,
                }),
            ),
            ManagedStorageReservationResult::Existing(claim) => {
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
        self.coordinator.reconcile_managed_storage_claim(
            request,
            inventory.allocated_bytes,
            inventory.root_instance_id.as_deref(),
            inventory.root_present,
        )
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
    root_instance_id: Option<String>,
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
            || self.root_present != self.root_instance_id.is_some()
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
        || parent_before.st_ino != root.parent_inode
    {
        return Err(managed_storage_refusal(
            "managed_storage_filesystem_changed",
            "managed root parent identity or filesystem changed after owner creation",
        ));
    }

    let mut scanner = Scanner::new(expected_device, Instant::now() + SCAN_MAX_DURATION);
    let mut root_instance_id = None;
    let root_present = if let Some(root_stat) = root_stat {
        if file_type(&root_stat) != libc::S_IFDIR || root_stat.st_dev as u64 != expected_device {
            return Err(managed_storage_refusal(
                "managed_storage_root_invalid",
                "managed root must remain a directory on the owner's filesystem",
            ));
        }
        let root_fd = openat_directory(parent_fd.as_raw_fd(), &leaf_c, &leaf_path)?;
        let opened_stat = fstat(root_fd.as_raw_fd(), &leaf_path)?;
        ensure_same_identity(
            &root_stat,
            &opened_stat,
            "managed root changed while it was being opened",
        )?;
        root_instance_id = Some(format!(
            "unix-inode-v1:{}:{}",
            opened_stat.st_dev, opened_stat.st_ino
        ));
        scanner.visit_directory(root_fd.as_raw_fd(), &leaf_path, 0, Some(&opened_stat))?;
        let named_after = stat_at(parent_fd.as_raw_fd(), &leaf_c, libc::AT_SYMLINK_NOFOLLOW)
            .map_err(|source| HostResourceError::Io {
                path: leaf_path.clone(),
                source,
            })?;
        ensure_same_identity(
            &opened_stat,
            &named_after,
            "managed root name changed during inventory",
        )?;
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
    if let Some(root_instance_id) = &root_instance_id {
        receipt_hash.update(root_instance_id.as_bytes());
    }
    Ok(CompleteManagedStorageInventory {
        repository: request.repository.clone(),
        filesystem_id: root.filesystem_id.clone(),
        claim_id: request.claim_id.clone(),
        request_digest: super::managed_storage_claim_digest(request)?,
        root_identity: root.identity.clone(),
        lease_id: grant.lease.lease_id.clone(),
        generation: grant.lease.generation,
        allocated_bytes: scanner.allocated_bytes,
        root_instance_id,
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
        if expected.is_some_and(|expected| !same_identity(expected, &dir_stat)) {
            return Err(scan_refusal(
                "directory identity changed before enumeration",
            ));
        }
        self.count_stat(&dir_stat, b".")?;

        // SAFETY: `fd` is the live directory descriptor checked above; `fcntl`
        // duplicates it and returns either a new owned descriptor or -1.
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate < 0 {
            return Err(io_failure(path));
        }
        // SAFETY: `duplicate` is a fresh descriptor owned by this function;
        // on success `fdopendir` takes ownership, and on failure we close it.
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            // SAFETY: `fdopendir` failed and therefore did not take ownership
            // of this freshly duplicated descriptor.
            unsafe { libc::close(duplicate) };
            return Err(io_failure(path));
        }
        let stream = DirectoryStream(stream);
        let mut names = Vec::<Vec<u8>>::new();
        loop {
            set_errno(0);
            // SAFETY: `stream` uniquely owns this DIR pointer and is used only
            // by this scan; `readdir` returns a borrowed entry for that stream.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                if get_errno() != 0 {
                    return Err(io_failure(path));
                }
                break;
            }
            // SAFETY: `readdir` returned a live entry whose `d_name` is
            // NUL-terminated and remains valid until the next `readdir` call.
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if names.len().is_multiple_of(256) {
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
                ensure_same_identity(
                    &child_stat,
                    &opened_stat,
                    "directory entry changed while opening",
                )?;
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
                    ensure_same_identity(
                        &child_stat,
                        &opened_stat,
                        "regular file changed while opening",
                    )?;
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
        if self.seen_inodes.insert((stat.st_dev as u64, stat.st_ino)) {
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
        self.digest.update(stat.st_ino.to_le_bytes());
        self.digest.update((stat.st_mode as u64).to_le_bytes());
        self.digest.update(stat.st_size.to_le_bytes());
        self.digest.update(stat.st_blocks.to_le_bytes());
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
        // SAFETY: this wrapper uniquely owns the non-null DIR pointer returned
        // by `fdopendir`; `closedir` closes its descriptor and frees the stream.
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
    // SAFETY: `root` is a valid NUL-terminated path for this call; `open`
    // returns a new descriptor on success, which is adopted below.
    let raw = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw < 0 {
        return Err(io_failure(Path::new("/")));
    }
    // SAFETY: `open` succeeded, so `raw` is a newly owned descriptor and is
    // transferred exactly once into `OwnedFd`.
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
        // SAFETY: `current` is a live directory descriptor and `name` is a
        // valid NUL-terminated component; `openat` returns a new descriptor.
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
        // SAFETY: `openat` succeeded, so `next` is a newly owned descriptor;
        // transferring it drops the previous parent descriptor exactly once.
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
    // SAFETY: `parent` is a live directory descriptor and `name` is a valid
    // NUL-terminated entry name; `openat` returns a new descriptor.
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
    // SAFETY: `openat` succeeded and returned a newly owned descriptor.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn openat_regular(
    parent: libc::c_int,
    name: &std::ffi::CStr,
    path: &Path,
) -> Result<std::fs::File, HostResourceError> {
    use std::os::fd::FromRawFd as _;
    // SAFETY: `parent` is a live directory descriptor and `name` is a valid
    // NUL-terminated entry name; `openat` returns a new descriptor.
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
    // SAFETY: `openat` succeeded and returned a newly owned descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(raw) })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stat_at(
    parent: libc::c_int,
    name: &std::ffi::CStr,
    flags: libc::c_int,
) -> std::io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `parent` is an open directory descriptor, `name` is NUL
    // terminated, and `stat` points to writable uninitialized output storage.
    if unsafe { libc::fstatat(parent, name.as_ptr(), stat.as_mut_ptr(), flags) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful `fstatat` initialized the complete `libc::stat` value.
    Ok(unsafe { stat.assume_init() })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn fstat(fd: libc::c_int, path: &Path) -> Result<libc::stat, HostResourceError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fd` is an open descriptor and `stat` points to writable
    // uninitialized output storage for the system call.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } < 0 {
        return Err(io_failure(path));
    }
    // SAFETY: successful `fstat` initialized the complete `libc::stat` value.
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
fn ensure_same_identity(
    expected: &libc::stat,
    actual: &libc::stat,
    message: &str,
) -> Result<(), HostResourceError> {
    if same_identity(expected, actual) {
        Ok(())
    } else {
        Err(scan_refusal(message))
    }
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
    // SAFETY: the platform errno accessor returns a writable pointer to this
    // thread's errno slot, valid for the current thread.
    unsafe {
        *errno_location() = value;
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn get_errno() -> libc::c_int {
    // SAFETY: the platform errno accessor returns a readable pointer to this
    // thread's errno slot, valid for the current thread.
    unsafe { *errno_location() }
}

#[cfg(target_os = "macos")]
unsafe fn errno_location() -> *mut libc::c_int {
    // SAFETY: `__error` is the macOS libc accessor for this thread's errno slot.
    unsafe { libc::__error() }
}

#[cfg(target_os = "linux")]
unsafe fn errno_location() -> *mut libc::c_int {
    // SAFETY: `__errno_location` is the Linux libc accessor for this thread's errno slot.
    unsafe { libc::__errno_location() }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use crate::resources::{
        HostResourceKind, HostResourceRequest, HostResourceRequirement, ManagedStoragePolicy,
        VerifiedManagedStorageRemoval,
    };
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::symlink;

    const REPOSITORY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const MATERIALIZATION: &str = "bbbbbbbbbbbb";

    struct Fixture {
        _temp: tempfile::TempDir,
        database: PathBuf,
        preparation_cache_root: PathBuf,
        repository_root: PathBuf,
        materialization_root: PathBuf,
        repository: String,
        materialization: String,
        filesystem_id: String,
        root_identity: String,
        lease_request: HostResourceRequest,
        coordinator: HostResourceCoordinator,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_resources(vec![preparation_requirement(REPOSITORY)])
        }

        fn with_resources(resources: Vec<HostResourceRequirement>) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let preparation_cache_root = temp.path().join("preparation-cache");
            let repository_root = preparation_cache_root.join(REPOSITORY);
            let materialization_root = repository_root.join(MATERIALIZATION);
            std::fs::create_dir_all(&repository_root).unwrap();

            let binding =
                RootBinding::new(&preparation_cache_root, REPOSITORY, MATERIALIZATION).unwrap();
            let filesystem_id = binding.filesystem_id.clone();
            let root_identity = binding.identity.clone();
            let database = temp.path().join("host-resources.db");
            let mut coordinator = HostResourceCoordinator::open(&database).unwrap();
            coordinator
                .set_managed_storage_policy(&ManagedStoragePolicy {
                    repository: REPOSITORY.into(),
                    filesystem_id: filesystem_id.clone(),
                    managed_cap_bytes: 1_000_000_000,
                    free_reserve_bytes: 0,
                })
                .unwrap();

            let lease_request = HostResourceRequest {
                schema_version: crate::resources::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
                request_id: "managed-storage-owner-lease".into(),
                repository: REPOSITORY.into(),
                worktree_fingerprint: "fixture-worktree".into(),
                run_id: "managed-storage-owner-run".into(),
                ttl_seconds: 60,
                holder_pid: Some(std::process::id()),
                resources,
            };

            Self {
                _temp: temp,
                database,
                preparation_cache_root,
                repository_root,
                materialization_root,
                repository: REPOSITORY.into(),
                materialization: MATERIALIZATION.into(),
                filesystem_id,
                root_identity,
                lease_request,
                coordinator,
            }
        }

        fn owner(&mut self) -> Result<ManagedStorageOwner<'_>, HostResourceError> {
            let grant = self.coordinator.acquire(&self.lease_request)?;
            let repository = self.repository.clone();
            let cache_root = self.preparation_cache_root.clone();
            let materialization = self.materialization.clone();
            self.coordinator.begin_managed_storage_owner(
                grant,
                &repository,
                &cache_root,
                &materialization,
                60,
            )
        }

        fn claim(&self, claim_id: &str, reserved_bytes: u64) -> ManagedStorageClaimRequest {
            ManagedStorageClaimRequest {
                repository: self.repository.clone(),
                filesystem_id: self.filesystem_id.clone(),
                claim_id: claim_id.into(),
                materialization: self.materialization.clone(),
                root_identity: self.root_identity.clone(),
                reserved_bytes,
            }
        }
    }

    fn preparation_requirement(repository: &str) -> HostResourceRequirement {
        HostResourceRequirement {
            key: "preparation_cache".into(),
            resource: HostResourceKind::ExclusiveKey {
                name: crate::preparation::preparation_cache_lease_name(repository),
            },
        }
    }

    fn extra_requirement() -> HostResourceRequirement {
        HostResourceRequirement {
            key: "other_cache".into(),
            resource: HostResourceKind::ExclusiveKey {
                name: "some-other-cache".into(),
            },
        }
    }

    fn conflict_code(error: &HostResourceError) -> Option<&str> {
        match error {
            HostResourceError::Conflict { code, .. } => Some(code),
            _ => None,
        }
    }

    fn expect_permit(admission: ManagedStorageAdmission) -> ManagedStorageGrowthPermit {
        match admission {
            ManagedStorageAdmission::Materialize(permit) => permit,
            ManagedStorageAdmission::Existing(claim) => {
                panic!("expected a new materialization permit, got {claim:?}")
            }
        }
    }

    #[test]
    fn managed_storage_owner_requires_only_the_exact_preparation_lease() {
        let cases = [
            vec![extra_requirement()],
            vec![preparation_requirement(REPOSITORY), extra_requirement()],
        ];
        for resources in cases {
            let mut fixture = Fixture::with_resources(resources);
            let result = fixture.owner();
            let error = match result {
                Err(error) => error,
                Ok(owner) => {
                    drop(owner);
                    panic!("wrong or mixed lease unexpectedly created an owner")
                }
            };
            assert_eq!(
                conflict_code(&error),
                Some("managed_storage_owner_missing"),
                "unexpected owner rejection: {error:?}"
            );
        }
    }

    #[test]
    fn managed_storage_owner_refuses_an_absent_repository_parent() {
        let temp = tempfile::tempdir().unwrap();
        let cache_root = temp.path().join("preparation-cache");
        std::fs::create_dir(&cache_root).unwrap();
        let error = RootBinding::new(&cache_root, REPOSITORY, MATERIALIZATION).unwrap_err();
        assert_eq!(
            conflict_code(&error),
            Some("managed_storage_repository_root_missing"),
            "unexpected absent-parent result: {error:?}"
        );
    }

    #[test]
    fn managed_storage_owner_rejects_repository_root_replacement_before_reserving() {
        let mut fixture = Fixture::new();
        let repository_root = fixture.repository_root.clone();
        let old_root = fixture._temp.path().join("repository-cache-old");
        let outside = fixture._temp.path().join("outside-repository");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("payload"), b"must remain outside inventory").unwrap();
        let request = fixture.claim("replaced-parent", 4096);
        let repository = fixture.repository.clone();
        let filesystem_id = fixture.filesystem_id.clone();
        let mut owner = fixture.owner().unwrap();

        std::fs::rename(&repository_root, &old_root).unwrap();
        symlink(&outside, &repository_root).unwrap();
        let error = owner.reserve(&request).unwrap_err();
        assert!(
            conflict_code(&error).is_some() || matches!(error, HostResourceError::Io { .. }),
            "repository-root replacement must fail closed: {error:?}"
        );
        assert!(
            owner
                .coordinator
                .managed_storage_claim(&repository, &filesystem_id, &request.claim_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            std::fs::read(outside.join("payload")).unwrap(),
            b"must remain outside inventory"
        );
    }

    #[test]
    fn managed_storage_root_symlink_replacement_keeps_reservation_unsettled() {
        let mut fixture = Fixture::new();
        let root = fixture.materialization_root.clone();
        let external = fixture._temp.path().join("external-root");
        std::fs::create_dir(&external).unwrap();
        std::fs::write(external.join("payload"), b"outside").unwrap();
        let request = fixture.claim("root-symlink", 4096);
        let repository = fixture.repository.clone();
        let filesystem_id = fixture.filesystem_id.clone();
        let mut owner = fixture.owner().unwrap();
        let permit = expect_permit(owner.reserve(&request).unwrap());

        symlink(&external, &root).unwrap();
        assert!(owner.reconcile_new(permit).is_err());
        assert_eq!(std::fs::read(external.join("payload")).unwrap(), b"outside");
        let claim = owner
            .coordinator
            .managed_storage_claim(&repository, &filesystem_id, &request.claim_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            claim.state,
            crate::resources::ManagedStorageClaimState::Reserved
        );
        assert!(matches!(
            owner.reserve(&request).unwrap(),
            ManagedStorageAdmission::Existing(existing)
                if existing.state == crate::resources::ManagedStorageClaimState::Reserved
        ));
    }

    #[test]
    fn existing_untracked_root_is_not_adopted_by_lineage_backfill() {
        let mut fixture = Fixture::new();
        let request = fixture.claim("untracked-existing", 16 * 1024);
        std::fs::create_dir(&fixture.materialization_root).unwrap();
        std::fs::write(fixture.materialization_root.join("legacy"), b"legacy bytes").unwrap();
        let repository = fixture.repository.clone();
        let filesystem_id = fixture.filesystem_id.clone();
        let mut owner = fixture.owner().unwrap();

        let reserve_error = owner.reserve(&request).unwrap_err();
        assert_eq!(
            conflict_code(&reserve_error),
            Some("managed_storage_root_not_fresh")
        );
        let backfill_error = owner
            .backfill_current_charge(&request.claim_id)
            .unwrap_err();
        assert_eq!(
            conflict_code(&backfill_error),
            Some("managed_storage_claim_missing")
        );
        assert!(
            owner
                .coordinator
                .managed_storage_claim(&repository, &filesystem_id, &request.claim_id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn charged_root_mutation_backfills_then_swaps_charge_and_gc_releases_the_chain() {
        let mut fixture = Fixture::new();
        let first = fixture.claim("first-charge", 1_000_000);
        let second = fixture.claim("second-epoch", 64 * 1024);
        let root = fixture.materialization_root.clone();
        let repository = fixture.repository.clone();
        let filesystem_id = fixture.filesystem_id.clone();
        let root_identity = fixture.root_identity.clone();
        let mut owner = fixture.owner().unwrap();
        let first_permit = expect_permit(owner.reserve(&first).unwrap());
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("base"), vec![1_u8; 4_096]).unwrap();
        let first_charged = owner.reconcile_new(first_permit).unwrap();
        let first_bytes = first_charged.measured_bytes.unwrap();
        assert!(first_bytes > 0);

        // A migrated v1 charge has no root-generation record. Backfill only
        // after a complete scan matches the exact persisted byte count.
        owner
            .coordinator
            .conn
            .execute(
                "DELETE FROM managed_storage_claim_lineage WHERE repository=?1
                 AND filesystem_id=?2 AND claim_id=?3",
                params![repository, filesystem_id, first.claim_id],
            )
            .unwrap();
        owner.backfill_current_charge(&first.claim_id).unwrap();

        let second_permit = match owner.reserve_mutation(&second, &first.claim_id).unwrap() {
            ManagedStorageAdmission::Materialize(permit) => permit,
            ManagedStorageAdmission::Existing(claim) => {
                panic!("new mutation unexpectedly returned status: {claim:?}")
            }
        };
        std::fs::write(root.join("growth"), vec![2_u8; 8_192]).unwrap();
        let second_charged = owner.reconcile_new(second_permit).unwrap();
        let second_bytes = second_charged.measured_bytes.unwrap();
        assert!(second_bytes > first_bytes);
        let first_lineage = load_managed_storage_lineage(
            &owner.coordinator.conn,
            &repository,
            &filesystem_id,
            &first.claim_id,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            first_lineage.superseded_by_claim_id.as_deref(),
            Some(second.claim_id.as_str())
        );

        let cleanup = owner
            .coordinator
            .mark_managed_storage_cleanup_pending(&second)
            .unwrap();
        assert_eq!(
            cleanup.state,
            crate::resources::ManagedStorageClaimState::CleanupPending
        );
        let receipt = VerifiedManagedStorageRemoval::after_complete_no_follow_removal(
            repository.clone(),
            filesystem_id.clone(),
            second.claim_id.clone(),
            root_identity,
            "complete-chain-removal".into(),
            0,
            true,
        )
        .unwrap();
        owner
            .coordinator
            .release_managed_storage_claim(&receipt)
            .unwrap();
        for claim_id in [&first.claim_id, &second.claim_id] {
            let claim = owner
                .coordinator
                .managed_storage_claim(&repository, &filesystem_id, claim_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                claim.state,
                crate::resources::ManagedStorageClaimState::Released
            );
        }
    }

    #[test]
    fn mutation_requires_exact_baseline_and_does_not_double_count_superseded_charge() {
        let mut fixture = Fixture::new();
        let first = fixture.claim("mutation-first", 1_000_000);
        let second = fixture.claim("mutation-second", 64 * 1024);
        let third = fixture.claim("mutation-third", 1);
        let root = fixture.materialization_root.clone();
        let repository = fixture.repository.clone();
        let filesystem_id = fixture.filesystem_id.clone();
        let mut owner = fixture.owner().unwrap();
        let first_permit = expect_permit(owner.reserve(&first).unwrap());
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("base"), vec![3_u8; 4_096]).unwrap();
        let first_charged = owner.reconcile_new(first_permit).unwrap();

        std::fs::write(root.join("external-growth"), vec![4_u8; 4_096]).unwrap();
        let error = owner
            .reserve_mutation(&second, &first.claim_id)
            .unwrap_err();
        assert_eq!(
            conflict_code(&error),
            Some("managed_storage_mutation_baseline_changed")
        );
        assert!(
            owner
                .coordinator
                .managed_storage_claim(&repository, &filesystem_id, &second.claim_id)
                .unwrap()
                .is_none()
        );
        std::fs::remove_file(root.join("external-growth")).unwrap();

        let second_permit = match owner.reserve_mutation(&second, &first.claim_id).unwrap() {
            ManagedStorageAdmission::Materialize(permit) => permit,
            ManagedStorageAdmission::Existing(claim) => {
                panic!("new mutation unexpectedly returned status: {claim:?}")
            }
        };
        std::fs::write(root.join("growth"), vec![5_u8; 8_192]).unwrap();
        let second_charged = owner.reconcile_new(second_permit).unwrap();
        let second_bytes = second_charged.measured_bytes.unwrap();
        assert!(second_bytes > first_charged.measured_bytes.unwrap());

        // If the superseded first charge were counted again, this exact cap
        // would reject a one-byte next epoch. The current root alone fits.
        owner
            .coordinator
            .set_managed_storage_policy(&ManagedStoragePolicy {
                repository: repository.clone(),
                filesystem_id: filesystem_id.clone(),
                managed_cap_bytes: second_bytes + 1,
                free_reserve_bytes: 0,
            })
            .unwrap();
        let self_predecessor = owner.reserve_mutation(&third, &third.claim_id).unwrap_err();
        assert_eq!(
            conflict_code(&self_predecessor),
            Some("managed_storage_self_predecessor")
        );
        assert!(matches!(
            owner.reserve_mutation(&third, &second.claim_id).unwrap(),
            ManagedStorageAdmission::Materialize(_)
        ));
    }

    #[test]
    fn restarted_mutation_reconciles_pending_claim_without_replaying_write() {
        let mut fixture = Fixture::new();
        let first = fixture.claim("crash-first", 1_000_000);
        let second = fixture.claim("crash-pending", 64 * 1024);
        let root = fixture.materialization_root.clone();
        let repository = fixture.repository.clone();
        let filesystem_id = fixture.filesystem_id.clone();
        {
            let mut owner = fixture.owner().unwrap();
            let first_permit = expect_permit(owner.reserve(&first).unwrap());
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("base"), vec![6_u8; 4_096]).unwrap();
            owner.reconcile_new(first_permit).unwrap();
            let pending = owner.reserve_mutation(&second, &first.claim_id).unwrap();
            assert!(matches!(pending, ManagedStorageAdmission::Materialize(_)));
            std::fs::write(root.join("partial-growth"), vec![7_u8; 2_048]).unwrap();
        }

        drop(fixture.coordinator);
        fixture.coordinator = HostResourceCoordinator::open(&fixture.database).unwrap();
        let mut owner = fixture.owner().unwrap();
        assert!(matches!(
            owner.reserve_mutation(&second, &first.claim_id).unwrap(),
            ManagedStorageAdmission::Existing(claim)
                if claim.state == crate::resources::ManagedStorageClaimState::Reserved
        ));
        let recovered = owner.reconcile_existing(&second).unwrap();
        assert_eq!(
            recovered.state,
            crate::resources::ManagedStorageClaimState::Charged
        );
        let predecessor_lineage = load_managed_storage_lineage(
            &owner.coordinator.conn,
            &repository,
            &filesystem_id,
            &first.claim_id,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            predecessor_lineage.superseded_by_claim_id.as_deref(),
            Some(second.claim_id.as_str())
        );
    }

    #[test]
    fn replacement_root_instance_refuses_mutation_even_when_bytes_match() {
        let mut fixture = Fixture::new();
        let first = fixture.claim("replace-first", 1_000_000);
        let second = fixture.claim("replace-second", 64 * 1024);
        let root = fixture.materialization_root.clone();
        let held_old = fixture._temp.path().join("held-old-root");
        let repository = fixture.repository.clone();
        let filesystem_id = fixture.filesystem_id.clone();
        let mut owner = fixture.owner().unwrap();
        let first_permit = expect_permit(owner.reserve(&first).unwrap());
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("same"), vec![8_u8; 4_096]).unwrap();
        owner.reconcile_new(first_permit).unwrap();

        std::fs::rename(&root, &held_old).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("same"), vec![8_u8; 4_096]).unwrap();
        let error = owner
            .reserve_mutation(&second, &first.claim_id)
            .unwrap_err();
        assert_eq!(
            conflict_code(&error),
            Some("managed_storage_mutation_baseline_changed")
        );
        assert!(
            owner
                .coordinator
                .managed_storage_claim(&repository, &filesystem_id, &second.claim_id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn managed_storage_retry_after_reopen_is_status_only_for_same_root() {
        let mut fixture = Fixture::new();
        let request = fixture.claim("restart-retry", 4096);
        {
            let mut owner = fixture.owner().unwrap();
            let _permit = expect_permit(owner.reserve(&request).unwrap());
        }

        drop(fixture.coordinator);
        fixture.coordinator = HostResourceCoordinator::open(&fixture.database).unwrap();
        let mut owner = fixture.owner().unwrap();
        match owner.reserve(&request).unwrap() {
            ManagedStorageAdmission::Existing(claim) => {
                assert_eq!(
                    claim.state,
                    crate::resources::ManagedStorageClaimState::Reserved
                );
                assert_eq!(claim.request, request);
            }
            ManagedStorageAdmission::Materialize(_) => {
                panic!("reopened exact claim granted another materialization")
            }
        }
    }

    #[test]
    fn forged_owner_generation_cannot_settle_a_claim_or_grant_a_retry() {
        let mut fixture = Fixture::new();
        let request = fixture.claim("forged-generation", 4096);
        let repository = fixture.repository.clone();
        let filesystem_id = fixture.filesystem_id.clone();
        let mut owner = fixture.owner().unwrap();
        let mut permit = expect_permit(owner.reserve(&request).unwrap());
        permit.generation = permit.generation.saturating_add(1);

        let error = owner.reconcile_new(permit).unwrap_err();
        assert_eq!(
            conflict_code(&error),
            Some("managed_storage_permit_owner_mismatch")
        );
        let claim = owner
            .coordinator
            .managed_storage_claim(&repository, &filesystem_id, &request.claim_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            claim.state,
            crate::resources::ManagedStorageClaimState::Reserved
        );
        assert!(matches!(
            owner.reserve(&request).unwrap(),
            ManagedStorageAdmission::Existing(existing)
                if existing.state == crate::resources::ManagedStorageClaimState::Reserved
        ));
    }

    #[test]
    fn failed_inventory_stays_reserved_and_uncertain_claims_block_new_ids() {
        let mut fixture = Fixture::new();
        let request = fixture.claim("failed-inventory", 4096);
        let next_request = fixture.claim("new-after-failed-scan", 4096);
        let repository = fixture.repository.clone();
        let filesystem_id = fixture.filesystem_id.clone();
        let root = fixture.materialization_root.clone();
        let external = fixture._temp.path().join("failed-inventory-external");
        let mut owner = fixture.owner().unwrap();
        let permit = expect_permit(owner.reserve(&request).unwrap());

        std::fs::create_dir(&external).unwrap();
        symlink(&external, &root).unwrap();
        assert!(owner.reconcile_new(permit).is_err());
        let reserved = owner
            .coordinator
            .managed_storage_claim(&repository, &filesystem_id, &request.claim_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            reserved.state,
            crate::resources::ManagedStorageClaimState::Reserved
        );
        assert!(matches!(
            owner.reserve(&request).unwrap(),
            ManagedStorageAdmission::Existing(existing)
                if existing.state == crate::resources::ManagedStorageClaimState::Reserved
        ));

        std::fs::remove_file(&root).unwrap();
        let error = owner.reserve(&next_request).unwrap_err();
        assert_eq!(
            conflict_code(&error),
            Some("managed_storage_materialization_pending")
        );

        let mut uncertain_fixture = Fixture::new();
        let uncertain_request = uncertain_fixture.claim("uncertain-inventory", 4096);
        let blocked_request = uncertain_fixture.claim("blocked-after-uncertain", 4096);
        let mut uncertain_owner = uncertain_fixture.owner().unwrap();
        let permit = expect_permit(uncertain_owner.reserve(&uncertain_request).unwrap());
        let uncertain = uncertain_owner.mark_uncertain(permit).unwrap();
        assert_eq!(
            uncertain.state,
            crate::resources::ManagedStorageClaimState::Uncertain
        );
        assert!(matches!(
            uncertain_owner.reserve(&uncertain_request).unwrap(),
            ManagedStorageAdmission::Existing(existing)
                if existing.state == crate::resources::ManagedStorageClaimState::Uncertain
        ));
        let error = uncertain_owner.reserve(&blocked_request).unwrap_err();
        assert_eq!(
            conflict_code(&error),
            Some("managed_storage_inventory_uncertain")
        );
    }

    #[test]
    fn managed_storage_inventory_does_not_follow_child_symlinks() {
        let mut fixture = Fixture::new();
        let request = fixture.claim("child-symlink", 4096);
        let root = fixture.materialization_root.clone();
        let external = fixture._temp.path().join("external-child");
        std::fs::create_dir(&external).unwrap();
        std::fs::write(external.join("payload"), vec![7_u8; 8192]).unwrap();
        let mut owner = fixture.owner().unwrap();
        let permit = expect_permit(owner.reserve(&request).unwrap());
        std::fs::create_dir(&root).unwrap();
        symlink(&external, root.join("link-to-external")).unwrap();

        let inventory = scan_complete(&owner.root, &owner.grant, &request).unwrap();
        assert_eq!(inventory.entry_count, 2, "root directory plus symlink only");
        let claim = owner.reconcile_new(permit).unwrap();
        assert_eq!(
            claim.state,
            crate::resources::ManagedStorageClaimState::Charged
        );
        assert_eq!(claim.measured_bytes, Some(inventory.allocated_bytes));
    }

    #[test]
    fn scanner_refuses_entry_depth_and_deadline_limits() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let fd = open_absolute_directory_nofollow(&root).unwrap();
        let stat = fstat(fd.as_raw_fd(), &root).unwrap();

        let mut entry_limited =
            Scanner::new(stat.st_dev as u64, Instant::now() + SCAN_MAX_DURATION);
        entry_limited.entry_count = SCAN_MAX_ENTRIES;
        let error = entry_limited
            .count_stat(&stat, b"one-too-many")
            .unwrap_err();
        assert_eq!(
            conflict_code(&error),
            Some("managed_storage_inventory_incomplete")
        );

        let mut depth_limited =
            Scanner::new(stat.st_dev as u64, Instant::now() + SCAN_MAX_DURATION);
        let error = depth_limited
            .visit_directory(-1, Path::new("unopened"), SCAN_MAX_DEPTH + 1, None)
            .unwrap_err();
        assert_eq!(
            conflict_code(&error),
            Some("managed_storage_inventory_incomplete")
        );

        let deadline = Instant::now() - Duration::from_secs(1);
        let expired = Scanner::new(stat.st_dev as u64, deadline);
        let error = expired.check_deadline(Path::new("expired")).unwrap_err();
        assert_eq!(
            conflict_code(&error),
            Some("managed_storage_inventory_incomplete")
        );
    }

    #[test]
    fn scanner_rejects_directory_and_regular_file_replacement_between_stat_and_open() {
        let temp = tempfile::tempdir().unwrap();
        let parent_path = std::fs::canonicalize(temp.path()).unwrap();
        let parent_fd = open_absolute_directory_nofollow(&parent_path).unwrap();
        let directory_path = parent_path.join("directory");
        std::fs::create_dir(&directory_path).unwrap();
        let directory_name = CString::new("directory").unwrap();
        let expected_directory = stat_at(
            parent_fd.as_raw_fd(),
            &directory_name,
            libc::AT_SYMLINK_NOFOLLOW,
        )
        .unwrap();
        let renamed_directory = parent_path.join("renamed-directory");
        std::fs::rename(&directory_path, &renamed_directory).unwrap();
        std::fs::create_dir(&directory_path).unwrap();
        let replacement_directory =
            openat_directory(parent_fd.as_raw_fd(), &directory_name, &directory_path).unwrap();
        let opened_directory = fstat(replacement_directory.as_raw_fd(), &directory_path).unwrap();
        assert!(
            ensure_same_identity(
                &expected_directory,
                &opened_directory,
                "directory replacement"
            )
            .is_err()
        );

        let file_path = parent_path.join("regular");
        std::fs::write(&file_path, b"same bytes").unwrap();
        let file_name = CString::new("regular").unwrap();
        let expected_file =
            stat_at(parent_fd.as_raw_fd(), &file_name, libc::AT_SYMLINK_NOFOLLOW).unwrap();
        let renamed_file = parent_path.join("renamed-regular");
        std::fs::rename(&file_path, &renamed_file).unwrap();
        std::fs::write(&file_path, b"same bytes").unwrap();
        let replacement_file =
            openat_regular(parent_fd.as_raw_fd(), &file_name, &file_path).unwrap();
        let opened_file = fstat(replacement_file.as_raw_fd(), &file_path).unwrap();
        assert!(ensure_same_identity(&expected_file, &opened_file, "file replacement").is_err());
    }
}
