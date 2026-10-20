//! Bounded, no-follow inventory receipts for one finite broker-owned tree.
//!
//! This scanner reports OS-reported allocated blocks while the caller holds
//! the exact preparation-cache exclusive key. It is a bounded cooperative
//! accounting input, not a filesystem quota, exact physical-extent measure,
//! or constraint on non-broker writers.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[path = "managed_storage_inventory_core.rs"]
pub(super) mod core;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) use core::{InventoryScan, InventoryScanPoll, ScanLimits, TreeInventory};

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
#[allow(
    dead_code,
    reason = "admission payloads are consumed only by the disabled managed preparation producer"
)]
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

#[allow(
    dead_code,
    reason = "managed preparation consumer remains disabled until its ownership fences are qualified"
)]
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
        self.root.filesystem_id()
    }

    /// A path-derived opaque identity suitable for a ManagedStorageClaimRequest.
    /// The absolute path itself is never written to the host-resource ledger.
    pub(crate) fn root_identity(&self) -> &str {
        self.root.root_identity()
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
        if inventory.root_present || inventory.allocated_bytes != 0 {
            return Err(managed_storage_refusal(
                "managed_storage_root_not_fresh",
                "a new storage claim requires its exact materialization root to be absent; existing bytes need a separate backfill proof",
            ));
        }

        let free_space_root = self.root.parent().to_path_buf();
        match self
            .coordinator
            .reserve_managed_storage_with_probe(request, || {
                crate::disk_headroom::available_bytes(&free_space_root).ok_or_else(|| {
                    managed_storage_refusal(
                        "managed_storage_free_bytes_unknown",
                        "filesystem free space could not be measured; reservation is refused",
                    )
                })
            })? {
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
            .managed_storage_claim(&self.repository, self.root.filesystem_id(), claim_id)?
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
        let free_space_root = self.root.parent().to_path_buf();
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
        match self.coordinator.reserve_managed_storage_mutation_with_probe(
            request,
            || {
                crate::disk_headroom::available_bytes(&free_space_root).ok_or_else(|| {
                    managed_storage_refusal(
                        "managed_storage_free_bytes_unknown",
                        "filesystem free space could not be measured; mutation reservation is refused",
                    )
                })
            },
            baseline,
        )? {
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
            || request.materialization != self.root.materialization()
            || request.filesystem_id != self.root.filesystem_id()
            || request.root_identity != self.root.root_identity()
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

pub(super) use core::PinnedManagedStorageRoot as RootBinding;

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
fn scan_complete(
    root: &RootBinding,
    grant: &HostResourceGrant,
    request: &ManagedStorageClaimRequest,
) -> Result<CompleteManagedStorageInventory, HostResourceError> {
    let mut scan = InventoryScan::begin(root, ScanLimits::existing_owner())?;
    let tree = loop {
        match scan.poll(512)? {
            InventoryScanPoll::Pending => {}
            InventoryScanPoll::Complete(tree) => break tree,
        }
    };
    Ok(CompleteManagedStorageInventory {
        repository: request.repository.clone(),
        filesystem_id: root.filesystem_id().to_owned(),
        claim_id: request.claim_id.clone(),
        request_digest: super::managed_storage_claim_digest(request)?,
        root_identity: root.root_identity().to_owned(),
        lease_id: grant.lease.lease_id.clone(),
        generation: grant.lease.generation,
        allocated_bytes: tree.allocated_bytes(),
        root_instance_id: tree.root_instance_id().map(str::to_owned),
        entry_count: tree.entry_count(),
        inventory_digest: tree.inventory_digest().to_owned(),
        root_present: tree.root_present(),
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

// libc::dev_t is u64 on Linux and i32 on macOS. Normalize at one boundary.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[allow(
    clippy::unnecessary_cast,
    reason = "libc::dev_t differs across supported operating systems"
)]
fn device_id(stat: &libc::stat) -> u64 {
    stat.st_dev as u64
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
        VerifiedManagedStorageRemoval, load_managed_storage_lineage, managed_storage_i64,
    };
    use rusqlite::params;
    use sha2::{Digest, Sha256};
    use std::collections::HashSet;
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
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
            let filesystem_id = binding.filesystem_id().to_owned();
            let root_identity = binding.root_identity().to_owned();
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

    fn run_inventory_scan(
        root: &RootBinding,
        limits: ScanLimits,
        work_budget: usize,
    ) -> Result<TreeInventory, HostResourceError> {
        let mut scan = InventoryScan::begin(root, limits)?;
        loop {
            match scan.poll(work_budget)? {
                InventoryScanPoll::Pending => {}
                InventoryScanPoll::Complete(inventory) => return Ok(inventory),
            }
        }
    }

    fn expected_fixed_tree_inventory(
        root: &RootBinding,
    ) -> (u64, Option<String>, usize, String, bool) {
        let parent_fd = open_absolute_directory_nofollow(root.parent()).unwrap();
        let root_name = cstring_from_os(root.leaf()).unwrap();
        let root_path = root.parent().join(root.leaf());
        let root_fd = openat_directory(parent_fd.as_raw_fd(), &root_name, &root_path).unwrap();
        let root_stat = fstat(root_fd.as_raw_fd(), &root_path).unwrap();

        let nested_name = CString::new("nested").unwrap();
        let nested_path = root_path.join("nested");
        let nested_fd = openat_directory(root_fd.as_raw_fd(), &nested_name, &nested_path).unwrap();
        let nested_stat = fstat(nested_fd.as_raw_fd(), &nested_path).unwrap();
        let a_file_name = CString::new("a-file").unwrap();
        let a_file_stat = stat_at(
            nested_fd.as_raw_fd(),
            &a_file_name,
            libc::AT_SYMLINK_NOFOLLOW,
        )
        .unwrap();
        let z_file_name = CString::new("z-file").unwrap();
        let z_file_stat =
            stat_at(root_fd.as_raw_fd(), &z_file_name, libc::AT_SYMLINK_NOFOLLOW).unwrap();

        // This is the fixed bytewise depth-first receipt order for the tiny
        // fixture: root directory, nested directory, nested file, root file.
        let mut entry_hash = Sha256::new();
        let mut seen_inodes = HashSet::new();
        let mut allocated_bytes = 0_u64;
        let mut entry_count = 0_usize;
        for (stat, name) in [
            (&root_stat, &b"."[..]),
            (&nested_stat, &b"."[..]),
            (&a_file_stat, &b"a-file"[..]),
            (&z_file_stat, &b"z-file"[..]),
        ] {
            if seen_inodes.insert((device_id(stat), stat.st_ino)) {
                let blocks = u64::try_from(stat.st_blocks).unwrap();
                allocated_bytes += blocks * 512;
            }
            entry_count += 1;
            entry_hash.update((name.len() as u64).to_le_bytes());
            entry_hash.update(name);
            entry_hash.update(device_id(stat).to_le_bytes());
            entry_hash.update(stat.st_ino.to_le_bytes());
            entry_hash.update((stat.st_mode as u64).to_le_bytes());
            entry_hash.update(stat.st_size.to_le_bytes());
            entry_hash.update(stat.st_blocks.to_le_bytes());
        }
        let root_instance_id = Some(format!(
            "unix-inode-v1:{}:{}",
            root_stat.st_dev, root_stat.st_ino
        ));
        let mut receipt_hash = entry_hash;
        receipt_hash.update(root.parent().as_os_str().as_bytes());
        receipt_hash.update(root.leaf().as_bytes());
        receipt_hash.update(allocated_bytes.to_le_bytes());
        receipt_hash.update((entry_count as u64).to_le_bytes());
        receipt_hash.update(root_instance_id.as_ref().unwrap().as_bytes());
        (
            allocated_bytes,
            root_instance_id,
            entry_count,
            format!("{:x}", receipt_hash.finalize()),
            true,
        )
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
        let second_lineage = load_managed_storage_lineage(
            &owner.coordinator.conn,
            &repository,
            &filesystem_id,
            &second.claim_id,
        )
        .unwrap()
        .unwrap();
        let root_instance_id = second_lineage
            .root_instance_id
            .clone()
            .expect("present root should have a pinned instance");
        let second_baseline = second_lineage.baseline_bytes;

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
            Some(root_instance_id.clone()),
            "complete-chain-removal".into(),
            0,
            true,
        )
        .unwrap();
        owner
            .coordinator
            .conn
            .execute(
                "UPDATE managed_storage_claim_lineage SET baseline_bytes=?4
                 WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
                params![
                    repository,
                    filesystem_id,
                    second.claim_id,
                    managed_storage_i64("baseline_bytes", second_baseline + 1).unwrap()
                ],
            )
            .unwrap();
        let inconsistent = owner
            .coordinator
            .release_managed_storage_claim(&receipt)
            .unwrap_err();
        assert_eq!(
            conflict_code(&inconsistent),
            Some("managed_storage_lineage_invalid")
        );
        for (claim_id, expected_state) in [
            (
                &first.claim_id,
                crate::resources::ManagedStorageClaimState::Charged,
            ),
            (
                &second.claim_id,
                crate::resources::ManagedStorageClaimState::CleanupPending,
            ),
        ] {
            let unchanged = owner
                .coordinator
                .managed_storage_claim(&repository, &filesystem_id, claim_id)
                .unwrap()
                .unwrap();
            assert_eq!(unchanged.state, expected_state);
        }
        owner
            .coordinator
            .conn
            .execute(
                "UPDATE managed_storage_claim_lineage SET baseline_bytes=?4
                 WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
                params![repository, filesystem_id, second.claim_id, second_baseline],
            )
            .unwrap();
        owner
            .coordinator
            .conn
            .execute(
                "UPDATE managed_storage_claim_lineage SET root_instance_id=?4
                 WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
                params![
                    repository,
                    filesystem_id,
                    first.claim_id,
                    "mismatched-instance"
                ],
            )
            .unwrap();
        let inconsistent_instance = owner
            .coordinator
            .release_managed_storage_claim(&receipt)
            .unwrap_err();
        assert_eq!(
            conflict_code(&inconsistent_instance),
            Some("managed_storage_lineage_invalid")
        );
        for (claim_id, expected_state) in [
            (
                &first.claim_id,
                crate::resources::ManagedStorageClaimState::Charged,
            ),
            (
                &second.claim_id,
                crate::resources::ManagedStorageClaimState::CleanupPending,
            ),
        ] {
            let unchanged = owner
                .coordinator
                .managed_storage_claim(&repository, &filesystem_id, claim_id)
                .unwrap()
                .unwrap();
            assert_eq!(unchanged.state, expected_state);
        }
        owner
            .coordinator
            .conn
            .execute(
                "UPDATE managed_storage_claim_lineage SET root_instance_id=?4
                 WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
                params![repository, filesystem_id, first.claim_id, root_instance_id],
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
        let first_charged = owner.reconcile_new(first_permit).unwrap();
        let first_measured_bytes = first_charged.measured_bytes.unwrap();
        let original_instance_id = load_managed_storage_lineage(
            &owner.coordinator.conn,
            &repository,
            &filesystem_id,
            &first.claim_id,
        )
        .unwrap()
        .unwrap()
        .root_instance_id
        .unwrap();

        std::fs::rename(&root, &held_old).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("same"), vec![8_u8; 4_096]).unwrap();
        let replacement_inventory = scan_complete(&owner.root, &owner.grant, &first).unwrap();
        let replacement_instance_id = replacement_inventory.root_instance_id.unwrap();
        assert_ne!(replacement_instance_id, original_instance_id);
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

        let cleanup = owner
            .coordinator
            .mark_managed_storage_cleanup_pending(&first)
            .unwrap();
        assert_eq!(
            cleanup.state,
            crate::resources::ManagedStorageClaimState::CleanupPending
        );
        std::fs::remove_file(root.join("same")).unwrap();
        std::fs::remove_dir(&root).unwrap();
        let replacement_receipt = VerifiedManagedStorageRemoval::after_complete_no_follow_removal(
            repository.clone(),
            filesystem_id.clone(),
            first.claim_id.clone(),
            first.root_identity.clone(),
            Some(replacement_instance_id),
            "removed-replacement-root".into(),
            0,
            true,
        )
        .unwrap();
        let release_error = owner
            .coordinator
            .release_managed_storage_claim(&replacement_receipt)
            .unwrap_err();
        assert_eq!(
            conflict_code(&release_error),
            Some("managed_storage_root_instance_mismatch")
        );
        let retained = owner
            .coordinator
            .managed_storage_claim(&repository, &filesystem_id, &first.claim_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            retained.state,
            crate::resources::ManagedStorageClaimState::CleanupPending
        );
        assert_eq!(retained.measured_bytes, Some(first_measured_bytes));

        std::fs::remove_file(held_old.join("same")).unwrap();
        std::fs::remove_dir(&held_old).unwrap();
        let original_receipt = VerifiedManagedStorageRemoval::after_complete_no_follow_removal(
            repository.clone(),
            filesystem_id.clone(),
            first.claim_id.clone(),
            first.root_identity.clone(),
            Some(original_instance_id),
            "removed-original-root".into(),
            0,
            true,
        )
        .unwrap();
        assert_eq!(
            owner
                .coordinator
                .release_managed_storage_claim(&original_receipt)
                .unwrap()
                .state,
            crate::resources::ManagedStorageClaimState::Released
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
        let cache_root = temp.path().join("preparation-cache");
        let repository_root = cache_root.join(REPOSITORY);
        std::fs::create_dir_all(&repository_root).unwrap();
        let binding = RootBinding::new(&cache_root, REPOSITORY, MATERIALIZATION).unwrap();
        let materialization_root = repository_root.join(MATERIALIZATION);
        std::fs::create_dir_all(materialization_root.join("nested")).unwrap();
        std::fs::write(materialization_root.join("payload"), b"bounded").unwrap();

        let entry_limits = ScanLimits::new(1, SCAN_MAX_DEPTH, SCAN_MAX_DURATION).unwrap();
        let error = run_inventory_scan(&binding, entry_limits, 2).unwrap_err();
        assert_eq!(
            conflict_code(&error),
            Some("managed_storage_inventory_incomplete")
        );

        let depth_limits = ScanLimits::new(SCAN_MAX_ENTRIES, 0, SCAN_MAX_DURATION).unwrap();
        let mut depth_scan = InventoryScan::begin(&binding, depth_limits).unwrap();
        let depth_error = loop {
            match depth_scan.poll(1) {
                Ok(InventoryScanPoll::Pending) => {}
                Ok(InventoryScanPoll::Complete(_)) => {
                    panic!("a nested directory exceeded the configured depth")
                }
                Err(error) => break error,
            }
        };
        assert_eq!(
            conflict_code(&depth_error),
            Some("managed_storage_inventory_incomplete")
        );
        let retry_error = depth_scan.poll(8).unwrap_err();
        assert_eq!(
            conflict_code(&retry_error),
            Some("managed_storage_inventory_incomplete")
        );

        let deadline_limits =
            ScanLimits::new(SCAN_MAX_ENTRIES, SCAN_MAX_DEPTH, Duration::from_nanos(1)).unwrap();
        let error = run_inventory_scan(&binding, deadline_limits, 1).unwrap_err();
        assert_eq!(
            conflict_code(&error),
            Some("managed_storage_inventory_incomplete")
        );
    }

    #[test]
    fn incremental_inventory_poll_budgets_preserve_the_complete_receipt() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.materialization_root.join("nested")).unwrap();
        std::fs::write(fixture.materialization_root.join("z-file"), b"z").unwrap();
        std::fs::write(
            fixture.materialization_root.join("nested").join("a-file"),
            b"nested",
        )
        .unwrap();
        let binding =
            RootBinding::new(&fixture.preparation_cache_root, REPOSITORY, MATERIALIZATION).unwrap();
        let limits = ScanLimits::existing_owner();
        let mut zero_budget_scan = InventoryScan::begin(&binding, limits).unwrap();
        let zero_budget_error = zero_budget_scan.poll(0).unwrap_err();
        assert_eq!(
            conflict_code(&zero_budget_error),
            Some("managed_storage_inventory_incomplete")
        );

        let one_entry_at_a_time = run_inventory_scan(&binding, limits, 1).unwrap();
        let larger_polls = run_inventory_scan(&binding, limits, 32).unwrap();
        let expected = expected_fixed_tree_inventory(&binding);

        for actual in [&one_entry_at_a_time, &larger_polls] {
            assert_eq!(actual.allocated_bytes(), expected.0);
            assert_eq!(actual.root_instance_id(), expected.1.as_deref());
            assert_eq!(actual.entry_count(), expected.2);
            assert_eq!(actual.inventory_digest(), expected.3.as_str());
            assert_eq!(actual.root_present(), expected.4);
        }
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
