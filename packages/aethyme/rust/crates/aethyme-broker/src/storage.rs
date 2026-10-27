//! Host-scoped worktree storage inventory and reviewed reclamation.
//!
//! Worktree directories live below a host-level container, while the Git
//! registrations and broker session ledger that explain them live in each
//! repository. Repository-local cleanup cannot see a deleted repository's
//! root, and it cannot distinguish a stray directory from a linked worktree
//! belonging to another clone. This module joins those three observations at
//! the host boundary.
//!
//! The inventory does not mutate broker ownership state. It may spend the
//! configured routine measurement budget refreshing the shared size cache.
//! Reclamation is a separate, digest-confirmed operation and rechecks the
//! ownership evidence immediately before every removal. An unreadable or
//! unmarked root is reported, but is never treated as disposable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::broker::{WORKTREE_ROOT_MARKER, WORKTREE_ROOT_SCHEMA_VERSION, WorktreeRootMarker};
use crate::gc::{PinnedTreeRemoval, TreeRemoval, remove_condemned_tree_with_progress_and_guard};
use crate::reclaim::is_artefact_directory_with_extras;
use crate::{BrokerStore, GitRepo};

pub const STORAGE_PLAN_SCHEMA_VERSION: u32 = 3;
pub const STORAGE_RECONCILIATION_SCHEMA_VERSION: u32 = 1;

const DAY_MS: i64 = 86_400_000;
const STORAGE_ROOT_MAX_ENTRIES: usize = 4096;
const WORKTREE_ROOT_MARKER_MAX_BYTES: u64 = 64 * 1024;
const WORKTREE_CACHE_ROOT_DISCOVERY_BUDGET: Duration = Duration::from_secs(5);
#[cfg(test)]
const PREPARATION_SWEEP_MAX_DISCOVERY_PATHS: usize = 4096;
#[cfg(test)]
const PREPARATION_SWEEP_MAX_MARKER_BYTES: u64 = 64 * 1024;
const PREPARATION_RETIRED_STORE_MAX_KEYS: usize = 4096;
const PREPARATION_ROTATION_DEBT_MAX_ENTRIES: usize = 100_000;
const PREPARATION_LEASE_TTL_MARGIN_SECONDS: u64 = 30;
const PREPARATION_COORDINATOR_OPEN_MAX_WAIT: Duration = Duration::from_millis(25);
const PREPARATION_APPLY_DEADLINE: Duration = Duration::from_secs(60 * 60);
const HOST_RESOURCE_MAX_TTL_SECONDS: u64 = 86_400;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error(transparent)]
    Git(#[from] crate::GitError),
    #[error("cannot inspect host worktree storage at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot inspect the broker ledger for {repository_root}: {source}")]
    Store {
        repository_root: PathBuf,
        #[source]
        source: crate::BrokerError,
    },
    #[error("host worktree storage is unavailable: {reason}")]
    Unavailable { reason: String },
    #[error("storage confirmation must be a full SHA-256 digest")]
    ConfirmationNotSha256,
    #[error(
        "the reviewed host storage plan no longer matches current state; nothing was removed; review a new plan with `aethyme broker gc storage plan` and confirm its digest"
    )]
    ConfirmationMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageFilesystemKind {
    Directory,
    Symlink,
    File,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageMarkerStatus {
    Valid,
    Missing,
    Invalid,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageDirectoryKind {
    Worktree,
    StrayDirectory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageSource {
    Disk,
    GitRegistration,
    SessionLedger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageCandidateKind {
    OrphanRoot,
    StrayDirectory,
    PrimaryArtifact,
    PreparationEntry,
    EmptyRoot,
}

impl StorageCandidateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OrphanRoot => "orphan_root",
            Self::StrayDirectory => "stray_directory",
            Self::PrimaryArtifact => "primary_artifact",
            Self::PreparationEntry => "preparation_entry",
            Self::EmptyRoot => "empty_root",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageEntry {
    /// Normalised absolute path of the direct child being reconciled.
    pub path: PathBuf,
    pub on_disk: bool,
    pub git_registered: bool,
    pub ledger_claimed: bool,
    pub missing_from: Vec<StorageSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<StorageDirectoryKind>,
    pub git_marker: bool,
    pub session_ids: Vec<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_inodes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageReconciliation {
    pub schema_version: u32,
    pub on_disk_count: usize,
    pub git_registered_count: usize,
    pub ledger_claimed_count: usize,
    /// Worktrees the session ledger records as removed on purpose: every
    /// session naming the path finished its cleanup, and neither the disk nor
    /// Git still has it. That is the correct end state of a finished session,
    /// not drift, so these paths are counted here and left out of `entries`
    /// and `ledger_claimed_count`.
    #[serde(default)]
    pub retired_count: usize,
    /// The union of the three sets, less retired worktrees. `missing_from` is
    /// empty only when all three sources agree about the path.
    pub entries: Vec<StorageEntry>,
}

/// A local branch ref that is either named by the broker session ledger or
/// uses the broker's `agent/` namespace without a matching ledger record.
/// This is an inventory only: it never authorizes branch deletion.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageBranchEntry {
    /// Branch name without the `refs/heads/` prefix.
    pub name: String,
    pub head: String,
    /// Every session record that names this branch, including cleaned rows.
    pub session_ids: Vec<i64>,
    /// Session rows that have not completed cleanup and therefore still hold
    /// the branch in the broker's ownership ledger.
    pub uncleared_session_ids: Vec<i64>,
    /// Registered Git worktrees that currently check out this branch.
    pub checked_out_paths: Vec<PathBuf>,
    pub ownership: StorageBranchOwnership,
    /// Why this inventory keeps the ref visible and what must be reviewed
    /// before a separate cleanup workflow could remove it.
    pub retention_reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageBranchOwnership {
    SessionLedger,
    Unknown,
    /// The repository's integration or default branch. No session owns it,
    /// even when an old ledger row (an adopted main checkout) names it.
    Shared,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageRoot {
    /// Normalised absolute path of one direct entry beneath the host storage
    /// container. Non-directory entries are retained in the inventory as
    /// blockers so the host scan does not silently hide unexpected state.
    pub path: PathBuf,
    pub filesystem_kind: StorageFilesystemKind,
    pub repository_key: Option<String>,
    pub repository_root: Option<PathBuf>,
    pub marker_status: StorageMarkerStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker_sha256: Option<String>,
    /// `None` means the marker was absent or unreadable; otherwise this is a
    /// direct observation of whether the marker's repository checkout exists.
    pub owner_exists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_days: Option<u32>,
    /// Count of on-disk direct child directories. A child may still be a
    /// stray directory; the reconciliation gives the exact classification.
    pub worktree_count: usize,
    pub on_disk_directory_count: usize,
    pub git_registered_count: usize,
    pub ledger_claimed_count: usize,
    /// See [`StorageReconciliation::retired_count`].
    #[serde(default)]
    pub retired_count: usize,
    /// True when the root holds nothing but the broker's own bookkeeping --
    /// its ownership marker, the build defaults it writes, reclaim review
    /// snapshots -- and no worktree, stray directory or other file.
    #[serde(default)]
    pub empty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_inodes: Option<u64>,
    pub sized: bool,
    pub reconciliation: StorageReconciliation,
    /// Whether the owner checkout's local refs were read successfully. False
    /// means an absent/unreadable owner or failed `for-each-ref`; it is never
    /// interpreted as an empty branch inventory.
    #[serde(default)]
    pub session_branch_inventory_complete: bool,
    /// Session-owned refs plus unclaimed `agent/` refs. An unclaimed ref is
    /// reported as unknown ownership and retained for review.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_branches: Vec<StorageBranchEntry>,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageCandidate {
    pub kind: StorageCandidateKind,
    /// The path that apply is authorized to remove.
    pub path: PathBuf,
    /// The containing repository root under the host container. For an
    /// orphan this equals `path`; for a stray it is its owning host root.
    pub root_path: PathBuf,
    pub repository_key: String,
    pub repository_root: PathBuf,
    /// Whether the reviewed stray directory carried a `.git` entry. This is
    /// part of the decision witness: a new Git marker may be an interrupted
    /// worktree, so apply must require a fresh review even when the path and
    /// broker claims are unchanged.
    pub git_marker: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_inodes: Option<u64>,
    pub reason: String,
    /// Exact ownership-marker bytes are the root witness at apply time.
    pub marker_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageSummary {
    pub root_count: usize,
    pub on_disk_directory_count: usize,
    pub owner_present_count: usize,
    pub owner_missing_count: usize,
    pub orphan_root_count: usize,
    pub stray_directory_count: usize,
    #[serde(default)]
    pub empty_root_count: usize,
    pub candidate_count: usize,
    pub primary_checkout_count: usize,
    pub primary_artifact_count: usize,
    pub primary_candidate_count: usize,
    #[serde(default)]
    pub preparation_entry_count: usize,
    #[serde(default)]
    pub preparation_candidate_count: usize,
    #[serde(default)]
    pub broker_branch_ref_count: usize,
    #[serde(default)]
    pub unclaimed_agent_branch_ref_count: usize,
    #[serde(default)]
    pub incomplete_branch_inventory_root_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation_reclaimable_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation_reclaimable_inodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_inodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reclaimable_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reclaimable_inodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_reclaimable_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_reclaimable_inodes: Option<u64>,
    pub sized: bool,
}

/// One entry in the shared preparation cache.
///
/// The cache lives beside the host resource database, independently of where
/// this repository places its worktrees.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoragePreparationEntry {
    pub path: PathBuf,
    pub repository: String,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_inodes: Option<u64>,
    pub reclaimable: bool,
    pub reason: String,
}

/// One preparation-cache entry the plan offers to remove.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoragePreparationCandidate {
    pub path: PathBuf,
    pub repository: String,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_inodes: Option<u64>,
    pub reason: String,
}

/// One enrolled primary checkout included in the host storage inventory.
///
/// A dirty checkout is reported as a whole-checkout blocker. Its recognized
/// artifacts remain visible for accounting, but none of them enters the
/// reviewed deletion candidate set.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoragePrimaryCheckout {
    pub path: PathBuf,
    pub repository_key: String,
    pub clean: bool,
    pub dirty_paths: Vec<String>,
    pub artifacts: Vec<StoragePrimaryArtifact>,
    pub blockers: Vec<String>,
}

/// One recognized regenerable directory beneath an enrolled primary checkout.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoragePrimaryArtifact {
    pub path: PathBuf,
    pub name: String,
    pub ignored: bool,
    pub tracked: bool,
    pub reclaimable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_inodes: Option<u64>,
    pub reason: String,
}

/// A primary-checkout artifact authorized by the reviewed storage plan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoragePrimaryCandidate {
    pub path: PathBuf,
    pub checkout_path: PathBuf,
    pub repository_key: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_inodes: Option<u64>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoragePlan {
    pub schema_version: u32,
    pub evaluated_at: i64,
    pub storage_root: PathBuf,
    /// The invoking repository's policy is the only policy still available
    /// when a marker names a repository that has been deleted. Existing
    /// owners are never modified by this command; their own repository-local
    /// cleanup remains the policy unit.
    pub orphan_worktree_roots_days: u32,
    pub digest: String,
    pub roots: Vec<StorageRoot>,
    pub candidates: Vec<StorageCandidate>,
    pub primary_checkouts: Vec<StoragePrimaryCheckout>,
    pub primary_candidates: Vec<StoragePrimaryCandidate>,
    #[serde(default)]
    pub preparation_entries: Vec<StoragePreparationEntry>,
    #[serde(default)]
    pub preparation_candidates: Vec<StoragePreparationCandidate>,
    pub summary: StorageSummary,
    pub warnings: Vec<String>,
    /// Recovery archive directories beside the worktree container, one per
    /// repository (or per hand-made kit). Reporting only: host storage never
    /// removes an archive, and these are not part of the digest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovery_archives: Vec<StorageRecoveryArchiveGroup>,
}

/// The selected storage path's shape matters: a keyed container contains one
/// root per repository, while a direct library override is itself the root
/// whose children are sessions. Keeping this distinction through planning,
/// attribution, and apply prevents direct-root sessions from being mistaken
/// for repository roots.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StorageRootLayout {
    KeyedContainer(PathBuf),
    DirectRoot(PathBuf),
}

impl StorageRootLayout {
    fn path(&self) -> &Path {
        match self {
            Self::KeyedContainer(path) | Self::DirectRoot(path) => path,
        }
    }

    fn is_direct(&self) -> bool {
        matches!(self, Self::DirectRoot(_))
    }

    fn digest_tag(&self) -> Option<&'static str> {
        match self {
            // Keep existing keyed-container confirmations byte-for-byte
            // compatible. Only the previously ambiguous direct layout adds a
            // new digest field.
            Self::KeyedContainer(_) => None,
            Self::DirectRoot(_) => Some("direct_root"),
        }
    }
}

/// One directory under `recovery-archives/` and what it holds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageRecoveryArchiveGroup {
    pub path: PathBuf,
    pub archive_count: usize,
    /// Archives carrying a manifest `finish cleanup resolve --archive` wrote.
    /// The rest were made outside the broker and are never proposed by
    /// `gc plan`; only a person can decide what they are worth.
    pub broker_archive_count: usize,
    /// `None` until the directory has been measured.
    pub estimated_bytes: Option<u64>,
    /// The oldest archive's creation time (from its manifest, else the
    /// directory's modification time).
    pub oldest_created_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageAppliedItem {
    pub kind: StorageCandidateKind,
    pub path: PathBuf,
    /// Logical byte accounting for removed content, not an observed increase
    /// in filesystem free space. Shared extents, hard links and concurrent
    /// writers can make physical reclamation differ.
    pub reclaimed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageApplyFailure {
    pub kind: StorageCandidateKind,
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageApplyReport {
    pub schema_version: u32,
    pub digest: String,
    pub complete: bool,
    pub applied: Vec<StorageAppliedItem>,
    pub failures: Vec<StorageApplyFailure>,
    /// Logical byte accounting for removed content, not an observed increase
    /// in filesystem free space. Shared extents, hard links and concurrent
    /// writers can make physical reclamation differ.
    pub reclaimed_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_action: Option<String>,
}

#[derive(Debug, Clone)]
struct MarkerObservation {
    status: StorageMarkerStatus,
    marker: Option<WorktreeRootMarker>,
    bytes: Option<Vec<u8>>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct DiskObservation {
    git_marker: bool,
    estimated_bytes: Option<u64>,
    estimated_inodes: Option<u64>,
}

#[derive(Debug, Clone)]
struct OwnerSources {
    git_registered: BTreeSet<PathBuf>,
    ledger_claimed: BTreeMap<PathBuf, Vec<i64>>,
    session_branch_inventory_complete: bool,
    session_branches: Vec<StorageBranchEntry>,
    worktree_root_binding_state: crate::store::repository_binding::RepositoryWorktreeBindingState,
    worktree_root_binding_path: Option<PathBuf>,
    /// Claimed paths whose every claiming session finished its cleanup.
    retired: BTreeSet<PathBuf>,
}

impl OwnerSources {
    /// Whether durable repository state makes this root ineligible for
    /// empty-root reclamation. Unbound owners do not prove that a discovered
    /// registered root is obsolete; interrupted reservations remain protected.
    fn empty_root_retention_reason(&self, root: &Path) -> Option<&'static str> {
        use crate::store::repository_binding::RepositoryWorktreeBindingState;

        match self.worktree_root_binding_state {
            RepositoryWorktreeBindingState::LegacyUnbound
            | RepositoryWorktreeBindingState::FreshUnbound => {
                Some("repository has no confirmed worktree-root binding")
            }
            RepositoryWorktreeBindingState::Bound
            | RepositoryWorktreeBindingState::FreshReserved
            | RepositoryWorktreeBindingState::RecoveryRequired
                if self
                    .worktree_root_binding_path
                    .as_deref()
                    .is_some_and(|bound| normalise(bound) == normalise(root)) =>
            {
                Some("repository binding selects or reserves this root")
            }
            _ => None,
        }
    }

    /// Whether some session still holds, or means to keep, a worktree here.
    fn has_unretired_claim(&self) -> bool {
        self.ledger_claimed
            .keys()
            .any(|path| !self.retired.contains(path))
    }
}

/// Build the host inventory for the repository containing `path_inside_repo`.
/// This function performs no broker-store creation, migration, lease refresh,
/// or ownership-changing filesystem writes. It may persist one bounded size
/// measurement when the routine policy permits it.
pub fn storage_plan(path_inside_repo: &Path) -> Result<StoragePlan, StorageError> {
    let context = capture_storage_context_for_path(path_inside_repo)?;
    storage_plan_with_context(path_inside_repo, &context)
}

pub(crate) fn storage_plan_with_context(
    path_inside_repo: &Path,
    context: &crate::broker::BrokerStorageContext,
) -> Result<StoragePlan, StorageError> {
    let checkout = GitRepo::discover(path_inside_repo)?;
    let main_root = checkout.main_root()?;
    validate_storage_context(&main_root, context)?;
    let storage_layout = storage_layout_with_context(&main_root, context)?;
    let (policy, warnings) = host_policy(&main_root);
    let mut records = crate::measurement::load_size_records(&main_root);
    let preparation_cache_root_binding =
        context
            .preparation_cache_root_binding()
            .map_err(|error| StorageError::Unavailable {
                reason: format!("captured preparation-cache root is unavailable: {error}"),
            })?;
    let worktree_volume_cache_bindings =
        worktree_volume_cache_bindings_for_context(&main_root, context)?;
    let preparation_cache_root = preparation_cache_root_binding
        .as_ref()
        .map(|binding| binding.path.as_path());
    let preparation_cache_root_identity = preparation_cache_root_binding
        .as_ref()
        .and_then(|binding| binding.identity.as_deref());
    let preparation_cache_root_nonce = preparation_cache_root_binding
        .as_ref()
        .and_then(|binding| binding.instance_nonce.as_deref());
    let mut enumerate_children = preparation_live_root_children;
    let plan = build_plan(
        &main_root,
        &storage_layout,
        preparation_cache_root,
        preparation_cache_root_identity,
        preparation_cache_root_nonce,
        policy.orphan_worktree_roots_days,
        warnings.clone(),
        crate::SizeScan::Recorded,
        &mut records,
        &worktree_volume_cache_bindings,
        context.host_resource_db_path.as_deref(),
        &mut enumerate_children,
    )?;
    if let Some(binding) = preparation_cache_root_binding.as_ref() {
        binding
            .validate()
            .map_err(|error| StorageError::Unavailable {
                reason: format!("captured preparation-cache root changed during planning: {error}"),
            })?;
    }
    if warnings.is_empty() {
        warm_one_storage_size_record(&main_root, &plan, &policy, &mut records);
    }
    Ok(plan)
}

fn capture_storage_context_for_path(
    path_inside_repo: &Path,
) -> Result<crate::broker::BrokerStorageContext, StorageError> {
    let checkout = GitRepo::discover(path_inside_repo)?;
    let main_root = checkout.main_root()?;
    let repository = GitRepo::discover(&main_root)?;
    crate::broker::BrokerStorageContext::capture_for_repository(&repository).map_err(|error| {
        StorageError::Unavailable {
            reason: format!("captured broker storage context is invalid: {error}"),
        }
    })
}

fn validate_storage_context(
    main_root: &Path,
    context: &crate::broker::BrokerStorageContext,
) -> Result<(), StorageError> {
    if context.repository_root.is_none() || context.git_common_dir.is_none() {
        return Err(StorageError::Unavailable {
            reason: "storage operation requires a repository-bound broker context".into(),
        });
    }
    let repository = GitRepo::discover(main_root)?;
    let common_dir = repository.git_common_dir()?;
    if let Some(reason) = context.repository_binding_refusal(main_root, Some(&common_dir)) {
        return Err(StorageError::Unavailable { reason });
    }
    Ok(())
}

fn worktree_volume_cache_bindings_for_context(
    main_root: &Path,
    context: &crate::broker::BrokerStorageContext,
) -> Result<Vec<crate::broker::PreparationCacheRootBinding>, StorageError> {
    let repository = GitRepo::discover(main_root)?;
    let git_common_dir = repository.git_common_dir()?;
    crate::Broker::capture_worktree_volume_preparation_cache_roots(
        main_root,
        &git_common_dir,
        context,
    )
    .map_err(|error| StorageError::Unavailable {
        reason: format!("worktree-volume preparation-cache inventory is incomplete: {error}"),
    })
}

/// Apply exactly one reviewed host storage plan. The digest is checked against
/// a fresh, read-only inventory before any candidate is touched.
pub fn storage_apply(
    path_inside_repo: &Path,
    confirm: &str,
) -> Result<StorageApplyReport, StorageError> {
    let context = capture_storage_context_for_path(path_inside_repo)?;
    storage_apply_with_context(path_inside_repo, confirm, &context)
}

pub(crate) fn storage_apply_with_context(
    path_inside_repo: &Path,
    confirm: &str,
    context: &crate::broker::BrokerStorageContext,
) -> Result<StorageApplyReport, StorageError> {
    if !is_sha256(confirm) {
        return Err(StorageError::ConfirmationNotSha256);
    }
    let checkout = GitRepo::discover(path_inside_repo)?;
    let main_root = checkout.main_root()?;
    validate_storage_context(&main_root, context)?;
    let storage_layout = storage_layout_with_context(&main_root, context)?;
    let (policy, warnings) = host_policy(&main_root);
    let mut records = crate::measurement::load_size_records(&main_root);
    let preparation_cache_root_binding =
        context
            .preparation_cache_root_binding()
            .map_err(|error| StorageError::Unavailable {
                reason: format!("captured preparation-cache root is unavailable: {error}"),
            })?;
    let worktree_volume_cache_bindings =
        worktree_volume_cache_bindings_for_context(&main_root, context)?;
    let preparation_cache_root = preparation_cache_root_binding
        .as_ref()
        .map(|binding| binding.path.clone());
    let mut enumerate_children = preparation_live_root_children;
    let plan = build_apply_plan(
        &main_root,
        &storage_layout,
        preparation_cache_root.as_deref(),
        preparation_cache_root_binding
            .as_ref()
            .and_then(|binding| binding.identity.as_deref()),
        preparation_cache_root_binding
            .as_ref()
            .and_then(|binding| binding.instance_nonce.as_deref()),
        policy.orphan_worktree_roots_days,
        warnings,
        &mut records,
        &worktree_volume_cache_bindings,
        context.host_resource_db_path.as_deref(),
        &mut enumerate_children,
    )?;
    if let Some(binding) = preparation_cache_root_binding.as_ref() {
        binding
            .validate()
            .map_err(|error| StorageError::Unavailable {
                reason: format!(
                    "captured preparation-cache root changed during apply planning: {error}"
                ),
            })?;
    }
    if !plan.digest.eq_ignore_ascii_case(confirm) {
        return Err(StorageError::ConfirmationMismatch);
    }

    let mut cache_bindings = preparation_cache_root_binding
        .into_iter()
        .collect::<Vec<_>>();
    cache_bindings.extend(worktree_volume_cache_bindings);
    let worktree_volume_cache_paths = worktree_volume_cache_paths(&cache_bindings);
    let live_roots = preparation_live_roots(
        &main_root,
        &plan.primary_checkouts,
        &plan.roots,
        &worktree_volume_cache_paths,
    )?;

    let mut applied = Vec::new();
    let mut failures = Vec::new();
    for candidate in &plan.candidates {
        match apply_candidate(candidate, &storage_layout) {
            Ok(reclaimed_bytes) => applied.push(StorageAppliedItem {
                kind: candidate.kind,
                path: candidate.path.clone(),
                reclaimed_bytes,
            }),
            Err(reason) => failures.push(StorageApplyFailure {
                kind: candidate.kind,
                path: candidate.path.clone(),
                reason,
            }),
        }
    }
    for candidate in &plan.preparation_candidates {
        let binding = cache_bindings
            .iter()
            .filter(|binding| candidate.path.starts_with(&binding.path))
            .max_by_key(|binding| binding.path.components().count());
        let result = match (binding, context.host_resource_db_path.as_deref()) {
            (Some(binding), Some(resource_db_path)) => {
                match binding.validate() {
                    Ok(()) => apply_preparation_candidate_with_binding(
                        candidate,
                        &live_roots,
                        resource_db_path,
                        binding,
                    ),
                    Err(error) => Err(format!(
                        "captured preparation-cache root changed before removal: {error}"
                    )),
                }
            }
            (None, _) => Err(
                "no captured preparation-cache root authorizes this entry; it was retained".into(),
            ),
            (_, None) => Err(
                "the captured host resource database path is unavailable; the cache entry was retained"
                    .into(),
            ),
        };
        match result {
            Ok(reclaimed_bytes) => applied.push(StorageAppliedItem {
                kind: StorageCandidateKind::PreparationEntry,
                path: candidate.path.clone(),
                reclaimed_bytes,
            }),
            Err(reason) => failures.push(StorageApplyFailure {
                kind: StorageCandidateKind::PreparationEntry,
                path: candidate.path.clone(),
                reason,
            }),
        }
    }
    for candidate in &plan.primary_candidates {
        match apply_primary_candidate(candidate) {
            Ok(reclaimed_bytes) => applied.push(StorageAppliedItem {
                kind: StorageCandidateKind::PrimaryArtifact,
                path: candidate.path.clone(),
                reclaimed_bytes,
            }),
            Err(reason) => failures.push(StorageApplyFailure {
                kind: StorageCandidateKind::PrimaryArtifact,
                path: candidate.path.clone(),
                reason,
            }),
        }
    }
    let reclaimed_bytes = applied.iter().fold(0_u64, |total, item| {
        total.saturating_add(item.reclaimed_bytes)
    });
    let complete = failures.is_empty();
    Ok(StorageApplyReport {
        schema_version: STORAGE_PLAN_SCHEMA_VERSION,
        digest: confirm.to_ascii_lowercase(),
        complete,
        applied,
        failures,
        reclaimed_bytes,
        recovery_action: (!complete).then(|| "aethyme broker gc storage plan".into()),
    })
}

/// Rebuild the safety decision used by apply without recursively sizing every
/// enrolled tree. Candidate eligibility and confirmation bind filesystem,
/// Git, ledger, marker, and policy facts; byte estimates are reporting-only.
#[allow(clippy::too_many_arguments)]
fn build_apply_plan<F>(
    main_root: &Path,
    storage_layout: &StorageRootLayout,
    preparation_cache_root: Option<&Path>,
    preparation_cache_root_identity: Option<&[u8]>,
    preparation_cache_root_nonce: Option<&str>,
    orphan_worktree_roots_days: u32,
    warnings: Vec<String>,
    records: &mut crate::measurement::SizeRecords,
    worktree_volume_cache_bindings: &[crate::broker::PreparationCacheRootBinding],
    host_resource_db_path: Option<&Path>,
    enumerate_children: &mut F,
) -> Result<StoragePlan, StorageError>
where
    F: FnMut(&Path) -> Result<Vec<PathBuf>, StorageError>,
{
    build_plan(
        main_root,
        storage_layout,
        preparation_cache_root,
        preparation_cache_root_identity,
        preparation_cache_root_nonce,
        orphan_worktree_roots_days,
        warnings,
        crate::SizeScan::Recorded,
        records,
        worktree_volume_cache_bindings,
        host_resource_db_path,
        enumerate_children,
    )
}

// Keep the captured preparation-cache identity components explicit here: the
// plan separately records the optional path, directory identity, and nonce.
#[allow(clippy::too_many_arguments)]
fn build_plan<F>(
    main_root: &Path,
    storage_layout: &StorageRootLayout,
    preparation_cache_root: Option<&Path>,
    preparation_cache_root_identity: Option<&[u8]>,
    preparation_cache_root_nonce: Option<&str>,
    orphan_worktree_roots_days: u32,
    mut warnings: Vec<String>,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
    worktree_volume_cache_bindings: &[crate::broker::PreparationCacheRootBinding],
    host_resource_db_path: Option<&Path>,
    enumerate_children: &mut F,
) -> Result<StoragePlan, StorageError>
where
    F: FnMut(&Path) -> Result<Vec<PathBuf>, StorageError>,
{
    let storage_root = normalise(&absolute_path(main_root, storage_layout.path()));
    let evaluated_at = now_ms();
    let paths = storage_root_entries(&storage_root, storage_layout)?;
    let this_repository = ThisRepository::observe(main_root).ok();
    let additional_cache_bindings = discover_additional_worktree_cache_bindings(
        main_root,
        &paths,
        host_resource_db_path,
        worktree_volume_cache_bindings,
        this_repository.as_ref(),
    );
    let mut verified_worktree_cache_paths =
        worktree_volume_cache_paths(worktree_volume_cache_bindings);
    verified_worktree_cache_paths.extend(worktree_volume_cache_paths(&additional_cache_bindings));

    let mut roots = Vec::with_capacity(paths.len());
    let mut candidates = Vec::new();
    // Read-only: a root with no marker is only told whether this repository
    // can prove it owns it, and how to record that (#257).
    for path in paths {
        let mut root = inspect_root(
            &path,
            evaluated_at,
            orphan_worktree_roots_days,
            scan,
            records,
            &verified_worktree_cache_paths,
        );
        if root.marker_status == StorageMarkerStatus::Missing
            && let Some(this_repository) = &this_repository
        {
            root.blockers
                .push(match attribute_root(&root.path, this_repository) {
                    Attribution::ThisRepository { .. } => "its Git worktrees are registered in this repository; record that ownership with `aethyme broker gc storage attribute --apply`, then review a new plan".into(),
                    Attribution::Unattributable(reason) => format!(
                        "not attributable to this repository: {reason}. Run `aethyme broker gc storage attribute` from the repository that owns it, or see what its worktrees hold with `aethyme broker advanced worktrees`"
                    ),
                });
        }
        candidates.extend(candidates_for_root(&root, orphan_worktree_roots_days));
        roots.push(root);
    }
    candidates.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
    });
    let (primary_checkouts, primary_candidates) =
        inspect_primary_checkouts(main_root, &roots, scan, records);

    // Live roots come from the worktree container, but the cache itself is
    // resolved beside the host resource database by preparation.rs. These
    // locations can differ when the repository overrides worktree placement.
    let live_roots = preparation_live_roots_with_reader(
        main_root,
        &primary_checkouts,
        &roots,
        &verified_worktree_cache_paths,
        enumerate_children,
    )?;
    let mut preparation_entries = Vec::new();
    let mut preparation_candidates = Vec::new();
    if let Some(cache_root) = preparation_cache_root {
        let (entries, candidates) =
            inspect_preparation_cache(cache_root, &live_roots, scan, records);
        preparation_entries.extend(entries);
        preparation_candidates.extend(candidates);
    } else {
        warnings.push(
            "the captured host resource database path is unavailable, so preparation cache entries were retained"
                .into(),
        );
    }
    for binding in worktree_volume_cache_bindings {
        binding
            .validate()
            .map_err(|error| StorageError::Unavailable {
                reason: format!("captured worktree-volume preparation-cache root changed: {error}"),
            })?;
        let (entries, candidates) =
            inspect_preparation_cache(&binding.path, &live_roots, scan, records);
        preparation_entries.extend(entries);
        preparation_candidates.extend(candidates);
    }
    let digest = decision_digest(
        &storage_root,
        DecisionRootMetadata {
            storage_root_layout: storage_layout.digest_tag(),
            preparation_cache_root_identity,
            preparation_cache_root_nonce,
            worktree_volume_cache_bindings,
            additional_worktree_volume_cache_bindings: &additional_cache_bindings,
        },
        orphan_worktree_roots_days,
        &candidates,
        &primary_candidates,
        &preparation_candidates,
    );
    let summary = summarise(
        &roots,
        &candidates,
        &primary_checkouts,
        &primary_candidates,
        &preparation_entries,
        &preparation_candidates,
    );
    let recovery_archives = inspect_recovery_archives(&storage_root, scan, records);
    Ok(StoragePlan {
        schema_version: STORAGE_PLAN_SCHEMA_VERSION,
        evaluated_at,
        storage_root,
        orphan_worktree_roots_days,
        digest,
        roots,
        candidates,
        primary_checkouts,
        primary_candidates,
        preparation_entries,
        preparation_candidates,
        summary,
        warnings,
        recovery_archives,
    })
}

/// Group every directory under `<host>/recovery-archives/` for reporting.
fn inspect_recovery_archives(
    storage_root: &Path,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
) -> Vec<StorageRecoveryArchiveGroup> {
    #[derive(serde::Deserialize)]
    struct BrokerManifest {
        #[allow(dead_code)]
        session_id: i64,
        #[allow(dead_code)]
        head: String,
        created_at_ms: i64,
    }
    let Some(root) = storage_root
        .parent()
        .map(|base| base.join("recovery-archives"))
    else {
        return Vec::new();
    };
    let Ok(groups) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let is_dir = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
    let mut found = Vec::new();
    for group in groups.flatten().map(|entry| entry.path()) {
        if !is_dir(&group) {
            continue;
        }
        let mut entry = StorageRecoveryArchiveGroup {
            path: group.clone(),
            archive_count: 0,
            broker_archive_count: 0,
            estimated_bytes: observed_size(&group, scan, records),
            oldest_created_at_ms: None,
        };
        for archive in std::fs::read_dir(&group)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_dir(path))
        {
            entry.archive_count += 1;
            let manifest = std::fs::read(archive.join("manifest.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<BrokerManifest>(&bytes).ok());
            let created = match &manifest {
                Some(manifest) => {
                    entry.broker_archive_count += 1;
                    Some(manifest.created_at_ms)
                }
                None => std::fs::metadata(&archive)
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|since| since.as_millis() as i64),
            };
            if let Some(created) = created {
                entry.oldest_created_at_ms = Some(
                    entry
                        .oldest_created_at_ms
                        .map_or(created, |oldest| oldest.min(created)),
                );
            }
        }
        found.push(entry);
    }
    found.sort_by(|left, right| left.path.cmp(&right.path));
    found
}

fn inspect_root(
    raw_path: &Path,
    evaluated_at: i64,
    orphan_worktree_roots_days: u32,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
    verified_worktree_cache_paths: &BTreeSet<PathBuf>,
) -> StorageRoot {
    let filesystem_kind = filesystem_kind(raw_path);
    let path = if filesystem_kind == StorageFilesystemKind::Directory {
        normalise(raw_path)
    } else {
        absolute_path(Path::new("/"), raw_path)
    };
    if filesystem_kind != StorageFilesystemKind::Directory {
        let estimated_bytes = observed_size(raw_path, scan, records);
        let estimated_inodes = observed_inodes(raw_path, records);
        return StorageRoot {
            path,
            filesystem_kind,
            repository_key: None,
            repository_root: None,
            marker_status: StorageMarkerStatus::NotApplicable,
            marker_error: Some("host storage roots must be real directories".into()),
            marker_sha256: None,
            owner_exists: None,
            age_days: None,
            worktree_count: 0,
            on_disk_directory_count: 0,
            git_registered_count: 0,
            ledger_claimed_count: 0,
            retired_count: 0,
            empty: false,
            sized: estimated_bytes.is_some() && estimated_inodes.is_some(),
            estimated_bytes,
            estimated_inodes,
            reconciliation: empty_reconciliation(),
            session_branch_inventory_complete: false,
            session_branches: Vec::new(),
            blockers: vec!["entry is not a real directory and is never swept".into()],
        };
    }

    // A checkout placed directly in the container (`git worktree add` aimed
    // one level too high) is a worktree, not a root. Reconciling it as a root
    // would list its source directories as strays -- removable once anything
    // wrote a marker beside them -- so its children are never enumerated.
    if has_git_marker(&path) {
        let estimated_bytes = observed_size(&path, scan, records);
        let estimated_inodes = observed_inodes(&path, records);
        return StorageRoot {
            age_days: age_days(&path, evaluated_at),
            blockers: vec![foreign_worktree_blocker(&path)],
            empty: false,
            estimated_bytes,
            estimated_inodes,
            filesystem_kind,
            git_registered_count: 0,
            ledger_claimed_count: 0,
            marker_error: None,
            marker_sha256: None,
            marker_status: StorageMarkerStatus::NotApplicable,
            on_disk_directory_count: 0,
            owner_exists: None,
            path,
            reconciliation: empty_reconciliation(),
            repository_key: None,
            repository_root: None,
            retired_count: 0,
            sized: estimated_bytes.is_some() && estimated_inodes.is_some(),
            worktree_count: 0,
            session_branch_inventory_complete: false,
            session_branches: Vec::new(),
        };
    }

    let marker = read_marker(&path);
    let mut blockers = Vec::new();
    if let Some(error) = &marker.error {
        blockers.push(error.clone());
    }
    let (repository_key, repository_root, owner_exists) = marker
        .marker
        .as_ref()
        .map(|marker| {
            (
                Some(marker.repository_key.clone()),
                Some(marker.repository_root.clone()),
                Some(is_real_directory(&marker.repository_root)),
            )
        })
        .unwrap_or((None, None, None));
    let marker_sha256 = (marker.status == StorageMarkerStatus::Valid).then(|| {
        format!(
            "{:x}",
            Sha256::digest(marker.bytes.as_deref().unwrap_or_default())
        )
    });

    let mut disk = BTreeMap::new();
    let mut bookkeeping_only = true;
    match std::fs::read_dir(&path) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        blockers.push(format!("cannot enumerate root entry: {error}"));
                        continue;
                    }
                };
                let child = entry.path();
                if is_infrastructure(entry.file_name().as_ref()) {
                    if entry.file_name()
                        == crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT
                    {
                        bookkeeping_only = false;
                        if !verified_worktree_cache_paths.contains(&child) {
                            blockers.push(format!(
                                "{} is a reserved worktree-volume preparation-cache root without a valid repository-bound marker; it is retained",
                                child.display()
                            ));
                        }
                        continue;
                    }
                    bookkeeping_only &= is_broker_bookkeeping(&child);
                    continue;
                }
                bookkeeping_only = false;
                if !is_real_directory(&child) {
                    blockers.push(format!(
                        "{} is not a real directory and is never swept",
                        child.display()
                    ));
                    continue;
                }
                let child = normalise(&child);
                let estimated_bytes = observed_size(&child, scan, records);
                let estimated_inodes = observed_inodes(&child, records);
                disk.insert(
                    child.clone(),
                    DiskObservation {
                        git_marker: has_git_marker(&child),
                        estimated_bytes,
                        estimated_inodes,
                    },
                );
            }
        }
        Err(error) => {
            bookkeeping_only = false;
            blockers.push(format!("cannot enumerate root: {error}"));
        }
    }

    let mut owner_sources = None;
    if let (Some(owner), Some(true)) = (repository_root.as_deref(), owner_exists) {
        match inspect_owner(owner, &path) {
            Ok(sources) => {
                // Binding protects the root's bookkeeping only when the
                // directory would otherwise be proposed as an EmptyRoot.
                // Stray-child cleanup remains independently reviewable.
                if bookkeeping_only
                    && disk.is_empty()
                    && let Some(reason) = sources.empty_root_retention_reason(&path)
                {
                    blockers.push(format!(
                        "durable worktree-root binding: {reason}; keep the root"
                    ));
                }
                owner_sources = Some(sources);
            }
            Err(error) => blockers.push(error),
        }
    }
    if owner_sources
        .as_ref()
        .is_some_and(|sources| !sources.session_branch_inventory_complete)
    {
        blockers.push(
            "local branch refs could not be completely enumerated; branch ownership is unknown and no ref is eligible for removal".into(),
        );
    }

    let mut entries = BTreeMap::new();
    for (path, observation) in &disk {
        entries.insert(
            path.clone(),
            EntryBuilder {
                on_disk: true,
                git_registered: false,
                ledger_claimed: false,
                git_marker: observation.git_marker,
                session_ids: Vec::new(),
                estimated_bytes: observation.estimated_bytes,
                estimated_inodes: observation.estimated_inodes,
            },
        );
    }
    if let Some(sources) = &owner_sources {
        for path in &sources.git_registered {
            entries.entry(path.clone()).or_default().git_registered = true;
        }
        for (path, session_ids) in &sources.ledger_claimed {
            let entry = entries.entry(path.clone()).or_default();
            entry.ledger_claimed = true;
            entry.session_ids = session_ids.clone();
        }
    }
    // A finished session's row outlives its worktree by design. Reporting it
    // as "missing from disk and Git" made every fully cleaned repository look
    // broken, and buried real drift under the record of normal cleanup.
    let mut retired_count = 0;
    if let Some(sources) = &owner_sources {
        entries.retain(|path, entry| {
            let retired = !entry.on_disk && !entry.git_registered && sources.retired.contains(path);
            retired_count += usize::from(retired);
            !retired
        });
    }
    let entries = entries
        .into_iter()
        .map(|(path, entry)| entry.finish(path))
        .collect::<Vec<_>>();
    let reconciliation = StorageReconciliation {
        schema_version: STORAGE_RECONCILIATION_SCHEMA_VERSION,
        on_disk_count: disk.len(),
        git_registered_count: owner_sources
            .as_ref()
            .map_or(0, |sources| sources.git_registered.len()),
        ledger_claimed_count: owner_sources.as_ref().map_or(0, |sources| {
            sources.ledger_claimed.len().saturating_sub(retired_count)
        }),
        retired_count,
        entries,
    };
    let estimated_bytes = observed_size(&path, scan, records);
    let estimated_inodes = observed_inodes(&path, records);
    let sized = estimated_bytes.is_some()
        && estimated_inodes.is_some()
        && reconciliation
            .entries
            .iter()
            .filter(|entry| entry.on_disk)
            .all(|entry| entry.estimated_bytes.is_some() && entry.estimated_inodes.is_some());
    let age_days = age_days(&path, evaluated_at);
    if marker.status != StorageMarkerStatus::Valid {
        blockers.push(format!(
            "root has no usable {WORKTREE_ROOT_MARKER} ownership evidence and is never swept"
        ));
    } else if owner_exists == Some(true) && owner_sources.is_none() {
        blockers.push("owning repository exists but its Git or session ledger could not be inspected; no stray directory is swept".into());
    } else if owner_exists == Some(false) && age_days.is_none() {
        blockers.push("cannot determine orphan root age; no removal is authorized".into());
    } else if owner_exists == Some(false)
        && age_days.is_none_or(|age| age < orphan_worktree_roots_days)
    {
        blockers.push(format!(
            "orphan root is younger than the {orphan_worktree_roots_days} day grace period"
        ));
    }
    StorageRoot {
        path,
        filesystem_kind,
        repository_key,
        repository_root,
        marker_status: marker.status,
        marker_error: marker.error,
        marker_sha256,
        owner_exists,
        age_days,
        worktree_count: disk.len(),
        on_disk_directory_count: disk.len(),
        git_registered_count: reconciliation.git_registered_count,
        ledger_claimed_count: reconciliation.ledger_claimed_count,
        retired_count: reconciliation.retired_count,
        empty: bookkeeping_only && disk.is_empty(),
        estimated_bytes,
        estimated_inodes,
        sized,
        reconciliation,
        session_branch_inventory_complete: owner_sources
            .as_ref()
            .is_some_and(|sources| sources.session_branch_inventory_complete),
        session_branches: owner_sources
            .as_ref()
            .map_or_else(Vec::new, |sources| sources.session_branches.clone()),
        blockers,
    }
}

fn inspect_primary_checkouts(
    invoking_main_root: &Path,
    roots: &[StorageRoot],
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
) -> (Vec<StoragePrimaryCheckout>, Vec<StoragePrimaryCandidate>) {
    let mut paths = BTreeSet::new();
    if is_enrolled_primary_checkout(invoking_main_root) {
        paths.insert(normalise(invoking_main_root));
    }
    for root in roots {
        if root.marker_status == StorageMarkerStatus::Valid
            && root.owner_exists == Some(true)
            && root
                .repository_root
                .as_deref()
                .is_some_and(is_enrolled_primary_checkout)
        {
            paths.insert(normalise(root.repository_root.as_deref().unwrap()));
        }
    }

    let mut checkouts = paths
        .into_iter()
        .map(|path| inspect_primary_checkout(&path, scan, records))
        .collect::<Vec<_>>();
    checkouts.sort_by(|left, right| left.path.cmp(&right.path));
    // Reporting spans every enrolled checkout on the host; deleting does not.
    // `storage apply` is confirmed by one digest produced from wherever the
    // operator happened to run `plan`, and nothing in that confirmation names
    // another repository. A sibling checkout therefore stays visible in the
    // inventory -- which is what makes the disk legible -- while only the
    // invoking checkout can lose bytes to it.
    let invoking = normalise(invoking_main_root);
    let mut candidates = checkouts
        .iter()
        .filter(|checkout| normalise(&checkout.path) == invoking)
        .flat_map(|checkout| {
            checkout
                .artifacts
                .iter()
                .filter(|artifact| artifact.reclaimable)
                .map(|artifact| StoragePrimaryCandidate {
                    path: artifact.path.clone(),
                    checkout_path: checkout.path.clone(),
                    repository_key: checkout.repository_key.clone(),
                    name: artifact.name.clone(),
                    estimated_bytes: artifact.estimated_bytes,
                    estimated_inodes: artifact.estimated_inodes,
                    reason: artifact.reason.clone(),
                })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.path.cmp(&right.path));
    (checkouts, candidates)
}

/// How long an artifact directory must have been still before the primary
/// lane will consider deleting it.
///
/// Long enough to span a link step or a slow test binary, short enough that a
/// checkout nobody is working in becomes reclaimable within one coffee break.
const PRIMARY_ARTIFACT_IDLE_MS: u64 = 30 * 60 * 1_000;

/// Whether `dir` or any of its immediate entries changed inside `window_ms`.
///
/// Only the top level is read. A deep walk of a multi-gigabyte `target/` is
/// precisely the cost this lane exists to reclaim, and a live build touches the
/// top level often enough -- profile directories, lock files, fingerprint
/// stamps -- for one level to answer the question being asked.
///
/// Every unreadable case answers "recently modified". The caller uses this to
/// decide whether deleting is safe, so not knowing must never read as safe.
fn directory_modified_within(dir: &Path, window_ms: u64) -> bool {
    let cutoff = match SystemTime::now().checked_sub(Duration::from_millis(window_ms)) {
        Some(cutoff) => cutoff,
        None => return true,
    };
    let recent = |metadata: &std::fs::Metadata| {
        metadata
            .modified()
            .map(|modified| modified > cutoff)
            .unwrap_or(true)
    };
    if std::fs::symlink_metadata(dir)
        .as_ref()
        .map(recent)
        .unwrap_or(true)
    {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    entries
        .flatten()
        .any(|entry| entry.metadata().as_ref().map(recent).unwrap_or(true))
}

/// Inventory the shared preparation cache, and decide which entries are dead.
///
/// An entry is named after the key its repository computed: a content hash of
/// the declared inputs, the config and the platform. So the live set is not a
/// guess -- it is exactly the keys the checkouts on this host would recompute
/// now. Anything else is a key nobody will ask for again, because the inputs
/// that produced it have changed.
///
/// `live_roots` are the checkouts entitled to keep an entry alive. A checkout
/// that declares no shared step contributes no key and keeps nothing, which is
/// correct: it never populated the cache either.
/// Reclaim dead preparation-cache entries without an operator present.
///
/// The manual lane exists so a human can review before deleting. That review
/// is the right default for worktrees, which hold work; it is the wrong
/// default for a content-addressed cache, whose entries are named after the
/// inputs that produced them. An entry no checkout computes will never be read
/// again no matter how long it is kept, so waiting for someone to type a
/// command only trades disk for nothing. Measured here: 13.3 GB reclaimed by
/// hand came back as 15 GB in three days, because nothing reclaimed it.
///
/// Result of one bounded preparation-cache sweep. `complete` is false when
/// discovery, the deadline, a lease, or deletion prevented the sweep from
/// deciding every eligible entry. A caller may report progress while keeping
/// its durable recovery request pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreparationCacheSweepReport {
    pub(crate) entries_removed: usize,
    pub(crate) bytes_reclaimed: u64,
    /// Filesystem entries actually unlinked, including partial progress from
    /// interrupted or failed removals that must resume at the same tree.
    pub(crate) filesystem_entries_reclaimed: usize,
    pub(crate) complete: bool,
    pub(crate) coverage_deferral: Option<PreparationCacheCoverageDeferral>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparationCacheCoverageDeferral {
    HostConsumersNotEnumerated,
    ExplicitRootConsumersNotEnumerated,
    WorktreeVolumeConsumersNotProven,
    CacheBindingsNotFullyEnumerated,
}

impl PreparationCacheSweepReport {
    fn complete_empty() -> Self {
        Self {
            entries_removed: 0,
            bytes_reclaimed: 0,
            filesystem_entries_reclaimed: 0,
            complete: true,
            coverage_deferral: None,
        }
    }

    fn incomplete_empty() -> Self {
        Self {
            complete: false,
            ..Self::complete_empty()
        }
    }
}

fn preparation_cache_sweep_report(
    entries_removed: usize,
    bytes_reclaimed: u64,
    filesystem_entries_reclaimed: usize,
    complete: bool,
) -> PreparationCacheSweepReport {
    PreparationCacheSweepReport {
        entries_removed,
        bytes_reclaimed,
        filesystem_entries_reclaimed,
        complete,
        coverage_deferral: None,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct PreparationRotationJournalLap {
    resource_db_path: PathBuf,
    cache_root: PathBuf,
    root_identity: Vec<u8>,
    generation: u64,
    finished: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl PreparationRotationJournalLap {
    fn keep_incomplete(&self) {
        crate::warn_unrecorded(
            "retain incomplete preparation rotation journal lap",
            crate::preparation::rotation_journal::mark_lap_incomplete(
                &self.resource_db_path,
                &self.cache_root,
                &self.root_identity,
            ),
        );
    }

    fn finish(&mut self, sweep_complete: bool) -> bool {
        if self.finished {
            return sweep_complete;
        }
        if !sweep_complete {
            self.keep_incomplete();
            self.finished = true;
            return false;
        }
        let current_identity =
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&self.cache_root)
                .and_then(|identity| identity.encode());
        if !matches!(current_identity, Ok(identity) if identity == self.root_identity.as_slice()) {
            self.keep_incomplete();
            self.finished = true;
            return false;
        }
        match crate::preparation::rotation_journal::acknowledge_lap(
            &self.resource_db_path,
            &self.cache_root,
            &self.root_identity,
            self.generation,
        ) {
            Ok(()) => {
                self.finished = true;
                true
            }
            Err(error) => {
                crate::warn_unrecorded(
                    "acknowledge completed preparation rotation journal lap",
                    Err::<(), _>(error),
                );
                self.keep_incomplete();
                self.finished = true;
                false
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for PreparationRotationJournalLap {
    fn drop(&mut self) {
        if !self.finished {
            self.keep_incomplete();
            self.finished = true;
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn preparation_rotation_journal_root_identity(
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    deadline: std::time::Instant,
) -> Result<Option<Vec<u8>>, ()> {
    if check_preparation_sweep_deadline(deadline).is_err() {
        return Err(());
    }
    let identity = match crate::preparation::rotation_debt::DirectoryIdentity::capture(cache_root) {
        Ok(identity) => identity.encode().map(Some).map_err(|_| ())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match crate::preparation::rotation_debt::cache_root_absent_under_observed_parent(
                cache_root, deadline,
            ) {
                Ok(true) => None,
                _ => return Err(()),
            }
        }
        Err(_) => return Err(()),
    };
    if expected_root_identity.is_some_and(|expected| identity.as_deref() != Some(expected)) {
        return Err(());
    }
    Ok(identity)
}

// Absence or an observed empty directory needs no consumer census: there
// are no generic entries to prune. This never authorizes removal and must
// not treat a failed or truncated inventory as empty.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn preparation_cache_is_observed_empty(
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
    deadline: std::time::Instant,
) -> bool {
    if root_binding.is_some_and(|binding| binding.validate().is_err()) {
        return false;
    }
    let before = match preparation_rotation_journal_root_identity(
        cache_root,
        expected_root_identity,
        deadline,
    ) {
        Ok(identity) => identity,
        Err(()) => return false,
    };
    if before.is_some() {
        let Ok(mut entries) = std::fs::read_dir(cache_root) else {
            return false;
        };
        if entries.next().is_some() {
            return false;
        }
    }
    root_binding.is_none_or(|binding| binding.validate().is_ok())
        && preparation_rotation_journal_root_identity(cache_root, expected_root_identity, deadline)
            == Ok(before)
}

/// Test adapter retaining the historical tuple result for fixture assertions.
#[cfg(test)]
fn sweep_preparation_cache_at(
    main_root: &Path,
    storage_root: &Path,
    cache_root: &Path,
    resource_db_path: &Path,
    deadline: std::time::Instant,
) -> (usize, u64) {
    let report = sweep_preparation_cache_at_with_status(
        main_root,
        storage_root,
        cache_root,
        resource_db_path,
        deadline,
    );
    (report.entries_removed, report.bytes_reclaimed)
}

#[cfg(test)]
fn sweep_preparation_cache_at_with_status(
    main_root: &Path,
    storage_root: &Path,
    cache_root: &Path,
    resource_db_path: &Path,
    deadline: std::time::Instant,
) -> PreparationCacheSweepReport {
    sweep_preparation_cache_with_discovery(
        cache_root,
        None,
        None,
        resource_db_path,
        deadline,
        |deadline| discover_preparation_live_roots(main_root, storage_root, deadline),
    )
}

/// Sweep using the exact worktree roots selected by one Broker instance.
#[cfg(test)]
pub(crate) fn sweep_preparation_cache_for_roots_with_status(
    main_root: &Path,
    worktree_roots: &[PathBuf],
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    resource_db_path: &Path,
    deadline: std::time::Instant,
) -> PreparationCacheSweepReport {
    if check_preparation_sweep_deadline(deadline).is_err() || worktree_roots.is_empty() {
        return PreparationCacheSweepReport::incomplete_empty();
    }
    sweep_preparation_cache_with_discovery(
        cache_root,
        expected_root_identity,
        None,
        resource_db_path,
        deadline,
        |deadline| {
            discover_preparation_live_roots_from_roots(main_root, worktree_roots, &[], deadline)
        },
    )
}

/// Sweep the captured preparation-cache namespace while retaining its outer
/// root marker guard across discovery and every destructive step.
#[cfg(test)]
pub(crate) fn sweep_preparation_cache_for_roots_with_binding(
    main_root: &Path,
    worktree_roots: &[PathBuf],
    binding: &crate::broker::PreparationCacheRootBinding,
    resource_db_path: &Path,
    deadline: std::time::Instant,
) -> PreparationCacheSweepReport {
    sweep_preparation_caches_for_roots_with_bindings(
        main_root,
        worktree_roots,
        std::slice::from_ref(binding),
        resource_db_path,
        deadline,
    )
}

/// Sweep every captured preparation-cache namespace while proving liveness
/// against the same complete set of marker-bound worktree-volume roots.
#[cfg(test)]
pub(crate) fn sweep_preparation_caches_for_roots_with_bindings(
    main_root: &Path,
    worktree_roots: &[PathBuf],
    bindings: &[crate::broker::PreparationCacheRootBinding],
    resource_db_path: &Path,
    deadline: std::time::Instant,
) -> PreparationCacheSweepReport {
    if check_preparation_sweep_deadline(deadline).is_err()
        || worktree_roots.is_empty()
        || bindings.is_empty()
    {
        return PreparationCacheSweepReport::incomplete_empty();
    }
    let mut total = PreparationCacheSweepReport {
        entries_removed: 0,
        bytes_reclaimed: 0,
        filesystem_entries_reclaimed: 0,
        complete: true,
        coverage_deferral: None,
    };
    for binding in bindings {
        if check_preparation_sweep_deadline(deadline).is_err() || binding.validate().is_err() {
            total.complete = false;
            break;
        }
        let report = sweep_preparation_cache_with_discovery(
            &binding.path,
            binding.identity.as_deref(),
            Some(binding),
            resource_db_path,
            deadline,
            |deadline| {
                discover_preparation_live_roots_from_roots(
                    main_root,
                    worktree_roots,
                    bindings,
                    deadline,
                )
            },
        );
        total.entries_removed = total.entries_removed.saturating_add(report.entries_removed);
        total.bytes_reclaimed = total.bytes_reclaimed.saturating_add(report.bytes_reclaimed);
        total.filesystem_entries_reclaimed = total
            .filesystem_entries_reclaimed
            .saturating_add(report.filesystem_entries_reclaimed);
        total.complete &= report.complete;
        if !total.complete {
            break;
        }
    }
    total
}

/// Recover independently witnessed cache-rotation work without treating the
/// caller's repository roots as a complete consumer inventory for the cache
/// namespaces. Root-plan or binding-discovery failure leaves known bindings
/// usable for recovery, but keeps the aggregate result incomplete.
pub(crate) fn recover_preparation_caches_for_bindings(
    bindings: &[crate::broker::PreparationCacheRootBinding],
    resource_db_path: &Path,
    deadline: std::time::Instant,
    bindings_fully_enumerated: bool,
) -> PreparationCacheSweepReport {
    if check_preparation_sweep_deadline(deadline).is_err() {
        return PreparationCacheSweepReport::incomplete_empty();
    }
    let mut total = PreparationCacheSweepReport::complete_empty();
    if bindings.is_empty() {
        total.complete = false;
        total.coverage_deferral =
            Some(PreparationCacheCoverageDeferral::CacheBindingsNotFullyEnumerated);
        return total;
    }
    for binding in bindings {
        if check_preparation_sweep_deadline(deadline).is_err() {
            total.complete = false;
            break;
        }
        if binding.validate().is_err() {
            total.complete = false;
            continue;
        }
        let reason = if binding
            .root_guard
            .as_ref()
            .is_some_and(|guard| guard.placement_root.is_some())
        {
            PreparationCacheCoverageDeferral::WorktreeVolumeConsumersNotProven
        } else if binding.instance_nonce.is_some() {
            PreparationCacheCoverageDeferral::ExplicitRootConsumersNotEnumerated
        } else {
            PreparationCacheCoverageDeferral::HostConsumersNotEnumerated
        };
        let report = sweep_preparation_cache_recovery_only(
            &binding.path,
            binding.identity.as_deref(),
            Some(binding),
            resource_db_path,
            deadline,
            reason,
        );
        total.entries_removed = total.entries_removed.saturating_add(report.entries_removed);
        total.bytes_reclaimed = total.bytes_reclaimed.saturating_add(report.bytes_reclaimed);
        total.filesystem_entries_reclaimed = total
            .filesystem_entries_reclaimed
            .saturating_add(report.filesystem_entries_reclaimed);
        total.complete &= report.complete;
        if total.coverage_deferral.is_none() {
            total.coverage_deferral = report.coverage_deferral;
        }
    }
    if !bindings_fully_enumerated {
        total.complete = false;
        total.coverage_deferral =
            Some(PreparationCacheCoverageDeferral::CacheBindingsNotFullyEnumerated);
    }
    total
}

#[cfg(test)]
fn sweep_preparation_cache_with_discovery(
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
    resource_db_path: &Path,
    deadline: std::time::Instant,
    discover_live_roots: impl FnMut(std::time::Instant) -> Result<Vec<PathBuf>, StorageError>,
) -> PreparationCacheSweepReport {
    sweep_preparation_cache_with_discovery_and_observer(
        cache_root,
        expected_root_identity,
        root_binding,
        resource_db_path,
        deadline,
        discover_live_roots,
        &mut || Ok(()),
    )
}

#[cfg(test)]
fn sweep_preparation_cache_with_discovery_and_observer(
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
    resource_db_path: &Path,
    deadline: std::time::Instant,
    discover_live_roots: impl FnMut(std::time::Instant) -> Result<Vec<PathBuf>, StorageError>,
    before_mutation_observer: &mut impl FnMut() -> std::io::Result<()>,
) -> PreparationCacheSweepReport {
    sweep_preparation_cache_with_coverage_and_observer(
        cache_root,
        expected_root_identity,
        root_binding,
        resource_db_path,
        deadline,
        (None, discover_live_roots),
        before_mutation_observer,
    )
}

fn sweep_preparation_cache_recovery_only(
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
    resource_db_path: &Path,
    deadline: std::time::Instant,
    reason: PreparationCacheCoverageDeferral,
) -> PreparationCacheSweepReport {
    sweep_preparation_cache_with_coverage_and_observer(
        cache_root,
        expected_root_identity,
        root_binding,
        resource_db_path,
        deadline,
        (Some(reason), |_| {
            Err(StorageError::Unavailable {
                reason: "consumer inventory is incomplete".into(),
            })
        }),
        &mut || Ok(()),
    )
}

fn sweep_preparation_cache_with_coverage_and_observer(
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
    resource_db_path: &Path,
    deadline: std::time::Instant,
    coverage: (
        Option<PreparationCacheCoverageDeferral>,
        impl FnMut(std::time::Instant) -> Result<Vec<PathBuf>, StorageError>,
    ),
    before_mutation_observer: &mut impl FnMut() -> std::io::Result<()>,
) -> PreparationCacheSweepReport {
    let (coverage_deferral, mut discover_live_roots) = coverage;
    let mut removed = 0_usize;
    let mut bytes = 0_u64;
    let mut filesystem_entries_reclaimed = 0_usize;
    let mut complete = true;
    let mut coordinator = None;
    if let Some(binding) = root_binding
        && binding.validate().is_err()
    {
        return PreparationCacheSweepReport::incomplete_empty();
    }
    if !crate::preparation::preparation_cache_root_identity_matches(
        cache_root,
        expected_root_identity,
    ) {
        return PreparationCacheSweepReport::incomplete_empty();
    }
    let inventory = match crate::preparation::rotation_debt::inventory(
        cache_root,
        PREPARATION_ROTATION_DEBT_MAX_ENTRIES,
        deadline,
    ) {
        Ok(inventory) => inventory,
        Err(_) => return PreparationCacheSweepReport::incomplete_empty(),
    };
    complete &= inventory.complete;
    let mut blocked_repositories = inventory.blocked_repositories.clone();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let journal_root_identity = match preparation_rotation_journal_root_identity(
        cache_root,
        expected_root_identity,
        deadline,
    ) {
        Ok(identity) => identity,
        Err(()) => {
            return preparation_cache_sweep_report(
                removed,
                bytes,
                filesystem_entries_reclaimed,
                false,
            );
        }
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    match crate::preparation::rotation_journal::probe(
        resource_db_path,
        cache_root,
        journal_root_identity.as_deref(),
    ) {
        Ok(Some(crate::preparation::rotation_journal::JournalProbe::Unknown)) | Err(_) => {
            return preparation_cache_sweep_report(
                removed,
                bytes,
                filesystem_entries_reclaimed,
                false,
            );
        }
        Ok(Some(crate::preparation::rotation_journal::JournalProbe::NoDebt))
        | Ok(Some(crate::preparation::rotation_journal::JournalProbe::Pending))
        | Ok(None) => {}
    }

    // A debt witness is explicit authority for the original cache rotation.
    // Recover it under the same exclusive key as writers before the generic
    // liveness sweep sees either the repository directory or any old retired
    // tree. Each repository is independent, so a busy or malformed record
    // blocks only that repository while other cache keys can still progress.
    for repository in &inventory.repositories {
        if check_preparation_sweep_deadline(deadline).is_err() {
            return preparation_cache_sweep_report(
                removed,
                bytes,
                filesystem_entries_reclaimed,
                false,
            );
        }
        let grant = match acquire_preparation_cache_lease(
            &mut coordinator,
            resource_db_path,
            cache_root,
            repository,
            deadline,
        ) {
            Ok(grant) => grant,
            Err(PreparationCacheLeaseError::Busy) => {
                complete = false;
                continue;
            }
            Err(PreparationCacheLeaseError::Deadline) => {
                return preparation_cache_sweep_report(
                    removed,
                    bytes,
                    filesystem_entries_reclaimed,
                    false,
                );
            }
            Err(PreparationCacheLeaseError::Unavailable(_reason)) => {
                complete = false;
                continue;
            }
        };
        if root_binding.is_some_and(|binding| binding.validate().is_err()) {
            complete = false;
            if !release_preparation_cache_lease(&mut coordinator, &grant) {
                complete = false;
            }
            continue;
        }
        let recovery = crate::preparation::recover_rotation_debt_for_cache_sweep_with_binding(
            cache_root,
            expected_root_identity,
            resource_db_path,
            repository,
            root_binding,
            deadline,
        );
        let released = release_preparation_cache_lease(&mut coordinator, &grant);
        if !released {
            complete = false;
        }
        match recovery {
            Ok(progress) => {
                filesystem_entries_reclaimed =
                    filesystem_entries_reclaimed.saturating_add(progress.entries_reclaimed);
                bytes = bytes.saturating_add(progress.bytes_reclaimed);
                if progress.complete && (progress.found_debt || inventory.complete) {
                    blocked_repositories.remove(repository);
                    if progress.found_debt && progress.entries_reclaimed > 0 {
                        removed = removed.saturating_add(1);
                    }
                } else {
                    complete = false;
                }
            }
            Err(error) => {
                filesystem_entries_reclaimed =
                    filesystem_entries_reclaimed.saturating_add(error.progress.entries_reclaimed);
                bytes = bytes.saturating_add(error.progress.bytes_reclaimed);
                complete = false;
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut rotation_journal_lap: Option<PreparationRotationJournalLap> = None;
    let mut rotation_journal_lap_active = false;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let current_identity = match preparation_rotation_journal_root_identity(
            cache_root,
            expected_root_identity,
            deadline,
        ) {
            Ok(identity) if identity == journal_root_identity => identity,
            _ => {
                return preparation_cache_sweep_report(
                    removed,
                    bytes,
                    filesystem_entries_reclaimed,
                    false,
                );
            }
        };
        let signal = match crate::preparation::rotation_journal::probe(
            resource_db_path,
            cache_root,
            current_identity.as_deref(),
        ) {
            Ok(signal) => signal,
            Err(_) => {
                return preparation_cache_sweep_report(
                    removed,
                    bytes,
                    filesystem_entries_reclaimed,
                    false,
                );
            }
        };
        match signal {
            Some(crate::preparation::rotation_journal::JournalProbe::Unknown) => {
                return preparation_cache_sweep_report(
                    removed,
                    bytes,
                    filesystem_entries_reclaimed,
                    false,
                );
            }
            Some(crate::preparation::rotation_journal::JournalProbe::Pending) => {
                if !inventory.complete {
                    // Individual SQLite rows are exact, independently validated
                    // recovery records. Process them for fairness, but preserve
                    // the incomplete bit so this partial inventory cannot ack.
                    complete = false;
                }
                if let Some(root_identity) = current_identity {
                    let generation = match crate::preparation::rotation_journal::begin_lap(
                        resource_db_path,
                        cache_root,
                        &root_identity,
                    ) {
                        Ok(generation) => generation,
                        Err(_) => {
                            complete = false;
                            u64::MAX
                        }
                    };
                    if generation != u64::MAX {
                        rotation_journal_lap_active = true;
                        rotation_journal_lap = Some(PreparationRotationJournalLap {
                            resource_db_path: resource_db_path.to_path_buf(),
                            cache_root: cache_root.to_path_buf(),
                            root_identity: root_identity.clone(),
                            generation,
                            finished: false,
                        });
                        if !inventory.complete {
                            crate::warn_unrecorded(
                                "retain incomplete preparation rotation journal lap",
                                crate::preparation::rotation_journal::mark_lap_incomplete(
                                    resource_db_path,
                                    cache_root,
                                    &root_identity,
                                ),
                            );
                        }
                        'journal_pages: loop {
                            if check_preparation_sweep_deadline(deadline).is_err() {
                                complete = false;
                                break;
                            }
                            let page = match crate::preparation::rotation_journal::next_page(
                                resource_db_path,
                                cache_root,
                                &root_identity,
                            ) {
                                Ok(page) => page,
                                Err(_) => {
                                    complete = false;
                                    break;
                                }
                            };
                            if page.entries.is_empty() {
                                if !page.lap_exhausted {
                                    complete = false;
                                }
                                break;
                            }
                            for entry in page.entries {
                                if check_preparation_sweep_deadline(deadline).is_err() {
                                    complete = false;
                                    break 'journal_pages;
                                }
                                let Some(repository) = entry.repository else {
                                    complete = false;
                                    continue;
                                };
                                if !entry.payload_valid {
                                    complete = false;
                                    continue;
                                }
                                let payload = match crate::preparation::rotation_journal::load(
                                    resource_db_path,
                                    cache_root,
                                    &root_identity,
                                    &repository,
                                ) {
                                    Ok(Some(payload)) => payload,
                                    _ => {
                                        complete = false;
                                        continue;
                                    }
                                };
                                if crate::preparation::rotation_debt::RotationDebt::from_journal_payload(
                                    cache_root,
                                    &repository,
                                    &payload,
                                )
                                .is_err()
                                {
                                    complete = false;
                                    continue;
                                }
                                let grant = match acquire_preparation_cache_lease(
                                    &mut coordinator,
                                    resource_db_path,
                                    cache_root,
                                    &repository,
                                    deadline,
                                ) {
                                    Ok(grant) => grant,
                                    Err(PreparationCacheLeaseError::Busy)
                                    | Err(PreparationCacheLeaseError::Unavailable(_)) => {
                                        complete = false;
                                        continue;
                                    }
                                    Err(PreparationCacheLeaseError::Deadline) => {
                                        complete = false;
                                        break 'journal_pages;
                                    }
                                };
                                let recovery =
                                    crate::preparation::recover_rotation_debt_for_cache_sweep_with_binding(
                                        cache_root,
                                        expected_root_identity,
                                        resource_db_path,
                                        &repository,
                                        root_binding,
                                        deadline,
                                    );
                                let released =
                                    release_preparation_cache_lease(&mut coordinator, &grant);
                                if !released {
                                    complete = false;
                                }
                                if !account_journal_rotation_recovery(
                                    recovery,
                                    &mut removed,
                                    &mut bytes,
                                    &mut filesystem_entries_reclaimed,
                                ) {
                                    complete = false;
                                }
                            }
                        }
                    }
                } else {
                    complete = false;
                }
            }
            Some(crate::preparation::rotation_journal::JournalProbe::NoDebt) | None => {}
        }
    }

    if let Some(reason) = coverage_deferral {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let observed_empty = preparation_cache_is_observed_empty(
            cache_root,
            expected_root_identity,
            root_binding,
            deadline,
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let observed_empty = false;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(lap) = rotation_journal_lap.as_mut() {
            // Recovery settlement is independent of the consumer census.
            // Nonempty unwitnessed cache content remains deferred.
            lap.finish(complete);
        }
        return PreparationCacheSweepReport {
            entries_removed: removed,
            bytes_reclaimed: bytes,
            filesystem_entries_reclaimed,
            complete: complete && observed_empty,
            coverage_deferral: (!observed_empty).then_some(reason),
        };
    }

    let live_roots = match discover_live_roots(deadline) {
        Ok(roots) => roots,
        Err(_) => {
            return preparation_cache_sweep_report(
                removed,
                bytes,
                filesystem_entries_reclaimed,
                false,
            );
        }
    };
    if live_roots.is_empty() {
        return preparation_cache_sweep_report(removed, bytes, filesystem_entries_reclaimed, false);
    }
    let live_keys = match current_preparation_keys(&live_roots, deadline) {
        Ok(keys) => keys,
        Err(_) => {
            return preparation_cache_sweep_report(
                removed,
                bytes,
                filesystem_entries_reclaimed,
                false,
            );
        }
    };
    if check_preparation_sweep_deadline(deadline).is_err() {
        return preparation_cache_sweep_report(removed, bytes, filesystem_entries_reclaimed, false);
    }
    let cache_metadata = match std::fs::symlink_metadata(cache_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let root_absent =
                crate::preparation::rotation_debt::cache_root_absent_under_observed_parent(
                    cache_root, deadline,
                )
                .unwrap_or(false);
            let no_inventory_debt =
                inventory.repositories.is_empty() && inventory.blocked_repositories.is_empty();
            return preparation_cache_sweep_report(
                removed,
                bytes,
                filesystem_entries_reclaimed,
                complete && no_inventory_debt && root_absent && !rotation_journal_lap_active,
            );
        }
        Err(_) => {
            return preparation_cache_sweep_report(
                removed,
                bytes,
                filesystem_entries_reclaimed,
                false,
            );
        }
    };
    if cache_metadata.file_type().is_symlink() || !cache_metadata.is_dir() {
        return preparation_cache_sweep_report(removed, bytes, filesystem_entries_reclaimed, false);
    }
    if check_preparation_sweep_deadline(deadline).is_err() {
        return preparation_cache_sweep_report(removed, bytes, filesystem_entries_reclaimed, false);
    }
    let repositories = match std::fs::read_dir(cache_root) {
        Ok(entries) => entries,
        Err(_) => {
            return preparation_cache_sweep_report(
                removed,
                bytes,
                filesystem_entries_reclaimed,
                false,
            );
        }
    };
    if check_preparation_sweep_deadline(deadline).is_err() {
        return preparation_cache_sweep_report(removed, bytes, filesystem_entries_reclaimed, false);
    }
    for repository in repositories {
        if check_preparation_sweep_deadline(deadline).is_err() {
            complete = false;
            break;
        }
        let Ok(repository) = repository else {
            complete = false;
            break;
        };
        let repository_path = repository.path();
        let Ok(repository_type) = repository.file_type() else {
            complete = false;
            break;
        };
        if repository_type.is_symlink() || !repository_type.is_dir() {
            complete = false;
            continue;
        }
        let Some(repository_name) = repository_path.file_name().and_then(|name| name.to_str())
        else {
            return PreparationCacheSweepReport {
                entries_removed: removed,
                bytes_reclaimed: bytes,
                filesystem_entries_reclaimed,
                complete: false,
                coverage_deferral: None,
            };
        };
        let Some((lease_repository, retired)) =
            preparation_cache_repository_identity(repository_name)
        else {
            complete = false;
            continue;
        };
        if root_binding.is_some_and(|binding| binding.validate().is_err()) {
            return preparation_cache_sweep_report(
                removed,
                bytes,
                filesystem_entries_reclaimed,
                false,
            );
        }
        if retired {
            if inventory.block_legacy_retired || blocked_repositories.contains(lease_repository) {
                complete = false;
                continue;
            }
            let grant = match acquire_preparation_cache_lease(
                &mut coordinator,
                resource_db_path,
                cache_root,
                lease_repository,
                deadline,
            ) {
                Ok(grant) => grant,
                Err(PreparationCacheLeaseError::Busy) => {
                    complete = false;
                    continue;
                }
                Err(PreparationCacheLeaseError::Deadline) => {
                    return PreparationCacheSweepReport {
                        entries_removed: removed,
                        bytes_reclaimed: bytes,
                        filesystem_entries_reclaimed,
                        complete: false,
                        coverage_deferral: None,
                    };
                }
                Err(PreparationCacheLeaseError::Unavailable(_reason)) => {
                    return PreparationCacheSweepReport {
                        entries_removed: removed,
                        bytes_reclaimed: bytes,
                        filesystem_entries_reclaimed,
                        complete: false,
                        coverage_deferral: None,
                    };
                }
            };
            if root_binding.is_some_and(|binding| binding.validate().is_err()) {
                complete = false;
                if !release_preparation_cache_lease(&mut coordinator, &grant) {
                    complete = false;
                }
                continue;
            }
            // Inventory ran before this lease was acquired. A writer may
            // have published durable rotation debt in that gap, then crashed
            // and released the lease. Reclassify under the lease before the
            // generic retired-tree path can gain deletion authority.
            if !account_rotation_recovery(
                crate::preparation::recover_rotation_debt_for_cache_sweep_with_binding(
                    cache_root,
                    expected_root_identity,
                    resource_db_path,
                    lease_repository,
                    root_binding,
                    deadline,
                ),
                &mut removed,
                &mut bytes,
                &mut filesystem_entries_reclaimed,
                &mut complete,
            ) {
                if !release_preparation_cache_lease(&mut coordinator, &grant) {
                    complete = false;
                }
                continue;
            }
            // The budget rotator renames a complete repository store while
            // holding the original repository lease. The retired path is no
            // longer used by writers, but the same lease prevents cleanup
            // from racing another rotation or active preparation command.
            let outcome = (|| {
                if root_binding.is_some_and(|binding| binding.validate().is_err()) {
                    return PreparationCacheSweepOutcome::Stop(0);
                }
                let current_roots = match discover_live_roots(deadline) {
                    Ok(roots) => roots,
                    Err(_) => return PreparationCacheSweepOutcome::Stop(0),
                };
                if current_preparation_keys(&current_roots, deadline).is_err() {
                    return PreparationCacheSweepOutcome::Stop(0);
                }
                if repository_path.parent() != Some(cache_root)
                    || !is_real_directory(cache_root)
                    || !retired_preparation_store_layout(&repository_path, Some(deadline))
                {
                    return PreparationCacheSweepOutcome::Protected;
                }
                let usage =
                    match crate::disk_headroom::directory_usage_bounded(&repository_path, deadline)
                    {
                        Some(usage) => usage,
                        None => return PreparationCacheSweepOutcome::Stop(0),
                    };
                if check_preparation_sweep_deadline(deadline).is_err()
                    || repository_path.parent() != Some(cache_root)
                    || !is_real_directory(cache_root)
                    || !retired_preparation_store_layout(&repository_path, Some(deadline))
                {
                    return PreparationCacheSweepOutcome::Stop(0);
                }
                if !crate::preparation::preparation_cache_root_identity_matches(
                    cache_root,
                    expected_root_identity,
                ) {
                    return PreparationCacheSweepOutcome::Stop(0);
                }
                if root_binding.is_some_and(|binding| binding.validate().is_err()) {
                    return PreparationCacheSweepOutcome::Stop(0);
                }
                match remove_preparation_cache_tree_with_binding_and_observer(
                    &repository_path,
                    Some(deadline),
                    root_binding,
                    before_mutation_observer,
                ) {
                    Ok((TreeRemoval::Complete, count)) => {
                        PreparationCacheSweepOutcome::Removed(usage.bytes, count)
                    }
                    Ok((TreeRemoval::Interrupted, count)) => {
                        PreparationCacheSweepOutcome::Stop(count)
                    }
                    Err(error) => PreparationCacheSweepOutcome::Failed(error.entries_reclaimed),
                }
            })();
            if !release_preparation_cache_lease(&mut coordinator, &grant) {
                complete = false;
            }
            match outcome {
                PreparationCacheSweepOutcome::Removed(reclaimed, count) => {
                    removed += 1;
                    bytes = bytes.saturating_add(reclaimed);
                    filesystem_entries_reclaimed =
                        filesystem_entries_reclaimed.saturating_add(count);
                }
                PreparationCacheSweepOutcome::Protected => {
                    complete = false;
                    continue;
                }
                PreparationCacheSweepOutcome::Failed(count) => {
                    filesystem_entries_reclaimed =
                        filesystem_entries_reclaimed.saturating_add(count);
                    complete = false;
                }
                PreparationCacheSweepOutcome::Stop(count) => {
                    filesystem_entries_reclaimed =
                        filesystem_entries_reclaimed.saturating_add(count);
                    return PreparationCacheSweepReport {
                        entries_removed: removed,
                        bytes_reclaimed: bytes,
                        filesystem_entries_reclaimed,
                        complete: false,
                        coverage_deferral: None,
                    };
                }
            }
            continue;
        }
        if blocked_repositories.contains(lease_repository) {
            complete = false;
            continue;
        }
        let entries = match std::fs::read_dir(&repository_path) {
            Ok(entries) => entries,
            Err(_) => {
                complete = false;
                break;
            }
        };
        for entry in entries {
            if check_preparation_sweep_deadline(deadline).is_err() {
                return PreparationCacheSweepReport {
                    entries_removed: removed,
                    bytes_reclaimed: bytes,
                    filesystem_entries_reclaimed,
                    complete: false,
                    coverage_deferral: None,
                };
            }
            let Ok(entry) = entry else {
                return PreparationCacheSweepReport {
                    entries_removed: removed,
                    bytes_reclaimed: bytes,
                    filesystem_entries_reclaimed,
                    complete: false,
                    coverage_deferral: None,
                };
            };
            let Ok(file_type) = entry.file_type() else {
                return PreparationCacheSweepReport {
                    entries_removed: removed,
                    bytes_reclaimed: bytes,
                    filesystem_entries_reclaimed,
                    complete: false,
                    coverage_deferral: None,
                };
            };
            if file_type.is_symlink() || !file_type.is_dir() {
                complete = false;
                continue;
            }
            let key = entry.file_name().to_string_lossy().into_owned();
            if !is_preparation_cache_key(&key) {
                complete = false;
                continue;
            }
            if live_keys.contains(&key) {
                continue;
            }

            let candidate_path = entry.path();
            let grant = match acquire_preparation_cache_lease(
                &mut coordinator,
                resource_db_path,
                cache_root,
                lease_repository,
                deadline,
            ) {
                Ok(grant) => grant,
                Err(PreparationCacheLeaseError::Busy) => {
                    complete = false;
                    continue;
                }
                Err(PreparationCacheLeaseError::Deadline) => {
                    return PreparationCacheSweepReport {
                        entries_removed: removed,
                        bytes_reclaimed: bytes,
                        filesystem_entries_reclaimed,
                        complete: false,
                        coverage_deferral: None,
                    };
                }
                Err(PreparationCacheLeaseError::Unavailable(_reason)) => {
                    return PreparationCacheSweepReport {
                        entries_removed: removed,
                        bytes_reclaimed: bytes,
                        filesystem_entries_reclaimed,
                        complete: false,
                        coverage_deferral: None,
                    };
                }
            };
            if root_binding.is_some_and(|binding| binding.validate().is_err()) {
                complete = false;
                if !release_preparation_cache_lease(&mut coordinator, &grant) {
                    complete = false;
                }
                continue;
            }

            // Revalidate durable rotation intent after acquiring the writer's
            // exact lease. A sidecar published after the initial bounded
            // inventory must still take precedence over deleting a child key.
            if !account_rotation_recovery(
                crate::preparation::recover_rotation_debt_for_cache_sweep_with_binding(
                    cache_root,
                    expected_root_identity,
                    resource_db_path,
                    lease_repository,
                    root_binding,
                    deadline,
                ),
                &mut removed,
                &mut bytes,
                &mut filesystem_entries_reclaimed,
                &mut complete,
            ) {
                if !release_preparation_cache_lease(&mut coordinator, &grant) {
                    complete = false;
                }
                continue;
            }

            // The writer and sweeper share this exclusive key. Re-discover
            // liveness only after acquiring it, then hold it through removal
            // so preparation cannot start using the entry between the check
            // and delete.
            let outcome = (|| {
                if root_binding.is_some_and(|binding| binding.validate().is_err()) {
                    return PreparationCacheSweepOutcome::Stop(0);
                }
                let current_roots = match discover_live_roots(deadline) {
                    Ok(roots) => roots,
                    Err(_) => return PreparationCacheSweepOutcome::Stop(0),
                };
                let current_keys = match current_preparation_keys(&current_roots, deadline) {
                    Ok(keys) => keys,
                    Err(_) => return PreparationCacheSweepOutcome::Stop(0),
                };
                if current_keys.contains(&key) {
                    return PreparationCacheSweepOutcome::Protected;
                }
                if candidate_path.parent() != Some(repository_path.as_path())
                    || repository_path.parent() != Some(cache_root)
                    || !is_real_directory(cache_root)
                    || !is_real_directory(&repository_path)
                {
                    return PreparationCacheSweepOutcome::Stop(0);
                }
                if check_preparation_sweep_deadline(deadline).is_err()
                    || !is_real_directory(&candidate_path)
                {
                    return PreparationCacheSweepOutcome::Stop(0);
                }
                let usage = match crate::disk_headroom::directory_usage_bounded(
                    &candidate_path,
                    deadline,
                ) {
                    Some(usage) => usage,
                    None => return PreparationCacheSweepOutcome::Stop(0),
                };
                if check_preparation_sweep_deadline(deadline).is_err()
                    || !is_real_directory(cache_root)
                    || !is_real_directory(&repository_path)
                    || !is_real_directory(&candidate_path)
                {
                    return PreparationCacheSweepOutcome::Stop(0);
                }
                if !crate::preparation::preparation_cache_root_identity_matches(
                    cache_root,
                    expected_root_identity,
                ) {
                    return PreparationCacheSweepOutcome::Stop(0);
                }
                if root_binding.is_some_and(|binding| binding.validate().is_err()) {
                    return PreparationCacheSweepOutcome::Stop(0);
                }
                match remove_preparation_cache_tree_with_binding_and_observer(
                    &candidate_path,
                    Some(deadline),
                    root_binding,
                    before_mutation_observer,
                ) {
                    Ok((TreeRemoval::Complete, count)) => {
                        PreparationCacheSweepOutcome::Removed(usage.bytes, count)
                    }
                    // Count only a fully removed cache entry. The interrupted
                    // entry can already have lost files, so `bytes` remains a
                    // conservative lower bound for this pass.
                    Ok((TreeRemoval::Interrupted, count)) => {
                        PreparationCacheSweepOutcome::Stop(count)
                    }
                    Err(error) => PreparationCacheSweepOutcome::Failed(error.entries_reclaimed),
                }
            })();
            if !release_preparation_cache_lease(&mut coordinator, &grant) {
                complete = false;
            }
            match outcome {
                PreparationCacheSweepOutcome::Removed(reclaimed, count) => {
                    removed += 1;
                    bytes = bytes.saturating_add(reclaimed);
                    filesystem_entries_reclaimed =
                        filesystem_entries_reclaimed.saturating_add(count);
                }
                PreparationCacheSweepOutcome::Protected => {
                    complete = false;
                    continue;
                }
                PreparationCacheSweepOutcome::Failed(count) => {
                    filesystem_entries_reclaimed =
                        filesystem_entries_reclaimed.saturating_add(count);
                    complete = false;
                }
                PreparationCacheSweepOutcome::Stop(count) => {
                    filesystem_entries_reclaimed =
                        filesystem_entries_reclaimed.saturating_add(count);
                    return PreparationCacheSweepReport {
                        entries_removed: removed,
                        bytes_reclaimed: bytes,
                        filesystem_entries_reclaimed,
                        complete: false,
                        coverage_deferral: None,
                    };
                }
            }
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(lap) = rotation_journal_lap.as_mut()
        && !lap.finish(complete)
    {
        complete = false;
    }
    PreparationCacheSweepReport {
        entries_removed: removed,
        bytes_reclaimed: bytes,
        filesystem_entries_reclaimed,
        complete,
        coverage_deferral: None,
    }
}

/// Apply exact progress from a rotation-debt recovery performed under the
/// repository's preparation-cache lease. Return true only when no debt was
/// found and the generic cache candidate may proceed through its own checks.
fn account_rotation_recovery(
    recovery: Result<
        crate::preparation::rotation_debt::RecoveryProgress,
        crate::preparation::rotation_debt::RecoveryError,
    >,
    entries_removed: &mut usize,
    bytes_reclaimed: &mut u64,
    filesystem_entries_reclaimed: &mut usize,
    complete: &mut bool,
) -> bool {
    match recovery {
        Ok(progress) => {
            *filesystem_entries_reclaimed =
                filesystem_entries_reclaimed.saturating_add(progress.entries_reclaimed);
            *bytes_reclaimed = bytes_reclaimed.saturating_add(progress.bytes_reclaimed);
            if progress.complete && progress.found_debt {
                if progress.entries_reclaimed > 0 {
                    *entries_removed = entries_removed.saturating_add(1);
                }
                false
            } else if progress.complete {
                true
            } else {
                *complete = false;
                false
            }
        }
        Err(error) => {
            *filesystem_entries_reclaimed =
                filesystem_entries_reclaimed.saturating_add(error.progress.entries_reclaimed);
            *bytes_reclaimed = bytes_reclaimed.saturating_add(error.progress.bytes_reclaimed);
            *complete = false;
            false
        }
    }
}

/// Account for a journal-row recovery while holding the repository lease.
/// A row can disappear between page selection and lease acquisition if a
/// writer completed it first; that is a successful no-op, while any recovery
/// uncertainty keeps the journal lap incomplete.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn account_journal_rotation_recovery(
    recovery: Result<
        crate::preparation::rotation_debt::RecoveryProgress,
        crate::preparation::rotation_debt::RecoveryError,
    >,
    entries_removed: &mut usize,
    bytes_reclaimed: &mut u64,
    filesystem_entries_reclaimed: &mut usize,
) -> bool {
    match recovery {
        Ok(progress) => {
            *filesystem_entries_reclaimed =
                filesystem_entries_reclaimed.saturating_add(progress.entries_reclaimed);
            *bytes_reclaimed = bytes_reclaimed.saturating_add(progress.bytes_reclaimed);
            if progress.complete {
                if progress.found_debt && progress.entries_reclaimed > 0 {
                    *entries_removed = entries_removed.saturating_add(1);
                }
                true
            } else {
                false
            }
        }
        Err(error) => {
            *filesystem_entries_reclaimed =
                filesystem_entries_reclaimed.saturating_add(error.progress.entries_reclaimed);
            *bytes_reclaimed = bytes_reclaimed.saturating_add(error.progress.bytes_reclaimed);
            false
        }
    }
}

enum PreparationCacheSweepOutcome {
    Removed(u64, usize),
    Protected,
    Failed(usize),
    Stop(usize),
}

#[derive(Debug)]
enum PreparationCacheLeaseError {
    Busy,
    Deadline,
    Unavailable(String),
}

fn acquire_preparation_cache_lease(
    coordinator: &mut Option<crate::HostResourceCoordinator>,
    resource_db_path: &Path,
    cache_root: &Path,
    repository: &str,
    deadline: std::time::Instant,
) -> Result<crate::HostResourceGrant, PreparationCacheLeaseError> {
    if check_preparation_sweep_deadline(deadline).is_err() {
        return Err(PreparationCacheLeaseError::Deadline);
    }
    if coordinator.is_none() {
        let metadata = std::fs::symlink_metadata(resource_db_path).map_err(|error| {
            PreparationCacheLeaseError::Unavailable(format!(
                "cannot inspect host resource database {}: {error}",
                resource_db_path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(PreparationCacheLeaseError::Unavailable(
                "host resource database is not a regular file".into(),
            ));
        }
        let open_wait = deadline
            .saturating_duration_since(std::time::Instant::now())
            .min(PREPARATION_COORDINATOR_OPEN_MAX_WAIT);
        if open_wait.is_zero() {
            return Err(PreparationCacheLeaseError::Deadline);
        }
        let opened =
            crate::HostResourceCoordinator::open_with_busy_timeout(resource_db_path, open_wait)
                .map_err(|error| {
                    PreparationCacheLeaseError::Unavailable(format!(
                        "cannot open host resource database: {error}"
                    ))
                })?;
        if check_preparation_sweep_deadline(deadline).is_err() {
            return Err(PreparationCacheLeaseError::Deadline);
        }
        *coordinator = Some(opened);
    }

    let request = preparation_cache_lease_request(repository, cache_root, deadline)?;
    let grant = coordinator
        .as_mut()
        .expect("the resource coordinator was opened above")
        .acquire_with_wait(&request, Duration::ZERO, |_| {})
        .map_err(|error| match error {
            crate::HostResourceError::Conflict { .. } => PreparationCacheLeaseError::Busy,
            error => PreparationCacheLeaseError::Unavailable(format!(
                "cannot acquire preparation-cache lease: {error}"
            )),
        })?;
    if check_preparation_sweep_deadline(deadline).is_err() {
        release_preparation_cache_lease(coordinator, &grant);
        return Err(PreparationCacheLeaseError::Deadline);
    }
    Ok(grant)
}

fn preparation_cache_lease_request(
    repository: &str,
    cache_root: &Path,
    deadline: std::time::Instant,
) -> Result<crate::HostResourceRequest, PreparationCacheLeaseError> {
    if !is_lowercase_hex(repository, 64) {
        return Err(PreparationCacheLeaseError::Unavailable(
            "preparation-cache repository identity is not a lowercase SHA-256 digest".into(),
        ));
    }
    static NEXT_REQUEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err(PreparationCacheLeaseError::Deadline);
    }
    let remaining_seconds = remaining
        .as_secs()
        .saturating_add(if remaining.subsec_nanos() > 0 { 1 } else { 0 });
    let ttl_seconds = remaining_seconds.saturating_add(PREPARATION_LEASE_TTL_MARGIN_SECONDS);
    if ttl_seconds > HOST_RESOURCE_MAX_TTL_SECONDS {
        return Err(PreparationCacheLeaseError::Unavailable(
            "sweep deadline exceeds the host-resource lease TTL limit".into(),
        ));
    }
    let sequence = NEXT_REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let run_id = format!("preparation-cache-gc-{}-{sequence}", std::process::id());
    let request_id = format!("{run_id}-{}", now_ms());
    let worktree_fingerprint = format!(
        "{:x}",
        Sha256::digest(cache_root.as_os_str().to_string_lossy().as_bytes())
    );
    Ok(crate::HostResourceRequest {
        schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id,
        repository: repository.to_string(),
        worktree_fingerprint,
        run_id,
        ttl_seconds: ttl_seconds.max(15),
        holder_pid: Some(std::process::id()),
        resources: vec![crate::HostResourceRequirement {
            key: "preparation_cache".into(),
            resource: crate::HostResourceKind::ExclusiveKey {
                name: crate::preparation::preparation_cache_lease_name(repository),
            },
        }],
    })
}

fn release_preparation_cache_lease(
    coordinator: &mut Option<crate::HostResourceCoordinator>,
    grant: &crate::HostResourceGrant,
) -> bool {
    let result = coordinator
        .as_mut()
        .expect("a grant must have an open coordinator")
        .release(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        );
    let released = result.is_ok();
    crate::warn_unrecorded("release preparation-cache storage lease", result);
    released
}

fn check_preparation_sweep_deadline(deadline: std::time::Instant) -> Result<(), StorageError> {
    if std::time::Instant::now() >= deadline {
        return Err(StorageError::Unavailable {
            reason: "preparation-cache liveness deadline elapsed".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
fn discover_preparation_live_roots(
    main_root: &Path,
    storage_root: &Path,
    deadline: std::time::Instant,
) -> Result<Vec<PathBuf>, StorageError> {
    check_preparation_sweep_deadline(deadline)?;
    let main_metadata =
        std::fs::symlink_metadata(main_root).map_err(|source| StorageError::Io {
            path: main_root.to_path_buf(),
            source,
        })?;
    if main_metadata.file_type().is_symlink() || !main_metadata.is_dir() {
        return Err(StorageError::Unavailable {
            reason: "invoking checkout is not a real directory".into(),
        });
    }
    check_preparation_sweep_deadline(deadline)?;
    let storage_metadata = match std::fs::symlink_metadata(storage_root) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(vec![main_root.to_path_buf()]);
        }
        Err(source) => {
            return Err(StorageError::Io {
                path: storage_root.to_path_buf(),
                source,
            });
        }
    };
    if storage_metadata.file_type().is_symlink() || !storage_metadata.is_dir() {
        return Err(StorageError::Unavailable {
            reason: "worktree storage root is not a real directory".into(),
        });
    }
    check_preparation_sweep_deadline(deadline)?;
    let entries = std::fs::read_dir(storage_root).map_err(|source| StorageError::Io {
        path: storage_root.to_path_buf(),
        source,
    })?;
    let mut roots = BTreeSet::from([main_root.to_path_buf()]);
    let mut examined = 0_usize;
    for entry in entries {
        check_preparation_sweep_deadline(deadline)?;
        let entry = entry.map_err(|source| StorageError::Io {
            path: storage_root.to_path_buf(),
            source,
        })?;
        examined += 1;
        if examined > PREPARATION_SWEEP_MAX_DISCOVERY_PATHS {
            return Err(StorageError::Unavailable {
                reason: "worktree-root discovery exceeded its entry bound".into(),
            });
        }
        let file_type = entry.file_type().map_err(|source| StorageError::Io {
            path: entry.path(),
            source,
        })?;
        check_preparation_sweep_deadline(deadline)?;
        if file_type.is_symlink() {
            return Err(StorageError::Unavailable {
                reason: "worktree storage contains a symlinked root".into(),
            });
        }
        if !file_type.is_dir() {
            continue;
        }
        let root = entry.path();
        roots.insert(root.clone());
        let marker = read_worktree_marker_for_liveness(&root, deadline)?.ok_or_else(|| {
            StorageError::Unavailable {
                reason: "worktree root has no readable ownership marker".into(),
            }
        })?;
        if marker.schema_version != WORKTREE_ROOT_SCHEMA_VERSION
            || marker.repository_key.is_empty()
            || !marker.repository_root.is_absolute()
        {
            return Err(StorageError::Unavailable {
                reason: "worktree root ownership marker is invalid".into(),
            });
        }
        roots.insert(marker.repository_root);

        let children = std::fs::read_dir(&root).map_err(|source| StorageError::Io {
            path: root.clone(),
            source,
        })?;
        check_preparation_sweep_deadline(deadline)?;
        for child in children {
            check_preparation_sweep_deadline(deadline)?;
            let child = child.map_err(|source| StorageError::Io {
                path: root.clone(),
                source,
            })?;
            examined += 1;
            if examined > PREPARATION_SWEEP_MAX_DISCOVERY_PATHS {
                return Err(StorageError::Unavailable {
                    reason: "worktree-root discovery exceeded its entry bound".into(),
                });
            }
            let child_type = child.file_type().map_err(|source| StorageError::Io {
                path: child.path(),
                source,
            })?;
            check_preparation_sweep_deadline(deadline)?;
            if child_type.is_symlink() {
                return Err(StorageError::Unavailable {
                    reason: "worktree root contains a symlinked checkout".into(),
                });
            }
            if child_type.is_dir() {
                if child.file_name()
                    == crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT
                {
                    return Err(StorageError::Unavailable {
                        reason: format!(
                            "worktree root contains an unverified reserved preparation-cache child at {}; cache liveness is incomplete",
                            child.path().display()
                        ),
                    });
                }
                roots.insert(child.path());
            }
        }
    }
    check_preparation_sweep_deadline(deadline)?;
    Ok(roots.into_iter().collect())
}

/// Resolve live checkouts from the exact worktree roots selected by one
/// Broker's captured storage context. Each candidate is the repository root
/// that carries the ownership marker; unlike the environment-default scanner,
/// no parent directory is re-resolved from process state.
#[cfg(test)]
fn discover_preparation_live_roots_from_roots(
    main_root: &Path,
    worktree_roots: &[PathBuf],
    cache_bindings: &[crate::broker::PreparationCacheRootBinding],
    deadline: std::time::Instant,
) -> Result<Vec<PathBuf>, StorageError> {
    check_preparation_sweep_deadline(deadline)?;
    for binding in cache_bindings {
        binding
            .validate()
            .map_err(|error| StorageError::Unavailable {
                reason: format!("captured preparation-cache root changed: {error}"),
            })?;
    }
    let verified_cache_paths = worktree_volume_cache_paths(cache_bindings);
    let main_metadata =
        std::fs::symlink_metadata(main_root).map_err(|source| StorageError::Io {
            path: main_root.to_path_buf(),
            source,
        })?;
    if main_metadata.file_type().is_symlink() || !main_metadata.is_dir() {
        return Err(StorageError::Unavailable {
            reason: "invoking checkout is not a real directory".into(),
        });
    }

    let mut roots = BTreeSet::from([main_root.to_path_buf()]);
    let mut examined = 0_usize;
    for root in worktree_roots {
        check_preparation_sweep_deadline(deadline)?;
        let metadata = match std::fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(StorageError::Io {
                    path: root.clone(),
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(StorageError::Unavailable {
                reason: format!(
                    "worktree storage root {} is not a real directory",
                    root.display()
                ),
            });
        }
        let marker = read_worktree_marker_for_liveness(root, deadline)?.ok_or_else(|| {
            StorageError::Unavailable {
                reason: format!(
                    "worktree root {} has no readable ownership marker",
                    root.display()
                ),
            }
        })?;
        if marker.schema_version != WORKTREE_ROOT_SCHEMA_VERSION
            || marker.repository_key.is_empty()
            || !marker.repository_root.is_absolute()
        {
            return Err(StorageError::Unavailable {
                reason: format!(
                    "worktree root {} has an invalid ownership marker",
                    root.display()
                ),
            });
        }
        roots.insert(root.clone());
        roots.insert(marker.repository_root);

        let children = std::fs::read_dir(root).map_err(|source| StorageError::Io {
            path: root.clone(),
            source,
        })?;
        check_preparation_sweep_deadline(deadline)?;
        for child in children {
            check_preparation_sweep_deadline(deadline)?;
            let child = child.map_err(|source| StorageError::Io {
                path: root.clone(),
                source,
            })?;
            examined += 1;
            if examined > PREPARATION_SWEEP_MAX_DISCOVERY_PATHS {
                return Err(StorageError::Unavailable {
                    reason: "worktree-root discovery exceeded its entry bound".into(),
                });
            }
            let file_type = child.file_type().map_err(|source| StorageError::Io {
                path: child.path(),
                source,
            })?;
            check_preparation_sweep_deadline(deadline)?;
            if file_type.is_symlink() {
                return Err(StorageError::Unavailable {
                    reason: "worktree root contains a symlinked checkout".into(),
                });
            }
            if file_type.is_dir() {
                let child_path = child.path();
                if child.file_name()
                    == crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT
                {
                    if verified_cache_paths.contains(&child_path) {
                        continue;
                    }
                    return Err(StorageError::Unavailable {
                        reason: format!(
                            "worktree root contains an unverified reserved preparation-cache child at {}; cache liveness is incomplete",
                            child_path.display()
                        ),
                    });
                }
                roots.insert(child_path);
            }
        }
    }
    check_preparation_sweep_deadline(deadline)?;
    Ok(roots.into_iter().collect())
}

fn current_preparation_keys(
    roots: &[PathBuf],
    deadline: std::time::Instant,
) -> Result<BTreeSet<String>, StorageError> {
    let mut keys = BTreeSet::new();
    for root in roots {
        check_preparation_sweep_deadline(deadline)?;
        // Inventory can contain direct regular-file roots (for example, a
        // .DS_Store entry). They cannot contain a checkout, so omit them from
        // cache liveness. Do not treat any other lstat result as safe; defer
        // to the shared reader so its existing error handling remains
        // authoritative for symlinks, special files, and I/O failures.
        if matches!(
            std::fs::symlink_metadata(root),
            Ok(metadata) if metadata.file_type().is_file()
        ) {
            continue;
        }
        let key = crate::preparation::current_cache_key_before(root, Some(deadline)).map_err(
            |error| StorageError::Unavailable {
                reason: format!("checkout preparation liveness is unknown: {error}"),
            },
        )?;
        if let Some(key) = key {
            keys.insert(key);
        }
    }
    Ok(keys)
}

#[cfg(test)]
fn read_worktree_marker_for_liveness(
    root: &Path,
    deadline: std::time::Instant,
) -> Result<Option<WorktreeRootMarker>, StorageError> {
    check_preparation_sweep_deadline(deadline)?;
    let path = root.join(WORKTREE_ROOT_MARKER);
    let Some(bytes) = crate::preparation::read_bounded_regular_file_for_liveness(
        &path,
        PREPARATION_SWEEP_MAX_MARKER_BYTES,
        deadline,
    )
    .map_err(|error| StorageError::Unavailable {
        reason: format!("worktree ownership marker could not be verified: {error}"),
    })?
    else {
        return Ok(None);
    };
    let marker = serde_json::from_slice(&bytes).map_err(|error| StorageError::Unavailable {
        reason: format!("worktree ownership marker is unreadable: {error}"),
    })?;
    check_preparation_sweep_deadline(deadline)?;
    Ok(Some(marker))
}

fn preparation_live_roots(
    main_root: &Path,
    primary_checkouts: &[StoragePrimaryCheckout],
    roots: &[StorageRoot],
    verified_worktree_cache_paths: &BTreeSet<PathBuf>,
) -> Result<Vec<PathBuf>, StorageError> {
    let mut enumerate_children = preparation_live_root_children;
    preparation_live_roots_with_reader(
        main_root,
        primary_checkouts,
        roots,
        verified_worktree_cache_paths,
        &mut enumerate_children,
    )
}

fn preparation_live_roots_with_reader<F>(
    main_root: &Path,
    primary_checkouts: &[StoragePrimaryCheckout],
    roots: &[StorageRoot],
    verified_worktree_cache_paths: &BTreeSet<PathBuf>,
    enumerate_children: &mut F,
) -> Result<Vec<PathBuf>, StorageError>
where
    F: FnMut(&Path) -> Result<Vec<PathBuf>, StorageError>,
{
    let mut enumerate_verified_children = |parent: &Path| {
        let children = enumerate_children(parent)?;
        for child in &children {
            if child.file_name().is_some_and(|name| {
                name == std::ffi::OsStr::new(
                    crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT,
                )
            }) && !verified_worktree_cache_paths.contains(child)
            {
                return Err(StorageError::Unavailable {
                    reason: format!(
                        "live-root inventory found an unverified reserved preparation-cache child at {}; cache liveness is incomplete",
                        child.display()
                    ),
                });
            }
        }
        Ok(children)
    };
    let mut live_roots = primary_checkouts
        .iter()
        .map(|checkout| normalise(&checkout.path))
        .collect::<Vec<_>>();
    live_roots.push(normalise(main_root));
    // Use the same population for inventory and immediate deletion checks.
    // Session worktrees, not just primary checkouts, consume shared entries.
    for root in roots {
        // `inspect_root` already knows that a non-directory entry is not a
        // storage container, and that a Git checkout placed at the container
        // level is never reconciled as one. Preserve the observed path without
        // walking through it. Cache liveness distinguishes regular files,
        // which cannot contain a checkout, from symlinks and other unresolved
        // types, which keep the inventory unknown.
        if root.filesystem_kind != StorageFilesystemKind::Directory
            || root.marker_status == StorageMarkerStatus::NotApplicable
        {
            // Keep the observed spelling: `normalise` canonicalizes symlinks,
            // which would turn an unknown root into its target.
            live_roots.push(root.path.clone());
            continue;
        }
        for child_path in enumerate_verified_children(&root.path)? {
            if !verified_worktree_cache_paths.contains(&child_path) {
                live_roots.push(normalise(&child_path));
            }
        }
    }
    // Include sessions from every exact captured placement root that carries
    // a valid cache. A fallback root may not be in the currently selected
    // storage layout, but its live worktrees still protect shared cache keys.
    for cache_path in verified_worktree_cache_paths {
        let Some(placement_root) = cache_path.parent() else {
            continue;
        };
        for child_path in enumerate_verified_children(placement_root)? {
            if child_path != *cache_path {
                live_roots.push(normalise(&child_path));
            }
        }
    }
    live_roots.sort();
    live_roots.dedup();
    Ok(live_roots)
}

fn preparation_live_root_children(root: &Path) -> Result<Vec<PathBuf>, StorageError> {
    let metadata = std::fs::symlink_metadata(root).map_err(|source| StorageError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StorageError::Unavailable {
            reason: format!(
                "live-root inventory path {} is not a real directory; cache liveness is incomplete",
                root.display()
            ),
        });
    }
    let children = std::fs::read_dir(root).map_err(|source| StorageError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let mut paths = Vec::new();
    for child in children {
        let child = child.map_err(|source| StorageError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let path = child.path();
        let file_type = child.file_type().map_err(|source| StorageError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_symlink() {
            return Err(StorageError::Unavailable {
                reason: format!(
                    "live-root inventory encountered a symlink at {}; cache liveness is incomplete",
                    path.display()
                ),
            });
        }
        if !file_type.is_dir() {
            if child.file_name()
                == std::ffi::OsStr::new(
                    crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT,
                )
            {
                return Err(StorageError::Unavailable {
                    reason: format!(
                        "live-root inventory found a reserved preparation-cache child that is not a real directory at {}; cache liveness is incomplete",
                        path.display()
                    ),
                });
            }
            continue;
        }
        paths.push(path);
    }
    Ok(paths)
}

fn discover_additional_worktree_cache_bindings(
    main_root: &Path,
    placement_roots: &[PathBuf],
    host_resource_db_path: Option<&Path>,
    existing_bindings: &[crate::broker::PreparationCacheRootBinding],
    this_repository: Option<&ThisRepository>,
) -> Vec<crate::broker::PreparationCacheRootBinding> {
    let Some(resource_db_path) = host_resource_db_path else {
        return Vec::new();
    };
    let existing_paths = worktree_volume_cache_paths(existing_bindings);
    let main_root = normalise(main_root);
    let discovery_started = Instant::now();

    crate::git::with_git_deadline(WORKTREE_CACHE_ROOT_DISCOVERY_BUDGET, || {
        let mut bindings = Vec::new();
        for placement_root in placement_roots {
            if discovery_started.elapsed() >= WORKTREE_CACHE_ROOT_DISCOVERY_BUDGET {
                break;
            }
            let placement_metadata = match std::fs::symlink_metadata(placement_root) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if placement_metadata.file_type().is_symlink() || !placement_metadata.is_dir() {
                continue;
            }
            let cache_root = placement_root
                .join(crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT);
            if existing_paths.contains(&cache_root) {
                continue;
            }
            let cache_metadata = match std::fs::symlink_metadata(&cache_root) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if cache_metadata.file_type().is_symlink() || !cache_metadata.is_dir() {
                continue;
            }

            let marker_path = placement_root.join(WORKTREE_ROOT_MARKER);
            let marker_bytes = match crate::preparation::read_bounded_regular_file_for_liveness(
                &marker_path,
                WORKTREE_ROOT_MARKER_MAX_BYTES,
                discovery_started + WORKTREE_CACHE_ROOT_DISCOVERY_BUDGET,
            ) {
                Ok(Some(bytes)) => bytes,
                Ok(None) | Err(_) => continue,
            };
            let marker = match serde_json::from_slice::<WorktreeRootMarker>(&marker_bytes) {
                Ok(marker)
                    if marker.schema_version == WORKTREE_ROOT_SCHEMA_VERSION
                        && !marker.repository_key.is_empty() =>
                {
                    marker
                }
                Ok(_) | Err(_) => continue,
            };
            let marker_root = normalise(&marker.repository_root);
            let repository_key_matches = if marker_root == main_root {
                this_repository.is_some_and(|repository| {
                    repository.main_root == main_root && repository.key == marker.repository_key
                })
            } else {
                let Ok(repository) = GitRepo::discover(&marker.repository_root) else {
                    continue;
                };
                let Ok(common_dir) = repository.git_common_dir() else {
                    continue;
                };
                let Some(owner_root) = common_dir.parent() else {
                    continue;
                };
                normalise(owner_root) == marker_root
                    && crate::host_state::repository_key(owner_root, Some(&common_dir))
                        == marker.repository_key
            };
            if !repository_key_matches
                || discovery_started.elapsed() >= WORKTREE_CACHE_ROOT_DISCOVERY_BUDGET
            {
                continue;
            }

            let binding =
                match crate::broker::lifecycle::capture_worktree_volume_preparation_cache_root(
                    placement_root,
                    &marker,
                    resource_db_path,
                ) {
                    Ok(Some(binding)) => binding,
                    Ok(None) | Err(_) => continue,
                };
            if binding.validate().is_err()
                || binding
                    .root_guard
                    .as_ref()
                    .and_then(|guard| guard.placement_root.as_ref())
                    .is_none()
            {
                continue;
            }
            bindings.push(binding);
        }
        bindings.sort_by(|left, right| left.path.cmp(&right.path));
        bindings.dedup_by(|left, right| left.path == right.path);
        bindings
    })
}

fn worktree_volume_cache_paths(
    bindings: &[crate::broker::PreparationCacheRootBinding],
) -> BTreeSet<PathBuf> {
    bindings
        .iter()
        .filter_map(|binding| {
            binding
                .root_guard
                .as_ref()?
                .placement_root
                .as_ref()
                .map(|placement_root| {
                    placement_root
                        .path
                        .join(crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT)
                })
        })
        .collect()
}

fn inspect_preparation_cache(
    cache_root: &Path,
    live_roots: &[PathBuf],
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
) -> (
    Vec<StoragePreparationEntry>,
    Vec<StoragePreparationCandidate>,
) {
    let mut live_keys = BTreeSet::new();
    let mut liveness_unknown = false;
    for root in live_roots {
        // `current_cache_key` reads config by path. Only a real directory can
        // supply a key. A regular file cannot contain a checkout, while a
        // symlink, special file, or missing path leaves liveness unknown.
        match std::fs::symlink_metadata(root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(metadata) if metadata.file_type().is_file() => continue,
            Ok(_) | Err(_) => {
                liveness_unknown = true;
                continue;
            }
        }
        match crate::preparation::current_cache_key(root) {
            Ok(Some(key)) => {
                live_keys.insert(key);
            }
            Ok(None) => {}
            Err(_) => liveness_unknown = true,
        }
    }

    let mut entries = Vec::new();
    let Ok(repositories) = std::fs::read_dir(cache_root) else {
        return (entries, Vec::new());
    };
    for repository in repositories.flatten() {
        let repository_path = repository.path();
        if !is_real_directory(&repository_path) {
            continue;
        }
        let repository_name = repository.file_name().to_string_lossy().into_owned();
        let Some((_, retired)) = preparation_cache_repository_identity(&repository_name) else {
            entries.push(retained_preparation_entry(
                repository_path,
                repository_name,
                String::new(),
                "repository directory name is not a managed preparation-cache identity; retained",
            ));
            continue;
        };
        if retired {
            let managed_layout = retired_preparation_store_layout(&repository_path, None);
            let estimated_bytes = managed_layout
                .then(|| observed_size(&repository_path, scan, records))
                .flatten();
            let estimated_inodes = managed_layout
                .then(|| observed_inodes(&repository_path, records))
                .flatten();
            entries.push(StoragePreparationEntry {
                path: repository_path,
                repository: repository_name,
                key: String::new(),
                estimated_bytes,
                estimated_inodes,
                reclaimable: managed_layout,
                reason: if managed_layout {
                    "retired preparation-cache store from a completed budget rotation".into()
                } else {
                    "retired preparation-cache store has an unknown layout; retained".into()
                },
            });
            continue;
        }
        let Ok(keys) = std::fs::read_dir(&repository_path) else {
            entries.push(retained_preparation_entry(
                repository_path,
                repository_name,
                String::new(),
                "preparation-cache keys could not be inspected; retained",
            ));
            continue;
        };
        for key_entry in keys.flatten() {
            let path = key_entry.path();
            let key = key_entry.file_name().to_string_lossy().into_owned();
            if !is_real_directory(&path) || !is_preparation_cache_key(&key) {
                entries.push(retained_preparation_entry(
                    path,
                    repository_name.clone(),
                    key,
                    "preparation-cache key path does not match the managed layout; retained",
                ));
                continue;
            }
            let live = liveness_unknown || live_keys.contains(&key);
            // Measuring is the expensive half, so a live entry is counted but
            // never walked: its size cannot change the decision.
            let estimated_bytes = if !live {
                observed_size(&path, scan, records)
            } else {
                None
            };
            let estimated_inodes = if !live {
                observed_inodes(&path, records)
            } else {
                None
            };
            let reason = if liveness_unknown {
                "checkout preparation liveness could not be verified; retaining cache entries"
                    .into()
            } else if live {
                "a checkout on this host still computes this key".into()
            } else {
                "no checkout on this host computes this key; its inputs have changed".into()
            };
            entries.push(StoragePreparationEntry {
                path,
                repository: repository_name.clone(),
                key,
                estimated_bytes,
                estimated_inodes,
                reclaimable: !live,
                reason,
            });
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let candidates = entries
        .iter()
        .filter(|entry| entry.reclaimable)
        .map(|entry| StoragePreparationCandidate {
            path: entry.path.clone(),
            repository: entry.repository.clone(),
            key: entry.key.clone(),
            estimated_bytes: entry.estimated_bytes,
            estimated_inodes: entry.estimated_inodes,
            reason: entry.reason.clone(),
        })
        .collect::<Vec<_>>();
    (entries, candidates)
}

fn retained_preparation_entry(
    path: PathBuf,
    repository: String,
    key: String,
    reason: &str,
) -> StoragePreparationEntry {
    StoragePreparationEntry {
        path,
        repository,
        key,
        estimated_bytes: None,
        estimated_inodes: None,
        reclaimable: false,
        reason: reason.into(),
    }
}

fn is_lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_preparation_cache_key(value: &str) -> bool {
    is_lowercase_hex(value, 12)
}

fn preparation_cache_repository_identity(name: &str) -> Option<(&str, bool)> {
    if is_lowercase_hex(name, 64) {
        return Some((name, false));
    }

    let (repository, retired_suffix) = name.split_once(".retired-")?;
    if !is_lowercase_hex(repository, 64) {
        return None;
    }
    let (timestamp_text, process_id_text) = retired_suffix.split_once('-')?;
    let timestamp = timestamp_text.parse::<u128>().ok()?;
    let process_id = process_id_text.parse::<u32>().ok()?;
    if process_id == 0
        || timestamp.to_string() != timestamp_text
        || process_id.to_string() != process_id_text
    {
        return None;
    }
    Some((repository, true))
}

fn retired_preparation_store_layout(path: &Path, deadline: Option<std::time::Instant>) -> bool {
    if !is_real_directory(path) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    let mut examined = 0_usize;
    for entry in entries {
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return false;
        }
        examined += 1;
        if examined > PREPARATION_RETIRED_STORE_MAX_KEYS {
            return false;
        }
        let Ok(entry) = entry else {
            return false;
        };
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        if file_type.is_symlink()
            || !file_type.is_dir()
            || !is_preparation_cache_key(&entry.file_name().to_string_lossy())
        {
            return false;
        }
    }
    deadline.is_none_or(|deadline| std::time::Instant::now() < deadline)
}

/// Remove a preparation-cache tree while revalidating its captured root
/// before every unlink and any witness restoration.
fn remove_preparation_cache_tree_with_binding_and_observer(
    path: &Path,
    deadline: Option<std::time::Instant>,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
    observer: &mut impl FnMut() -> std::io::Result<()>,
) -> Result<(TreeRemoval, usize), crate::gc::TreeRemovalError> {
    let validate_binding = || {
        if let Some(binding) = root_binding {
            binding.validate().map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("captured preparation-cache root changed: {error}"),
                )
            })?;
        }
        Ok(())
    };
    validate_binding().map_err(|source| crate::gc::TreeRemovalError {
        source,
        entries_reclaimed: 0,
    })?;
    let result = remove_condemned_tree_with_progress_and_guard(path, None, deadline, || {
        observer()?;
        validate_binding()
    });
    match result {
        Ok((outcome, entries_reclaimed)) => {
            validate_binding().map_err(|source| crate::gc::TreeRemovalError {
                source,
                entries_reclaimed,
            })?;
            Ok((outcome, entries_reclaimed))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
fn apply_preparation_candidate_with_lease_at(
    candidate: &StoragePreparationCandidate,
    live_roots: &[PathBuf],
    resource_db_path: &Path,
    cache_root: &Path,
) -> Result<u64, String> {
    apply_preparation_candidate_with_root_identity(
        candidate,
        live_roots,
        resource_db_path,
        cache_root,
        None,
    )
}

#[cfg(test)]
fn apply_preparation_candidate_with_root_identity(
    candidate: &StoragePreparationCandidate,
    live_roots: &[PathBuf],
    resource_db_path: &Path,
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
) -> Result<u64, String> {
    apply_preparation_candidate_with_optional_binding(
        candidate,
        live_roots,
        resource_db_path,
        cache_root,
        expected_root_identity,
        None,
    )
}

fn apply_preparation_candidate_with_binding(
    candidate: &StoragePreparationCandidate,
    live_roots: &[PathBuf],
    resource_db_path: &Path,
    binding: &crate::broker::PreparationCacheRootBinding,
) -> Result<u64, String> {
    binding
        .validate()
        .map_err(|error| format!("captured preparation-cache root changed: {error}"))?;
    apply_preparation_candidate_with_optional_binding(
        candidate,
        live_roots,
        resource_db_path,
        &binding.path,
        binding.identity.as_deref(),
        Some(binding),
    )
}

fn apply_preparation_candidate_with_optional_binding(
    candidate: &StoragePreparationCandidate,
    live_roots: &[PathBuf],
    resource_db_path: &Path,
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
) -> Result<u64, String> {
    apply_preparation_candidate_with_optional_binding_and_observer(
        candidate,
        live_roots,
        resource_db_path,
        cache_root,
        expected_root_identity,
        root_binding,
        &mut || Ok(()),
    )
}

#[cfg(test)]
fn apply_preparation_candidate_with_binding_and_observer(
    candidate: &StoragePreparationCandidate,
    live_roots: &[PathBuf],
    resource_db_path: &Path,
    binding: &crate::broker::PreparationCacheRootBinding,
    observer: &mut impl FnMut() -> std::io::Result<()>,
) -> Result<u64, String> {
    apply_preparation_candidate_with_optional_binding_and_observer(
        candidate,
        live_roots,
        resource_db_path,
        &binding.path,
        binding.identity.as_deref(),
        Some(binding),
        observer,
    )
}

fn apply_preparation_candidate_with_optional_binding_and_observer(
    candidate: &StoragePreparationCandidate,
    live_roots: &[PathBuf],
    resource_db_path: &Path,
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
    observer: &mut impl FnMut() -> std::io::Result<()>,
) -> Result<u64, String> {
    let deadline = std::time::Instant::now() + PREPARATION_APPLY_DEADLINE;
    let (lease_repository, retired) = validate_preparation_candidate_shape(candidate, cache_root)?;
    if !crate::preparation::preparation_cache_root_identity_matches(
        cache_root,
        expected_root_identity,
    ) {
        return Err("captured preparation-cache root changed; review a fresh storage plan".into());
    }
    let mut coordinator = None;
    let grant = acquire_preparation_cache_lease(
        &mut coordinator,
        resource_db_path,
        cache_root,
        lease_repository,
        deadline,
    )
    .map_err(|error| match error {
        PreparationCacheLeaseError::Busy => {
            "preparation cache is currently in use; no entry was removed".into()
        }
        PreparationCacheLeaseError::Deadline => {
            "preparation-cache lease deadline elapsed; no entry was removed".into()
        }
        PreparationCacheLeaseError::Unavailable(reason) => {
            format!("cannot safely acquire preparation-cache lease: {reason}")
        }
    })?;
    if let Some(binding) = root_binding {
        binding
            .validate()
            .map_err(|error| format!("captured preparation-cache root changed: {error}"))?;
    }
    let result = apply_preparation_candidate_under_lease(
        candidate,
        live_roots,
        cache_root,
        expected_root_identity,
        deadline,
        retired,
        root_binding,
        observer,
    );
    release_preparation_cache_lease(&mut coordinator, &grant);
    result
}

// The removal sequence keeps its validated roots, deadline, lease kind, and
// observer explicit so each safety check remains visible at the call site.
#[allow(clippy::too_many_arguments)]
fn apply_preparation_candidate_under_lease(
    candidate: &StoragePreparationCandidate,
    live_roots: &[PathBuf],
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    deadline: std::time::Instant,
    retired: bool,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
    observer: &mut impl FnMut() -> std::io::Result<()>,
) -> Result<u64, String> {
    if let Some(binding) = root_binding {
        binding
            .validate()
            .map_err(|error| format!("captured preparation-cache root changed: {error}"))?;
    }
    if !crate::preparation::preparation_cache_root_identity_matches(
        cache_root,
        expected_root_identity,
    ) {
        return Err("captured preparation-cache root changed; review a fresh storage plan".into());
    }
    let repository_path = cache_root.join(&candidate.repository);
    if retired {
        if candidate.path != repository_path
            || candidate.path.parent() != Some(cache_root)
            || !is_real_directory(cache_root)
            || !retired_preparation_store_layout(&candidate.path, Some(deadline))
        {
            return Err("retired preparation cache no longer has a managed layout".into());
        }
    } else {
        for root in live_roots {
            let metadata = std::fs::symlink_metadata(root).map_err(|error| {
                format!(
                    "cannot verify preparation liveness at {}: {error}",
                    root.display()
                )
            })?;
            if metadata.file_type().is_file() {
                // A regular file cannot be a checkout. Recheck its type at
                // apply time so a replacement symlink or special file refuses.
                continue;
            }
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(format!(
                    "cannot verify preparation liveness at {}; it is not a real directory",
                    root.display()
                ));
            }
            let key = crate::preparation::current_cache_key_before(root, Some(deadline))
                .map_err(|error| format!("cannot verify preparation liveness: {error}"))?;
            if key.as_deref() == Some(candidate.key.as_str()) {
                return Err("a checkout now computes this key again; it is no longer dead".into());
            }
        }
        if candidate.path.parent() != Some(repository_path.as_path())
            || candidate.path.file_name().and_then(|name| name.to_str())
                != Some(candidate.key.as_str())
            || !is_real_directory(cache_root)
            || !is_real_directory(&repository_path)
            || !is_real_directory(&candidate.path)
        {
            return Err("cache entry is no longer a directory".into());
        }
    }
    let usage = crate::disk_headroom::directory_usage_bounded(&candidate.path, deadline)
        .ok_or_else(|| "cache entry size could not be verified before the deadline".to_string())?;
    if check_preparation_sweep_deadline(deadline).is_err()
        || !is_real_directory(cache_root)
        || !crate::preparation::preparation_cache_root_identity_matches(
            cache_root,
            expected_root_identity,
        )
        || !is_real_directory(&candidate.path)
        || (!retired && !is_real_directory(&repository_path))
        || (retired && !retired_preparation_store_layout(&candidate.path, Some(deadline)))
    {
        return Err("cache entry changed before removal; review a fresh storage plan".into());
    }
    if let Some(binding) = root_binding {
        binding.validate().map_err(|error| {
            format!("captured preparation-cache root changed before removal: {error}")
        })?;
    }
    match remove_preparation_cache_tree_with_binding_and_observer(
        &candidate.path,
        Some(deadline),
        root_binding,
        observer,
    )
    .map(|(outcome, _entries_reclaimed)| outcome)
    .map_err(|error| format!("cannot remove cache entry: {}", error.source))?
    {
        TreeRemoval::Complete => Ok(usage.bytes),
        TreeRemoval::Interrupted => {
            Err("cache removal deadline elapsed; review a fresh storage plan".into())
        }
    }
}

fn validate_preparation_candidate_shape<'a>(
    candidate: &'a StoragePreparationCandidate,
    cache_root: &Path,
) -> Result<(&'a str, bool), String> {
    let (lease_repository, retired) = preparation_cache_repository_identity(&candidate.repository)
        .ok_or_else(|| {
            "preparation-cache repository identity is not a managed lowercase digest".to_string()
        })?;
    let repository_path = cache_root.join(&candidate.repository);
    if retired {
        if !candidate.key.is_empty() || candidate.path != repository_path {
            return Err("retired preparation-cache candidate has an invalid path shape".into());
        }
    } else if !is_preparation_cache_key(&candidate.key)
        || candidate.path.parent() != Some(repository_path.as_path())
        || candidate.path.file_name().and_then(|name| name.to_str()) != Some(candidate.key.as_str())
    {
        return Err(
            "preparation-cache candidate does not match the managed repository/key layout".into(),
        );
    }
    Ok((lease_repository, retired))
}

fn is_enrolled_primary_checkout(path: &Path) -> bool {
    path.join(".aethyme/config.toml").is_file()
}

fn inspect_primary_checkout(
    path: &Path,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
) -> StoragePrimaryCheckout {
    let path = normalise(path);
    let mut blockers = Vec::new();
    let repo = match GitRepo::discover(&path) {
        Ok(repo) => Some(repo),
        Err(error) => {
            blockers.push(format!("cannot inspect enrolled primary checkout: {error}"));
            None
        }
    };
    let repository_key = primary_repository_key(&path, repo.as_ref());
    let mut dirty_paths = Vec::new();
    let mut clean = false;
    let mut usable = false;
    let mut extras = Vec::new();

    if let Some(repo) = repo.as_ref() {
        match repo.main_root() {
            Ok(main_root) if normalise(&main_root) == path => usable = true,
            Ok(main_root) => blockers.push(format!(
                "enrollment path resolves to Git main checkout {}, not {}",
                main_root.display(),
                path.display()
            )),
            Err(error) => blockers.push(format!("cannot resolve primary checkout root: {error}")),
        }
        match repo.dirty_paths() {
            Ok(paths) => {
                dirty_paths = paths;
                clean = dirty_paths.is_empty();
                if !clean {
                    blockers.push(format!(
                        "checkout has uncommitted changes; refusing the whole checkout and no primary artifact is eligible ({} path(s))",
                        dirty_paths.len()
                    ));
                }
            }
            Err(error) => blockers.push(format!("cannot inspect checkout dirtiness: {error}")),
        }
        match crate::load_retention_policy(&path) {
            Ok(policy) => extras = policy.artefact_directories,
            Err(error) => blockers.push(format!(
                "cannot load this checkout's retention policy; configured artifact names are ignored: {error}"
            )),
        }
    }

    let mut artifacts = Vec::new();
    if usable {
        let Some(repo) = repo.as_ref() else {
            unreachable!("a usable primary checkout has a Git repository");
        };
        match std::fs::read_dir(&path) {
            Ok(entries) => {
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            blockers
                                .push(format!("cannot enumerate primary checkout entry: {error}"));
                            continue;
                        }
                    };
                    let raw_path = entry.path();
                    if !is_real_directory(&raw_path) {
                        continue;
                    }
                    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    if !is_artefact_directory_with_extras(&name, &extras) {
                        continue;
                    }
                    let path = normalise(&raw_path);
                    let ignored = repo.path_is_ignored(&name);
                    let tracked = match repo.is_tracked(&name) {
                        Ok(tracked) => tracked,
                        Err(error) => {
                            blockers.push(format!(
                                "cannot determine whether primary artifact {} is tracked: {error}",
                                path.display()
                            ));
                            true
                        }
                    };
                    // A session worktree can be proven idle because the broker
                    // owns its lifecycle. A primary checkout has no session and
                    // no close event, so the only honest evidence that nothing
                    // is building is that the tree itself has stopped moving. A
                    // running `cargo build` writes into `target/` continuously
                    // while leaving the checkout `clean`, because `target/` is
                    // git-ignored -- so cleanliness alone would license deleting
                    // a build out from under itself.
                    let busy = directory_modified_within(&path, PRIMARY_ARTIFACT_IDLE_MS);
                    let reclaimable = clean && ignored && !tracked && !busy;
                    let reason = if !clean {
                        "enrolled primary checkout is dirty; the whole checkout is refused".into()
                    } else if tracked {
                        "directory contains tracked files and is never a candidate".into()
                    } else if !ignored {
                        "directory is not ignored by Git and is never a candidate".into()
                    } else if busy {
                        "artifact changed recently; a build may be running against it".into()
                    } else {
                        "regenerable Git-ignored artifact in a clean enrolled primary checkout"
                            .into()
                    };
                    let estimated_bytes = observed_size(&path, scan, records);
                    let estimated_inodes = observed_inodes(&path, records);
                    artifacts.push(StoragePrimaryArtifact {
                        path,
                        name,
                        ignored,
                        tracked,
                        reclaimable,
                        estimated_bytes,
                        estimated_inodes,
                        reason,
                    });
                }
            }
            Err(error) => blockers.push(format!(
                "cannot enumerate enrolled primary checkout: {error}"
            )),
        }
    }
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    StoragePrimaryCheckout {
        path,
        repository_key,
        clean,
        dirty_paths,
        artifacts,
        blockers,
    }
}

fn primary_repository_key(path: &Path, repo: Option<&GitRepo>) -> String {
    let Some(repo) = repo else {
        return path.to_string_lossy().into_owned();
    };
    let Ok(common) = repo.git_common_dir() else {
        return path.to_string_lossy().into_owned();
    };
    crate::host_state::repository_key(path, Some(&common))
}

#[derive(Debug, Default)]
struct EntryBuilder {
    on_disk: bool,
    git_registered: bool,
    ledger_claimed: bool,
    git_marker: bool,
    session_ids: Vec<i64>,
    estimated_bytes: Option<u64>,
    estimated_inodes: Option<u64>,
}

impl EntryBuilder {
    fn finish(self, path: PathBuf) -> StorageEntry {
        let mut missing_from = Vec::new();
        if !self.on_disk {
            missing_from.push(StorageSource::Disk);
        }
        if !self.git_registered {
            missing_from.push(StorageSource::GitRegistration);
        }
        if !self.ledger_claimed {
            missing_from.push(StorageSource::SessionLedger);
        }
        let kind = self.on_disk.then_some(if self.git_marker {
            StorageDirectoryKind::Worktree
        } else {
            StorageDirectoryKind::StrayDirectory
        });
        StorageEntry {
            path,
            on_disk: self.on_disk,
            git_registered: self.git_registered,
            ledger_claimed: self.ledger_claimed,
            missing_from,
            kind,
            git_marker: self.git_marker,
            session_ids: self.session_ids,
            estimated_bytes: self.estimated_bytes,
            estimated_inodes: self.estimated_inodes,
        }
    }
}

fn candidates_for_root(
    root: &StorageRoot,
    orphan_worktree_roots_days: u32,
) -> Vec<StorageCandidate> {
    if root.filesystem_kind != StorageFilesystemKind::Directory
        || root.marker_status != StorageMarkerStatus::Valid
        || !root.blockers.is_empty()
    {
        return Vec::new();
    }
    let Some(repository_key) = root.repository_key.clone() else {
        return Vec::new();
    };
    let Some(repository_root) = root.repository_root.clone() else {
        return Vec::new();
    };
    let Some(marker_sha256) = root.marker_sha256.clone() else {
        return Vec::new();
    };
    if root.owner_exists == Some(false) {
        if root
            .age_days
            .is_some_and(|age| age >= orphan_worktree_roots_days)
        {
            return vec![StorageCandidate {
                kind: StorageCandidateKind::OrphanRoot,
                path: root.path.clone(),
                root_path: root.path.clone(),
                repository_key,
                repository_root,
                git_marker: false,
                estimated_bytes: root.estimated_bytes,
                estimated_inodes: root.estimated_inodes,
                reason: "owning repository no longer exists".into(),
                marker_sha256,
            }];
        }
        return Vec::new();
    }
    if root.owner_exists != Some(true) || !root.blockers.is_empty() {
        return Vec::new();
    }
    // Nothing but bookkeeping, and no session holds or keeps a worktree here:
    // the repository's last worktree is gone, and only a host-level sweep ever
    // looks at the root again. The grace period is the orphan one, which keeps
    // a root out of review while a `broker start` may still be filling it; the
    // next start rewrites the marker and build defaults if the root is gone.
    if root.empty && root.reconciliation.entries.is_empty() {
        if root
            .age_days
            .is_some_and(|age| age >= orphan_worktree_roots_days)
        {
            return vec![StorageCandidate {
                kind: StorageCandidateKind::EmptyRoot,
                path: root.path.clone(),
                root_path: root.path.clone(),
                repository_key,
                repository_root,
                git_marker: false,
                estimated_bytes: root.estimated_bytes,
                estimated_inodes: root.estimated_inodes,
                reason: "no worktree remains and no session keeps one here; the root holds only broker bookkeeping, which the next `broker start` rewrites".into(),
                marker_sha256,
            }];
        }
        return Vec::new();
    }
    root.reconciliation
        .entries
        .iter()
        .filter(|entry| entry.on_disk && !entry.git_registered && !entry.ledger_claimed)
        .map(|entry| StorageCandidate {
            kind: StorageCandidateKind::StrayDirectory,
            path: entry.path.clone(),
            root_path: root.path.clone(),
            repository_key: repository_key.clone(),
            repository_root: repository_root.clone(),
            git_marker: entry.git_marker,
            estimated_bytes: entry.estimated_bytes,
            estimated_inodes: entry.estimated_inodes,
            reason: if entry.git_marker {
                "directory has .git metadata but is absent from both Git worktree registrations and the session ledger".into()
            } else {
                "directory is absent from both Git worktree registrations and the session ledger".into()
            },
            marker_sha256: marker_sha256.clone(),
        })
        .collect()
}

/// Observe one path through the selected sizing policy.
///
/// The recorded path never touches the directory contents. A full storage
/// apply may still request a fresh walk, but the ordinary plan is deliberately
/// assembled from the shared measurement cache so a cheap inventory cannot
/// turn into a recursive scan of every checkout on the host.
fn observed_size(
    path: &Path,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
) -> Option<u64> {
    let key = path.to_string_lossy();
    if scan.measures() {
        let usage = crate::disk_headroom::directory_usage_without_following_links(path).ok()?;
        records.record_usage(&key, usage.bytes, Some(usage.inodes), now_ms());
        Some(usage.bytes)
    } else {
        records.get(&key).map(|record| record.bytes)
    }
}

fn observed_inodes(path: &Path, records: &crate::measurement::SizeRecords) -> Option<u64> {
    records
        .get(&path.to_string_lossy())
        .and_then(|record| record.inodes)
}

/// Spend one routine measurement budget on the oldest or never-measured
/// direct storage entry. A failed or timed-out walk records nothing, leaving
/// the corresponding estimate explicitly unknown for the next plan.
fn warm_one_storage_size_record(
    main_root: &Path,
    plan: &StoragePlan,
    policy: &crate::RetentionPolicy,
    records: &mut crate::measurement::SizeRecords,
) {
    let paths = storage_size_paths(plan);
    let Some(path) = records.next_to_measure(
        &paths,
        now_ms(),
        i64::from(policy.size_record_ttl_hours).saturating_mul(3_600_000),
    ) else {
        return;
    };
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(policy.routine_size_budget_ms);
    let Some(usage) = crate::disk_headroom::directory_usage_bounded(Path::new(&path), deadline)
    else {
        return;
    };
    records.record_usage(&path, usage.bytes, Some(usage.inodes), now_ms());
    let _ = crate::measurement::save_size_records(main_root, records);
}

fn storage_size_paths(plan: &StoragePlan) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for root in &plan.roots {
        for entry in &root.reconciliation.entries {
            if entry.on_disk {
                paths.insert(entry.path.to_string_lossy().into_owned());
            }
        }
    }
    for checkout in &plan.primary_checkouts {
        for artifact in &checkout.artifacts {
            paths.insert(artifact.path.to_string_lossy().into_owned());
        }
    }
    for entry in &plan.preparation_entries {
        if entry.reclaimable {
            paths.insert(entry.path.to_string_lossy().into_owned());
        }
    }
    paths.into_iter().collect()
}

fn summarise(
    roots: &[StorageRoot],
    candidates: &[StorageCandidate],
    primary_checkouts: &[StoragePrimaryCheckout],
    primary_candidates: &[StoragePrimaryCandidate],
    preparation_entries: &[StoragePreparationEntry],
    preparation_candidates: &[StoragePreparationCandidate],
) -> StorageSummary {
    let owner_present_count = roots
        .iter()
        .filter(|root| root.owner_exists == Some(true))
        .count();
    let owner_missing_count = roots
        .iter()
        .filter(|root| root.owner_exists == Some(false))
        .count();
    let on_disk_directory_count = roots.iter().map(|root| root.on_disk_directory_count).sum();
    let estimated_bytes = sized_sum(roots.iter().map(|root| root.estimated_bytes));
    let estimated_inodes = sized_sum(roots.iter().map(|root| root.estimated_inodes));
    let reclaimable_bytes = sized_sum(candidates.iter().map(|candidate| candidate.estimated_bytes));
    let reclaimable_inodes = sized_sum(
        candidates
            .iter()
            .map(|candidate| candidate.estimated_inodes),
    );
    let primary_reclaimable_bytes = sized_sum(
        primary_candidates
            .iter()
            .map(|candidate| candidate.estimated_bytes),
    );
    let primary_reclaimable_inodes = sized_sum(
        primary_candidates
            .iter()
            .map(|candidate| candidate.estimated_inodes),
    );
    StorageSummary {
        root_count: roots.len(),
        on_disk_directory_count,
        owner_present_count,
        owner_missing_count,
        orphan_root_count: candidates
            .iter()
            .filter(|candidate| candidate.kind == StorageCandidateKind::OrphanRoot)
            .count(),
        stray_directory_count: candidates
            .iter()
            .filter(|candidate| candidate.kind == StorageCandidateKind::StrayDirectory)
            .count(),
        empty_root_count: candidates
            .iter()
            .filter(|candidate| candidate.kind == StorageCandidateKind::EmptyRoot)
            .count(),
        candidate_count: candidates.len(),
        primary_checkout_count: primary_checkouts.len(),
        primary_artifact_count: primary_checkouts
            .iter()
            .map(|checkout| checkout.artifacts.len())
            .sum(),
        primary_candidate_count: primary_candidates.len(),
        preparation_entry_count: preparation_entries.len(),
        preparation_candidate_count: preparation_candidates.len(),
        preparation_reclaimable_bytes: sized_sum(
            preparation_candidates
                .iter()
                .map(|candidate| candidate.estimated_bytes),
        ),
        preparation_reclaimable_inodes: sized_sum(
            preparation_candidates
                .iter()
                .map(|candidate| candidate.estimated_inodes),
        ),
        broker_branch_ref_count: roots
            .iter()
            .flat_map(|root| &root.session_branches)
            .filter(|branch| branch.ownership == StorageBranchOwnership::SessionLedger)
            .count(),
        unclaimed_agent_branch_ref_count: roots
            .iter()
            .flat_map(|root| &root.session_branches)
            .filter(|branch| branch.ownership == StorageBranchOwnership::Unknown)
            .count(),
        incomplete_branch_inventory_root_count: roots
            .iter()
            .filter(|root| {
                root.marker_status == StorageMarkerStatus::Valid
                    && root.owner_exists == Some(true)
                    && !root.session_branch_inventory_complete
            })
            .count(),
        estimated_bytes,
        estimated_inodes,
        reclaimable_bytes,
        reclaimable_inodes,
        primary_reclaimable_bytes,
        primary_reclaimable_inodes,
        sized: roots.iter().all(|root| root.sized),
    }
}

fn sized_sum(values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    let mut total = 0_u64;
    for value in values {
        total = total.saturating_add(value?);
    }
    Some(total)
}

fn inspect_owner(owner: &Path, host_root: &Path) -> Result<OwnerSources, String> {
    let checkout = GitRepo::discover(owner)
        .map_err(|error| format!("owning repository cannot be inspected: {error}"))?;
    let discovered_main = checkout
        .main_root()
        .map_err(|error| format!("owning repository main root cannot be resolved: {error}"))?;
    if normalise(&discovered_main) != normalise(owner) {
        return Err(format!(
            "owning repository marker points at {}, but Git resolves its main checkout to {}; no stray directory is swept",
            owner.display(),
            discovered_main.display()
        ));
    }
    let worktrees = checkout
        .worktree_inventory()
        .map_err(|error| format!("Git worktree registrations cannot be inspected: {error}"))?;
    let mut git_registered = BTreeSet::new();
    let mut checked_out = BTreeMap::<String, Vec<PathBuf>>::new();
    for worktree in &worktrees {
        let path = normalise_absolute(&worktree.path, owner);
        if is_direct_child(&path, host_root) {
            git_registered.insert(path.clone());
        }
        if !worktree.prunable
            && worktree.path.exists()
            && let Some(branch) = worktree.branch.as_deref()
        {
            checked_out
                .entry(
                    branch
                        .strip_prefix("refs/heads/")
                        .unwrap_or(branch)
                        .to_string(),
                )
                .or_default()
                .push(path);
        }
    }

    let store_path = owner.join(crate::BROKER_DB_RELPATH);
    if !is_regular_file(&store_path) {
        return Err(format!(
            "session ledger is missing or is not a regular file at {}; no stray directory is swept",
            store_path.display()
        ));
    }
    let store = BrokerStore::open_snapshot_at_in_repo(owner, &store_path).map_err(|source| {
        format!(
            "owner-scoped session ledger and root binding cannot be inspected at {}: {source}; no root is swept",
            store_path.display()
        )
    })?;
    let worktree_root_binding = store
        .repository_worktree_binding()
        .map_err(|source| {
            format!(
                "repository worktree-root binding cannot be inspected at {}: {source}; no root is swept",
                store_path.display()
            )
        })?;
    let mut ledger_claimed = BTreeMap::<PathBuf, Vec<i64>>::new();
    let mut all_cleaned = BTreeMap::<PathBuf, bool>::new();
    let uncleared_sessions = store
        .live_sessions()
        .map_err(|error| format!("live session ledger cannot be read: {error}"))?;
    let mut sessions = uncleared_sessions.clone();
    sessions.extend(
        store
            .cleaned_sessions()
            .map_err(|error| format!("closed session ledger cannot be read: {error}"))?,
    );
    let mut session_ids_by_branch = BTreeMap::<String, Vec<i64>>::new();
    let mut uncleared_ids_by_branch = BTreeMap::<String, Vec<i64>>::new();
    for session in &sessions {
        let branch = session
            .branch
            .strip_prefix("refs/heads/")
            .unwrap_or(&session.branch);
        if !branch.is_empty() {
            session_ids_by_branch
                .entry(branch.to_string())
                .or_default()
                .push(session.id);
        }
    }
    for session in &uncleared_sessions {
        let branch = session
            .branch
            .strip_prefix("refs/heads/")
            .unwrap_or(&session.branch);
        if !branch.is_empty() {
            uncleared_ids_by_branch
                .entry(branch.to_string())
                .or_default()
                .push(session.id);
        }
    }
    for ids in session_ids_by_branch.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    for ids in uncleared_ids_by_branch.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    for paths in checked_out.values_mut() {
        paths.sort();
        paths.dedup();
    }
    // Resolved from the repository, never from a session record: an adopted
    // main checkout's ledger row names the default branch, and reporting it
    // as session-owned would invite a later cleanup to review it for removal.
    let mut shared_branches = vec![crate::merge::PromoteConfig::load(owner).branch];
    match checkout
        .upstream_default()
        .and_then(|(upstream, _)| upstream.split_once('/').map(|(_, b)| b.to_string()))
    {
        Some(branch) => shared_branches.push(branch),
        None => shared_branches.extend(["main".to_string(), "master".to_string()]),
    }
    let branch_tips = checkout.local_branch_tips();
    let session_branch_inventory_complete = branch_tips.is_some();
    let session_branches = branch_tips
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(reference, head)| {
            let name = reference.strip_prefix("refs/heads/")?;
            let session_ids = session_ids_by_branch.remove(name).unwrap_or_default();
            let ownership = if shared_branches.iter().any(|shared| shared == name) {
                if session_ids.is_empty() {
                    return None;
                }
                StorageBranchOwnership::Shared
            } else if session_ids.is_empty() {
                if !name.starts_with("agent/") {
                    return None;
                }
                StorageBranchOwnership::Unknown
            } else {
                StorageBranchOwnership::SessionLedger
            };
            let uncleared_session_ids = uncleared_ids_by_branch.remove(name).unwrap_or_default();
            let checked_out_paths = checked_out.remove(name).unwrap_or_default();
            let retention_reason = if ownership == StorageBranchOwnership::Shared {
                "the repository's integration or default branch; it is never session-owned or removable".into()
            } else if !uncleared_session_ids.is_empty() {
                "one or more sessions remain in the broker ledger; retain this branch until their lifecycle is resolved".into()
            } else if !checked_out_paths.is_empty() {
                "a registered Git worktree still checks out this branch; retain it while that checkout exists".into()
            } else if ownership == StorageBranchOwnership::Unknown {
                "the branch uses the broker agent namespace but has no ledger owner; ownership is unknown and requires review".into()
            } else {
                "a historical session record owns this ref, but delivery proof and cleanup eligibility must be reviewed before removal".into()
            };
            Some(StorageBranchEntry {
                name: name.to_string(),
                head,
                session_ids,
                uncleared_session_ids,
                checked_out_paths,
                ownership,
                retention_reason,
            })
        })
        .collect::<Vec<_>>();
    let mut session_branches = session_branches;
    session_branches.sort_by(|left, right| left.name.cmp(&right.name));
    for session in sessions {
        let path = normalise_absolute(Path::new(&session.worktree_path), owner);
        if is_direct_child(&path, host_root) {
            // One live or retained claim is enough to keep the path loud: a
            // directory can be reused by a later session.
            let cleaned = session.cleanup_state == crate::SessionCleanupState::Cleaned;
            *all_cleaned.entry(path.clone()).or_insert(true) &= cleaned;
            ledger_claimed.entry(path).or_default().push(session.id);
        }
    }
    for session_ids in ledger_claimed.values_mut() {
        session_ids.sort_unstable();
    }
    let retired = all_cleaned
        .into_iter()
        .filter_map(|(path, cleaned)| cleaned.then_some(path))
        .collect();
    Ok(OwnerSources {
        git_registered,
        ledger_claimed,
        session_branch_inventory_complete,
        session_branches,
        worktree_root_binding_state: worktree_root_binding.state,
        worktree_root_binding_path: worktree_root_binding.worktree_root,
        retired,
    })
}

fn apply_candidate(
    candidate: &StorageCandidate,
    storage_layout: &StorageRootLayout,
) -> Result<u64, String> {
    let storage_root = normalise(storage_layout.path());
    let candidate_root = normalise(&candidate.root_path);
    let current_marker_root = match candidate.kind {
        StorageCandidateKind::OrphanRoot | StorageCandidateKind::EmptyRoot => {
            candidate.path.as_path()
        }
        StorageCandidateKind::StrayDirectory => candidate.root_path.as_path(),
        StorageCandidateKind::PrimaryArtifact => {
            return Err("primary artifacts use the primary-checkout apply lane".into());
        }
        StorageCandidateKind::PreparationEntry => {
            return Err("preparation cache entries use the preparation apply lane".into());
        }
    };
    let root_matches_layout = if storage_layout.is_direct() {
        candidate_root == storage_root
    } else {
        is_direct_child(&candidate_root, &storage_root)
    };
    if !root_matches_layout {
        return Err("reviewed root is outside the selected storage layout".into());
    }
    if candidate.kind == StorageCandidateKind::StrayDirectory
        && !is_direct_child(
            &normalise(&candidate.path),
            &normalise(&candidate.root_path),
        )
    {
        return Err("stray directory is not a direct child of its reviewed root".into());
    }
    // Session creation/adoption/removal use this same repository-wide lock.
    // Take it before rechecking ownership and the root marker so a start that
    // has passed its own final claim cannot race the last bookkeeping unlink.
    // Stray checkouts need the same exclusion: worktree-add may be between
    // creating its `.git` pointer and registering the checkout in Git.
    let _lifecycle_claim = match candidate.kind {
        StorageCandidateKind::EmptyRoot | StorageCandidateKind::StrayDirectory => Some(
            crate::file_lock::acquire_repository_lifecycle_lock(&candidate.repository_root)
                .map_err(|error| format!("cannot acquire repository lifecycle lock: {error}"))?,
        ),
        _ => None,
    };
    let observation = read_marker(current_marker_root);
    let Some(marker) = observation.marker else {
        return Err("ownership marker disappeared or became unreadable".into());
    };
    let Some(marker_bytes) = observation.bytes.as_deref() else {
        return Err("ownership marker could not be hashed".into());
    };
    let observed_digest = format!("{:x}", Sha256::digest(marker_bytes));
    if observed_digest != candidate.marker_sha256
        || marker.repository_key != candidate.repository_key
        || marker.repository_root != candidate.repository_root
    {
        return Err("ownership marker changed since the reviewed plan".into());
    }
    match candidate.kind {
        StorageCandidateKind::OrphanRoot => {
            if is_real_directory(&candidate.repository_root) {
                return Err("owning repository reappeared".into());
            }
            remove_candidate_tree(&candidate.path, Some(WORKTREE_ROOT_MARKER))
        }
        StorageCandidateKind::StrayDirectory => {
            if has_git_marker(&candidate.path) != candidate.git_marker {
                return Err("stray directory Git marker changed since the reviewed plan".into());
            }
            if !is_real_directory(&candidate.repository_root) {
                return Err("owning repository disappeared; review a new plan".into());
            }
            let sources = inspect_owner(&candidate.repository_root, &candidate.root_path)?;
            let path = normalise(&candidate.path);
            if sources.git_registered.contains(&path) {
                return Err("Git worktree registration appeared".into());
            }
            if sources.ledger_claimed.contains_key(&path) {
                return Err("session ledger claim appeared".into());
            }
            if !is_real_directory(&candidate.path) {
                return Err("stray directory disappeared or is no longer a real directory".into());
            }
            remove_candidate_tree(&candidate.path, None)
        }
        StorageCandidateKind::EmptyRoot => {
            if !is_real_directory(&candidate.repository_root) {
                return Err("owning repository disappeared; review a new plan".into());
            }
            let sources = inspect_owner(&candidate.repository_root, &candidate.path)?;
            if let Some(reason) = sources.empty_root_retention_reason(&candidate.path) {
                return Err(format!(
                    "durable worktree-root binding recheck refused removal: {reason}"
                ));
            }
            if !sources.git_registered.is_empty() {
                return Err("Git worktree registration appeared".into());
            }
            if sources.has_unretired_claim() {
                return Err("a session now claims a worktree under this root".into());
            }
            remove_empty_root(&candidate.path)
        }
        StorageCandidateKind::PrimaryArtifact => {
            unreachable!("primary artifacts are rejected before host-root matching")
        }
        StorageCandidateKind::PreparationEntry => {
            Err("preparation cache entries use the preparation apply lane".into())
        }
    }
}

fn apply_primary_candidate(candidate: &StoragePrimaryCandidate) -> Result<u64, String> {
    if !is_enrolled_primary_checkout(&candidate.checkout_path) {
        return Err("primary checkout is no longer enrolled".into());
    }
    let repo = GitRepo::discover(&candidate.checkout_path)
        .map_err(|error| format!("cannot inspect primary checkout: {error}"))?;
    let main_root = repo
        .main_root()
        .map_err(|error| format!("cannot resolve primary checkout root: {error}"))?;
    if normalise(&main_root) != normalise(&candidate.checkout_path) {
        return Err("primary checkout no longer resolves to its reviewed Git main root".into());
    }
    let dirty_paths = repo
        .dirty_paths()
        .map_err(|error| format!("cannot inspect primary checkout dirtiness: {error}"))?;
    if !dirty_paths.is_empty() {
        return Err(format!(
            "primary checkout is dirty; refusing the whole checkout ({} path(s))",
            dirty_paths.len()
        ));
    }
    let name = candidate.name.strip_prefix("./").unwrap_or(&candidate.name);
    if name.is_empty() {
        return Err("primary artifact name is empty".into());
    }
    let current_policy = crate::load_retention_policy(&candidate.checkout_path)
        .map_err(|error| format!("cannot load primary checkout retention policy: {error}"))?;
    if !is_artefact_directory_with_extras(name, &current_policy.artefact_directories) {
        return Err("primary artifact is no longer in the configured regenerable catalog".into());
    }
    if !repo.path_is_ignored(name) {
        return Err("primary artifact is no longer ignored by Git".into());
    }
    if repo
        .is_tracked(name)
        .map_err(|error| format!("cannot determine whether primary artifact is tracked: {error}"))?
    {
        return Err("primary artifact contains tracked files".into());
    }
    if !is_direct_child(
        &normalise(&candidate.path),
        &normalise(&candidate.checkout_path),
    ) {
        return Err("primary artifact is not a direct child of its reviewed checkout".into());
    }
    remove_candidate_tree(&candidate.path, None)
}

fn remove_candidate_tree(path: &Path, witness: Option<&str>) -> Result<u64, String> {
    if !is_real_directory(path) {
        return Err("candidate is no longer a real directory".into());
    }
    let tree = PinnedTreeRemoval::open(path).map_err(|error| error.to_string())?;
    // Reporting-only logical lengths observed by the guarded unlink walk;
    // this does not claim physical disk space recovered or require a prewalk.
    match tree.remove_with_observed_file_bytes(witness, None) {
        Ok((TreeRemoval::Complete, bytes)) => Ok(bytes),
        Ok((TreeRemoval::Interrupted, _)) => Err("directory removal was interrupted".into()),
        Err(error) => Err(error.source.to_string()),
    }
}

/// Whether a dotted root entry is something the broker itself wrote there.
///
/// Exact matches only. An operator who edited the build defaults, or anything
/// else that left a file here, has put content in the root that no plan was
/// written to judge, so the root stops being empty.
fn is_broker_bookkeeping(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name == WORKTREE_ROOT_MARKER
        || name == ".DS_Store"
        || (name.starts_with(".aethyme-reclaim-plan-") && name.ends_with(".json"))
    {
        return is_regular_file(path);
    }
    if name != ".cargo" || !is_real_directory(path) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.flatten().all(|entry| {
        let child = entry.path();
        match entry.file_name().to_str() {
            Some(".DS_Store") => is_regular_file(&child),
            Some("config.toml") => {
                is_regular_file(&child)
                    && std::fs::read(&child)
                        .is_ok_and(|bytes| bytes == crate::broker::WORKTREE_CARGO_CONFIG.as_bytes())
            }
            _ => false,
        }
    })
}

/// Remove a reviewed empty root, re-proving that it still holds only broker
/// bookkeeping.
///
/// Entry by entry rather than as a tree, and the directory itself with
/// `remove_dir`: a worktree a concurrent `broker start` adds after the check
/// makes that final step fail rather than be deleted with the root. The marker
/// goes last so an interrupted removal leaves a root that still names its owner.
fn remove_empty_root(root: &Path) -> Result<u64, String> {
    if !is_real_directory(root) {
        return Err("root is no longer a real directory".into());
    }
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("cannot enumerate root: {error}"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot enumerate root: {error}"))?;
    if let Some(unexpected) = entries.iter().find(|path| !is_broker_bookkeeping(path)) {
        return Err(format!(
            "root now holds {}, which is not broker bookkeeping",
            unexpected.display()
        ));
    }
    let mut freed = 0_u64;
    let mut remove_file = |path: &Path| -> Result<(), String> {
        freed += std::fs::symlink_metadata(path).map_or(0, |metadata| metadata.len());
        std::fs::remove_file(path).map_err(|error| format!("{}: {error}", path.display()))
    };
    let (marker, rest): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|path| path.file_name() == Some(WORKTREE_ROOT_MARKER.as_ref()));
    for path in rest {
        if is_real_directory(&path) {
            for child in std::fs::read_dir(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?
                .flatten()
            {
                remove_file(&child.path())?;
            }
            std::fs::remove_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        } else {
            remove_file(&path)?;
        }
    }
    for path in marker {
        remove_file(&path)?;
    }
    std::fs::remove_dir(root).map_err(|error| format!("{}: {error}", root.display()))?;
    Ok(freed)
}

fn storage_layout_with_context(
    main_root: &Path,
    context: &crate::broker::BrokerStorageContext,
) -> Result<StorageRootLayout, StorageError> {
    let repository = GitRepo::discover(main_root)?;
    let git_common_dir = repository.git_common_dir()?;
    let plan = crate::broker::lifecycle::plan_worktree_root_with_context(
        main_root,
        Some(&git_common_dir),
        context,
    );
    storage_layout_from_worktree_root_plan(&plan)
}

fn storage_layout_from_worktree_root_plan(
    plan: &crate::broker::WorktreeRootPlan,
) -> Result<StorageRootLayout, StorageError> {
    if let Some(reason) = &plan.storage_context_refusal {
        return Err(StorageError::Unavailable {
            reason: reason.clone(),
        });
    }

    let layout = match plan.preferred_source {
        Some(crate::broker::WorktreeRootSource::LibraryOverride) => plan
            .preferred_root
            .as_ref()
            .map(|root| StorageRootLayout::DirectRoot(root.clone())),
        Some(crate::broker::WorktreeRootSource::RepositoryConfig)
            if plan.preferred_unavailable_reason.is_some() =>
        {
            plan.host_state_fallback_root
                .as_deref()
                .and_then(Path::parent)
                .map(|root| StorageRootLayout::KeyedContainer(root.to_path_buf()))
        }
        Some(
            crate::broker::WorktreeRootSource::RepositoryConfig
            | crate::broker::WorktreeRootSource::EnvironmentOverride
            | crate::broker::WorktreeRootSource::HostState,
        ) => plan
            .root_container
            .as_ref()
            .map(|root| StorageRootLayout::KeyedContainer(root.clone())),
        Some(crate::broker::WorktreeRootSource::RepositoryFallback) => plan
            .preferred_root
            .as_ref()
            .map(|root| StorageRootLayout::KeyedContainer(root.clone())),
        None => None,
    };
    let Some(layout) = layout else {
        let reason = if let Some(unavailable) = &plan.preferred_unavailable_reason {
            format!(
                "the configured worktree root is unavailable and no captured host-state fallback is available: {unavailable}; set AETHYME_WORKTREE_ROOT or AETHYME_HOST_STATE_DIR explicitly"
            )
        } else {
            "no eligible captured host-state directory is available; set AETHYME_WORKTREE_ROOT or AETHYME_HOST_STATE_DIR explicitly".to_string()
        };
        return Err(StorageError::Unavailable { reason });
    };

    if path_is_inside(&plan.repository_root, layout.path()) {
        return Err(StorageError::Unavailable {
            reason: format!(
                "{} resolves inside repository {}; choose an external host storage root",
                layout.path().display(),
                plan.repository_root.display()
            ),
        });
    }
    Ok(layout)
}

/// Return the roots represented by a selected storage layout. Keyed
/// containers enumerate their repository children; a direct root is itself
/// the one repository root and its children are sessions beneath that root.
fn storage_root_entries(
    storage_root: &Path,
    layout: &StorageRootLayout,
) -> Result<Vec<PathBuf>, StorageError> {
    storage_root_entries_with_limit(storage_root, layout, STORAGE_ROOT_MAX_ENTRIES)
}

fn storage_root_entries_with_limit(
    storage_root: &Path,
    layout: &StorageRootLayout,
    max_entries: usize,
) -> Result<Vec<PathBuf>, StorageError> {
    match std::fs::symlink_metadata(storage_root) {
        Ok(metadata) if metadata.is_dir() => {
            if layout.is_direct() {
                return Ok(vec![storage_root.to_path_buf()]);
            }
            let entries = std::fs::read_dir(storage_root).map_err(|source| StorageError::Io {
                path: storage_root.to_path_buf(),
                source,
            })?;
            let mut paths = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|source| StorageError::Io {
                    path: storage_root.to_path_buf(),
                    source,
                })?;
                if paths.len() >= max_entries {
                    return Err(StorageError::Unavailable {
                        reason: "storage-root inventory exceeded its entry bound".into(),
                    });
                }
                paths.push(entry.path());
            }
            paths.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
            Ok(paths)
        }
        Ok(_) => Err(StorageError::Unavailable {
            reason: format!("{} is not a directory", storage_root.display()),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(StorageError::Io {
            path: storage_root.to_path_buf(),
            source,
        }),
    }
}

fn host_policy(main_root: &Path) -> (crate::RetentionPolicy, Vec<String>) {
    match crate::load_retention_policy(main_root) {
        Ok(policy) => (policy, Vec::new()),
        Err(error) => (
            crate::RetentionPolicy::default(),
            vec![format!(
                "could not load the invoking repository's retention policy; using the default {} day orphan grace period: {error}",
                crate::RetentionPolicy::default().orphan_worktree_roots_days
            )],
        ),
    }
}

fn read_marker(root: &Path) -> MarkerObservation {
    let path = root.join(WORKTREE_ROOT_MARKER);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return MarkerObservation {
                status: StorageMarkerStatus::Missing,
                marker: None,
                bytes: None,
                error: Some(format!("{} is missing", path.display())),
            };
        }
        Err(error) => {
            return MarkerObservation {
                status: StorageMarkerStatus::Invalid,
                marker: None,
                bytes: None,
                error: Some(format!("cannot inspect {}: {error}", path.display())),
            };
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return MarkerObservation {
            status: StorageMarkerStatus::Invalid,
            marker: None,
            bytes: None,
            error: Some(format!("{} is not a regular file", path.display())),
        };
    }
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return MarkerObservation {
                status: StorageMarkerStatus::Invalid,
                marker: None,
                bytes: None,
                error: Some(format!("cannot read {}: {error}", path.display())),
            };
        }
    };
    let marker = match serde_json::from_slice::<WorktreeRootMarker>(&bytes) {
        Ok(marker)
            if marker.schema_version == WORKTREE_ROOT_SCHEMA_VERSION
                && !marker.repository_key.is_empty()
                && marker.repository_root.is_absolute() =>
        {
            marker
        }
        Ok(marker) => {
            return MarkerObservation {
                status: StorageMarkerStatus::Invalid,
                marker: None,
                bytes: Some(bytes),
                error: Some(format!(
                    "{} has invalid ownership-marker fields (schema {}, key {:?}, repository root {})",
                    path.display(),
                    marker.schema_version,
                    marker.repository_key,
                    marker.repository_root.display()
                )),
            };
        }
        Err(error) => {
            return MarkerObservation {
                status: StorageMarkerStatus::Invalid,
                marker: None,
                bytes: Some(bytes),
                error: Some(format!("{} is unreadable: {error}", path.display())),
            };
        }
    };
    MarkerObservation {
        status: StorageMarkerStatus::Valid,
        marker: Some(marker),
        bytes: Some(bytes),
        error: None,
    }
}

pub const STORAGE_ATTRIBUTION_SCHEMA_VERSION: u32 = 1;

/// Whether one host worktree root with no ownership marker can be proved to
/// belong to the invoking repository, and whether this run recorded it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageAttribution {
    pub root: PathBuf,
    pub attributable: bool,
    /// True only when this run wrote the marker.
    pub marked: bool,
    /// Git worktrees under the root that this repository has registered.
    pub registered_worktree_count: usize,
    pub reason: String,
}

/// `broker gc storage attribute`: the host roots with no ownership marker,
/// judged against the invoking repository (#257).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageAttributionReport {
    pub schema_version: u32,
    pub storage_root: PathBuf,
    pub repository_root: PathBuf,
    pub repository_key: String,
    /// Whether markers were written (`--apply`) or only reported.
    pub applied: bool,
    pub roots: Vec<StorageAttribution>,
}

/// The invoking repository as attribution evidence: its host-storage key and
/// every worktree path its Git registrations name.
struct ThisRepository {
    main_root: PathBuf,
    key: String,
    registered: BTreeSet<PathBuf>,
}

impl ThisRepository {
    fn observe(main_root: &Path) -> Result<Self, StorageError> {
        let repo = GitRepo::discover(main_root)?;
        let key = crate::host_state::repository_key(main_root, Some(&repo.git_common_dir()?));
        let registered = repo
            .worktree_paths()?
            .iter()
            .map(|path| normalise(path))
            .collect();
        Ok(Self {
            main_root: normalise(main_root),
            key,
            registered,
        })
    }
}

enum Attribution {
    ThisRepository { registered: usize },
    Unattributable(String),
}

/// Prove ownership from Git, never from a guess. A root belongs to this
/// repository when every Git worktree directly under it is one this
/// repository registered, and there is at least one, or when its name is
/// exactly the key this repository's broker would create it under. A single
/// worktree registered elsewhere, or none at all under a foreign name, leaves
/// it unattributable: a marker is what later lets cleanup remove strays, so
/// writing one on weaker evidence would authorize removing someone else's
/// directories.
fn attribute_root(root: &Path, this: &ThisRepository) -> Attribution {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Attribution::Unattributable("its entries cannot be listed".into());
    };
    let mut worktrees = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if is_real_directory(&path)
            && !is_infrastructure(&entry.file_name())
            && has_git_marker(&path)
        {
            worktrees.push(normalise(&path));
        }
    }
    worktrees.sort();
    let foreign: Vec<&PathBuf> = worktrees
        .iter()
        .filter(|path| !this.registered.contains(*path))
        .collect();
    if let Some(first) = foreign.first() {
        return Attribution::Unattributable(format!(
            "{} of its {} Git worktrees {} not registered in this repository (first: {})",
            foreign.len(),
            worktrees.len(),
            if foreign.len() == 1 { "is" } else { "are" },
            first.display()
        ));
    }
    let named_for_this = root
        .file_name()
        .is_some_and(|name| name.to_string_lossy() == this.key);
    if worktrees.is_empty() && !named_for_this {
        return Attribution::Unattributable(
            "it holds no Git worktree and its name is not this repository's key, so nothing ties it to a repository".into(),
        );
    }
    Attribution::ThisRepository {
        registered: worktrees.len(),
    }
}

/// Report, and with `apply` record, which unmarked host worktree roots belong
/// to the repository containing `path_inside_repo`.
///
/// Writing a marker removes nothing. It lets the reviewed, digest-confirmed
/// `gc storage` plan reconcile the root against this repository's Git
/// registrations and session ledger, as it does every root the broker created
/// since markers existed. The proof is re-taken immediately before each
/// write, and an existing marker is never replaced.
pub fn storage_attribute(
    path_inside_repo: &Path,
    apply: bool,
) -> Result<StorageAttributionReport, StorageError> {
    let context = capture_storage_context_for_path(path_inside_repo)?;
    storage_attribute_with_context(path_inside_repo, apply, &context)
}

pub(crate) fn storage_attribute_with_context(
    path_inside_repo: &Path,
    apply: bool,
    context: &crate::broker::BrokerStorageContext,
) -> Result<StorageAttributionReport, StorageError> {
    let checkout = GitRepo::discover(path_inside_repo)?;
    let main_root = checkout.main_root()?;
    validate_storage_context(&main_root, context)?;
    let storage_layout = storage_layout_with_context(&main_root, context)?;
    let storage_root = normalise(&absolute_path(&main_root, storage_layout.path()));
    let this = ThisRepository::observe(&main_root)?;
    let paths = storage_root_entries(&storage_root, &storage_layout)?;
    let mut roots = Vec::new();
    for path in paths {
        // Only an unmarked root is in question. A checkout sitting where a
        // root belongs is reported by the storage plan, not attributed.
        if !is_real_directory(&path)
            || is_infrastructure(path.file_name().unwrap_or_default())
            || has_git_marker(&path)
        {
            continue;
        }
        let path = normalise(&path);
        let observation = read_marker(&path);
        if observation.status == StorageMarkerStatus::Valid {
            continue;
        }
        if observation.status == StorageMarkerStatus::Invalid {
            roots.push(StorageAttribution {
                root: path,
                attributable: false,
                marked: false,
                registered_worktree_count: 0,
                reason: format!(
                    "{}; an existing marker is never replaced, so repair or remove it by hand",
                    observation
                        .error
                        .unwrap_or_else(|| "its ownership marker is invalid".into())
                ),
            });
            continue;
        }
        let attribution = match attribute_root(&path, &this) {
            Attribution::ThisRepository { registered } => {
                let reason = if registered == 0 {
                    "its name is this repository's worktree-root key".to_string()
                } else {
                    format!(
                        "all {registered} of its Git {} registered in this repository",
                        if registered == 1 {
                            "worktree is"
                        } else {
                            "worktrees are"
                        }
                    )
                };
                let marked = if apply {
                    write_attributed_marker(&path, &this).map_err(|source| StorageError::Io {
                        path: path.join(WORKTREE_ROOT_MARKER),
                        source,
                    })?
                } else {
                    false
                };
                StorageAttribution {
                    root: path,
                    attributable: true,
                    marked,
                    registered_worktree_count: registered,
                    reason,
                }
            }
            Attribution::Unattributable(reason) => StorageAttribution {
                root: path,
                attributable: false,
                marked: false,
                registered_worktree_count: 0,
                reason,
            },
        };
        roots.push(attribution);
    }
    Ok(StorageAttributionReport {
        schema_version: STORAGE_ATTRIBUTION_SCHEMA_VERSION,
        storage_root,
        repository_root: this.main_root.clone(),
        repository_key: this.key.clone(),
        applied: apply,
        roots,
    })
}

/// Write the marker the broker would have written when it created `root`.
/// `Ok(false)` when a marker appeared in the meantime: it is left untouched,
/// because the hard link that publishes the new one never replaces a file.
fn write_attributed_marker(root: &Path, this: &ThisRepository) -> std::io::Result<bool> {
    // Re-take the proof at the moment of writing: a worktree registered
    // elsewhere may have been added since the report was read.
    if !matches!(
        attribute_root(root, this),
        Attribution::ThisRepository { .. }
    ) {
        return Ok(false);
    }
    let marker = WorktreeRootMarker {
        schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
        repository_key: this.key.clone(),
        repository_root: this.main_root.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&marker).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    let temporary = root.join(format!(
        ".aethyme-worktree-root.{}.{}.tmp",
        std::process::id(),
        now_ms()
    ));
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    let published = crate::host_state::protect_host_state_path(&temporary, false)
        .and_then(|()| std::fs::hard_link(&temporary, root.join(WORKTREE_ROOT_MARKER)));
    let _ = std::fs::remove_file(&temporary);
    match published {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy, Default)]
struct DecisionRootMetadata<'a> {
    storage_root_layout: Option<&'static str>,
    preparation_cache_root_identity: Option<&'a [u8]>,
    preparation_cache_root_nonce: Option<&'a str>,
    worktree_volume_cache_bindings: &'a [crate::broker::PreparationCacheRootBinding],
    additional_worktree_volume_cache_bindings: &'a [crate::broker::PreparationCacheRootBinding],
}

fn decision_digest(
    storage_root: &Path,
    root_metadata: DecisionRootMetadata<'_>,
    orphan_worktree_roots_days: u32,
    candidates: &[StorageCandidate],
    primary_candidates: &[StoragePrimaryCandidate],
    preparation_candidates: &[StoragePreparationCandidate],
) -> String {
    #[derive(serde::Serialize)]
    struct DecisionCandidate<'a> {
        kind: StorageCandidateKind,
        path: &'a Path,
        root_path: &'a Path,
        repository_key: &'a str,
        repository_root: &'a Path,
        git_marker: bool,
        marker_sha256: &'a str,
    }
    #[derive(serde::Serialize)]
    struct Decision<'a> {
        storage_root: &'a Path,
        #[serde(skip_serializing_if = "Option::is_none")]
        storage_root_layout: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        preparation_cache_root_identity: Option<&'a [u8]>,
        #[serde(skip_serializing_if = "Option::is_none")]
        preparation_cache_root_nonce: Option<&'a str>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        worktree_volume_cache_roots: Vec<DecisionWorktreeVolumeCacheRoot<'a>>,
        orphan_worktree_roots_days: u32,
        candidates: Vec<DecisionCandidate<'a>>,
        primary_candidates: Vec<DecisionPrimaryCandidate<'a>>,
        preparation_candidates: Vec<DecisionPreparationCandidate<'a>>,
    }
    #[derive(serde::Serialize)]
    struct DecisionPreparationCandidate<'a> {
        path: &'a Path,
        repository: &'a str,
        key: &'a str,
    }
    #[derive(serde::Serialize)]
    struct DecisionPrimaryCandidate<'a> {
        path: &'a Path,
        checkout_path: &'a Path,
        repository_key: &'a str,
        name: &'a str,
    }
    #[derive(serde::Serialize)]
    struct DecisionWorktreeVolumeCacheRoot<'a> {
        placement_root: &'a Path,
        placement_identity: &'a [u8],
        repository_key: &'a str,
        repository_root: &'a Path,
        cache_root: &'a Path,
        cache_root_identity: &'a [u8],
        cache_namespace: &'a Path,
        cache_namespace_identity: Option<&'a [u8]>,
        cache_namespace_nonce: Option<&'a str>,
    }
    let worktree_volume_cache_roots = root_metadata
        .worktree_volume_cache_bindings
        .iter()
        .chain(
            root_metadata
                .additional_worktree_volume_cache_bindings
                .iter(),
        )
        .filter_map(|binding| {
            let guard = binding.root_guard.as_ref()?;
            let placement = guard.placement_root.as_ref()?;
            Some(DecisionWorktreeVolumeCacheRoot {
                placement_root: &placement.path,
                placement_identity: &placement.identity,
                repository_key: &placement.marker.repository_key,
                repository_root: &placement.marker.repository_root,
                cache_root: &guard.root_path,
                cache_root_identity: &guard.root_identity,
                cache_namespace: &binding.path,
                cache_namespace_identity: binding.identity.as_deref(),
                cache_namespace_nonce: binding.instance_nonce.as_deref(),
            })
        })
        .collect();
    let bytes = serde_json::to_vec(&Decision {
        storage_root,
        storage_root_layout: root_metadata.storage_root_layout,
        preparation_cache_root_identity: root_metadata.preparation_cache_root_identity,
        preparation_cache_root_nonce: root_metadata.preparation_cache_root_nonce,
        worktree_volume_cache_roots,
        orphan_worktree_roots_days,
        candidates: candidates
            .iter()
            .map(|candidate| DecisionCandidate {
                kind: candidate.kind,
                path: &candidate.path,
                root_path: &candidate.root_path,
                repository_key: &candidate.repository_key,
                repository_root: &candidate.repository_root,
                git_marker: candidate.git_marker,
                marker_sha256: &candidate.marker_sha256,
            })
            .collect(),
        preparation_candidates: preparation_candidates
            .iter()
            .map(|candidate| DecisionPreparationCandidate {
                path: &candidate.path,
                repository: &candidate.repository,
                key: &candidate.key,
            })
            .collect(),
        primary_candidates: primary_candidates
            .iter()
            .map(|candidate| DecisionPrimaryCandidate {
                path: &candidate.path,
                checkout_path: &candidate.checkout_path,
                repository_key: &candidate.repository_key,
                name: &candidate.name,
            })
            .collect(),
    })
    .expect("storage decision digest inputs are serializable");
    format!("{:x}", Sha256::digest(bytes))
}

fn empty_reconciliation() -> StorageReconciliation {
    StorageReconciliation {
        schema_version: STORAGE_RECONCILIATION_SCHEMA_VERSION,
        on_disk_count: 0,
        git_registered_count: 0,
        ledger_claimed_count: 0,
        retired_count: 0,
        entries: Vec::new(),
    }
}

fn filesystem_kind(path: &Path) -> StorageFilesystemKind {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return StorageFilesystemKind::Other;
    };
    if metadata.file_type().is_symlink() {
        StorageFilesystemKind::Symlink
    } else if metadata.is_dir() {
        StorageFilesystemKind::Directory
    } else if metadata.is_file() {
        StorageFilesystemKind::File
    } else {
        StorageFilesystemKind::Other
    }
}

fn has_git_marker(path: &Path) -> bool {
    std::fs::symlink_metadata(path.join(".git")).is_ok()
}

/// Name a checkout that sits where a worktree root belongs, and say what it
/// holds, so the operator can publish its work before moving or removing it.
fn foreign_worktree_blocker(path: &Path) -> String {
    let branch = crate::git::GitRepo::discover(path)
        .ok()
        .and_then(|checkout| checkout.current_branch().ok())
        .unwrap_or_else(|| "an unknown branch".into());
    format!(
        "{} is a Git worktree on {branch} placed directly in the worktree container, not a worktree root; it is never swept. Publish any work it holds, then `git worktree move` it under its repository's root or `git worktree remove` it",
        path.display()
    )
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn is_real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

fn is_infrastructure(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn is_direct_child(path: &Path, root: &Path) -> bool {
    let path = normalise(path);
    let root = normalise(root);
    path.parent() == Some(root.as_path())
}

fn normalise(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| absolute_path(Path::new("/"), path))
}

fn normalise_absolute(path: &Path, base: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalise(&path)
}

fn absolute_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn path_is_inside(root: &Path, candidate: &Path) -> bool {
    normalise(candidate).starts_with(normalise(root))
}

fn age_days(path: &Path, now: i64) -> Option<u32> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let timestamp = modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64;
    let days = now.saturating_sub(timestamp) / DAY_MS;
    Some(u32::try_from(days).unwrap_or(u32::MAX))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PREPARATION_REPOSITORY: &str = concat!(
        "aaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaa"
    );

    fn init_storage_root_test_repository(root: &Path) -> GitRepo {
        std::fs::create_dir_all(root).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["commit", "--allow-empty", "-qm", "fixture"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(root)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        GitRepo::discover(root).unwrap()
    }

    fn bound_storage_context(
        repository: &GitRepo,
        host_state_dir: &Path,
    ) -> crate::broker::BrokerStorageContext {
        crate::broker::BrokerStorageContext {
            repository_root: Some(repository.root().canonicalize().unwrap()),
            git_common_dir: Some(repository.git_common_dir().unwrap().canonicalize().unwrap()),
            host_state_dir: Some(host_state_dir.to_path_buf()),
            host_state_dir_is_explicit: true,
            ..crate::broker::BrokerStorageContext::default()
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    struct LiveRootFailureFixture {
        _temporary: tempfile::TempDir,
        main_root: PathBuf,
        storage_layout: StorageRootLayout,
        host_cache_root: PathBuf,
        cache_candidate: PathBuf,
        bindings: Vec<crate::broker::PreparationCacheRootBinding>,
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl LiveRootFailureFixture {
        fn new(placement_root_count: usize) -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let main_root = temporary.path().join("repository");
            std::fs::create_dir_all(&main_root).unwrap();
            let storage_root = temporary.path().join("worktrees");
            std::fs::create_dir_all(&storage_root).unwrap();

            let host_cache_root = temporary.path().join("host-preparation-cache");
            let cache_candidate = host_cache_root
                .join(TEST_PREPARATION_REPOSITORY)
                .join("deadbeefdead");
            std::fs::create_dir_all(&cache_candidate).unwrap();
            std::fs::write(
                cache_candidate.join("cache.bin"),
                b"keep on incomplete inventory",
            )
            .unwrap();

            let mut bindings = Vec::new();
            for index in 0..placement_root_count {
                let placement_root = temporary.path().join(format!("placement-{index}"));
                std::fs::create_dir_all(&placement_root).unwrap();
                let cache_root = placement_root
                    .join(crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT);
                let marker = WorktreeRootMarker {
                    schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
                    repository_key: format!("fixture-{index}"),
                    repository_root: main_root.clone(),
                };
                let marker_path = placement_root.join(WORKTREE_ROOT_MARKER);
                std::fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
                let resource_db_path = test_host_resource_db(temporary.path());
                let placement_identity =
                    crate::preparation::rotation_debt::DirectoryIdentity::capture(&placement_root)
                        .unwrap()
                        .encode()
                        .unwrap();
                crate::preparation::rotation_debt::ensure_preparation_cache_root_child(
                    &placement_root,
                    crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT,
                    &placement_identity,
                )
                .unwrap();
                let _initialized =
                    initialized_storage_cache_root_binding(&cache_root, &resource_db_path);
                let binding =
                    crate::broker::lifecycle::capture_worktree_volume_preparation_cache_root(
                        &placement_root,
                        &marker,
                        &resource_db_path,
                    )
                    .unwrap()
                    .unwrap();
                bindings.push(binding);
            }

            Self {
                _temporary: temporary,
                main_root,
                storage_layout: StorageRootLayout::KeyedContainer(storage_root),
                host_cache_root,
                cache_candidate,
                bindings,
            }
        }

        fn run_plan<F>(
            &self,
            apply: bool,
            enumerate_children: &mut F,
        ) -> Result<StoragePlan, StorageError>
        where
            F: FnMut(&Path) -> Result<Vec<PathBuf>, StorageError>,
        {
            let mut records = crate::measurement::SizeRecords::default();
            if apply {
                build_apply_plan(
                    &self.main_root,
                    &self.storage_layout,
                    Some(&self.host_cache_root),
                    None,
                    None,
                    7,
                    Vec::new(),
                    &mut records,
                    &self.bindings,
                    None,
                    enumerate_children,
                )
            } else {
                build_plan(
                    &self.main_root,
                    &self.storage_layout,
                    Some(&self.host_cache_root),
                    None,
                    None,
                    7,
                    Vec::new(),
                    crate::SizeScan::Recorded,
                    &mut records,
                    &self.bindings,
                    None,
                    enumerate_children,
                )
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    struct ForeignCacheRootFixture {
        _temporary: tempfile::TempDir,
        _foreign_storage: crate::test_storage::TestStorage,
        invoking_root: PathBuf,
        foreign_common_dir: PathBuf,
        storage_root: PathBuf,
        placement_root: PathBuf,
        resource_db_path: PathBuf,
        marker: WorktreeRootMarker,
        unplaced_cache_binding: crate::broker::PreparationCacheRootBinding,
        foreign_cache_entry: PathBuf,
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl ForeignCacheRootFixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let invoking_root = temporary.path().join("invoking-repository");
            let invoking_repo = init_storage_root_test_repository(&invoking_root);
            let invoking_root = invoking_repo.main_root().unwrap();

            let foreign_root = temporary.path().join("foreign-repository");
            let foreign_repo = init_storage_root_test_repository(&foreign_root);
            let foreign_root = foreign_repo.main_root().unwrap();
            let foreign_common_dir = foreign_repo.git_common_dir().unwrap();
            let foreign_storage = crate::test_storage::TestStorage::new();
            let _foreign_broker = foreign_storage.open(&foreign_root).unwrap();

            let resource_db_path = test_host_resource_db(temporary.path());
            let storage_root = temporary.path().join("worktree-container");
            std::fs::create_dir_all(&storage_root).unwrap();
            let storage_root = storage_root.canonicalize().unwrap();
            let placement_root = storage_root.join("foreign-placement");
            std::fs::create_dir_all(&placement_root).unwrap();
            let placement_root = placement_root.canonicalize().unwrap();
            let marker = WorktreeRootMarker {
                schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
                repository_key: crate::host_state::repository_key(
                    &foreign_root,
                    Some(&foreign_common_dir),
                ),
                repository_root: foreign_root,
            };
            std::fs::write(
                placement_root.join(WORKTREE_ROOT_MARKER),
                serde_json::to_vec(&marker).unwrap(),
            )
            .unwrap();

            let cache_root = placement_root
                .join(crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT);
            let placement_identity =
                crate::preparation::rotation_debt::DirectoryIdentity::capture(&placement_root)
                    .unwrap()
                    .encode()
                    .unwrap();
            crate::preparation::rotation_debt::ensure_preparation_cache_root_child(
                &placement_root,
                crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT,
                &placement_identity,
            )
            .unwrap();
            let unplaced_cache_binding =
                initialized_storage_cache_root_binding(&cache_root, &resource_db_path);
            let _placed_cache_binding =
                crate::broker::lifecycle::capture_worktree_volume_preparation_cache_root(
                    &placement_root,
                    &marker,
                    &resource_db_path,
                )
                .unwrap()
                .expect("the initialized foreign cache root has a valid placement binding");

            let foreign_cache_entry = cache_root
                .join(TEST_PREPARATION_REPOSITORY)
                .join("ffffffffffff");
            std::fs::create_dir_all(&foreign_cache_entry).unwrap();
            std::fs::write(foreign_cache_entry.join("payload.bin"), b"foreign cache").unwrap();

            Self {
                _temporary: temporary,
                _foreign_storage: foreign_storage,
                invoking_root,
                foreign_common_dir,
                storage_root,
                placement_root,
                resource_db_path,
                marker,
                unplaced_cache_binding,
                foreign_cache_entry,
            }
        }

        fn build_plan(&self, apply: bool) -> Result<StoragePlan, StorageError> {
            let mut records = crate::measurement::SizeRecords::default();
            let mut enumerate_children = preparation_live_root_children;
            let layout = StorageRootLayout::KeyedContainer(self.storage_root.clone());
            if apply {
                build_apply_plan(
                    &self.invoking_root,
                    &layout,
                    None,
                    None,
                    None,
                    7,
                    Vec::new(),
                    &mut records,
                    &[],
                    Some(&self.resource_db_path),
                    &mut enumerate_children,
                )
            } else {
                build_plan(
                    &self.invoking_root,
                    &layout,
                    None,
                    None,
                    None,
                    7,
                    Vec::new(),
                    crate::SizeScan::Recorded,
                    &mut records,
                    &[],
                    Some(&self.resource_db_path),
                    &mut enumerate_children,
                )
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn storage_plan_recognizes_but_never_reclaims_a_valid_foreign_cache_root() {
        let fixture = ForeignCacheRootFixture::new();

        let plan = fixture.build_plan(false).unwrap();

        assert!(
            plan.roots
                .iter()
                .any(|root| root.path == fixture.placement_root),
            "the foreign placement root remains in the host inventory"
        );
        assert!(
            plan.roots
                .iter()
                .all(|root| !root.blockers.iter().any(|reason| {
                    reason.contains("reserved worktree-volume preparation-cache root")
                })),
            "the independently validated placement/cache markers resolve the reserved child"
        );
        assert!(
            plan.preparation_candidates
                .iter()
                .all(|candidate| candidate.path != fixture.foreign_cache_entry),
            "foreign cache entries are not inspected or granted deletion authority"
        );
        assert!(fixture.foreign_cache_entry.join("payload.bin").is_file());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn storage_apply_refuses_changed_foreign_cache_markers() {
        let fixture = ForeignCacheRootFixture::new();
        let _reviewed_plan = fixture.build_plan(false).unwrap();

        let marker_path = fixture.placement_root.join(WORKTREE_ROOT_MARKER);
        let original_marker = std::fs::read(&marker_path).unwrap();
        let mut wrong_owner = fixture.marker.clone();
        wrong_owner.repository_key.push_str("-wrong");
        std::fs::write(&marker_path, serde_json::to_vec(&wrong_owner).unwrap()).unwrap();

        let changed_placement = fixture.build_plan(true).unwrap_err();
        assert!(
            matches!(&changed_placement, StorageError::Unavailable { reason }
                if reason.contains("unverified reserved preparation-cache child")),
            "a changed parent marker makes the reserved child unverified: {changed_placement:?}"
        );

        std::fs::write(&marker_path, original_marker).unwrap();
        let detached_common_dir = fixture
            ._temporary
            .path()
            .join("detached-foreign-git-common");
        std::fs::rename(&fixture.foreign_common_dir, &detached_common_dir).unwrap();
        let changed_git_common = fixture.build_plan(true).unwrap_err();
        assert!(
            matches!(&changed_git_common, StorageError::Unavailable { reason }
                if reason.contains("unverified reserved preparation-cache child")),
            "a missing Git common directory keeps the reserved cache unknown: {changed_git_common:?}"
        );
        std::fs::rename(&detached_common_dir, &fixture.foreign_common_dir).unwrap();

        change_cache_root_coordinator(&fixture.unplaced_cache_binding);
        let changed_cache_marker = fixture.build_plan(true).unwrap_err();
        assert!(
            matches!(&changed_cache_marker, StorageError::Unavailable { reason }
                if reason.contains("unverified reserved preparation-cache child")),
            "a changed cache-root marker is retained: {changed_cache_marker:?}"
        );
        assert!(fixture.foreign_cache_entry.join("payload.bin").is_file());
    }

    #[test]
    fn storage_root_entry_inventory_is_bounded_and_direct_roots_are_not_enumerated() {
        let temporary = tempfile::tempdir().unwrap();
        let storage_root = temporary.path().join("container");
        std::fs::create_dir_all(storage_root.join("one")).unwrap();
        std::fs::create_dir_all(storage_root.join("two")).unwrap();

        let bounded = storage_root_entries_with_limit(
            &storage_root,
            &StorageRootLayout::KeyedContainer(storage_root.clone()),
            1,
        );
        assert!(
            matches!(
                bounded.as_ref(),
                Err(StorageError::Unavailable { reason }) if reason.contains("entry bound")
            ),
            "an incomplete keyed-root inventory must fail closed: {bounded:?}"
        );
        assert_eq!(
            storage_root_entries_with_limit(
                &storage_root,
                &StorageRootLayout::DirectRoot(storage_root.clone()),
                0,
            )
            .unwrap(),
            vec![storage_root],
            "a direct root is one item and does not enumerate its descendants"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn storage_plan_and_apply_refuse_an_unreadable_placement_root() {
        let fixture = LiveRootFailureFixture::new(1);
        let expected_root = fixture.bindings[0]
            .root_guard
            .as_ref()
            .unwrap()
            .placement_root
            .as_ref()
            .unwrap()
            .path
            .clone();

        for apply in [false, true] {
            let mut enumerate_children = |root: &Path| {
                assert_eq!(root, expected_root.as_path());
                Err(StorageError::Io {
                    path: root.to_path_buf(),
                    source: std::io::Error::other("injected read_dir open failure"),
                })
            };
            let result = fixture.run_plan(apply, &mut enumerate_children);
            assert!(
                matches!(result.as_ref().err(), Some(StorageError::Io { .. })),
                "planning must refuse an unreadable placement root: {result:?}"
            );
            assert!(
                fixture.cache_candidate.is_dir(),
                "an incomplete liveness inventory must preserve cache entries"
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn storage_plan_and_apply_refuse_a_later_placement_iterator_error() {
        let fixture = LiveRootFailureFixture::new(2);
        let placement_roots = fixture
            .bindings
            .iter()
            .map(|binding| {
                binding
                    .root_guard
                    .as_ref()
                    .unwrap()
                    .placement_root
                    .as_ref()
                    .unwrap()
                    .path
                    .clone()
            })
            .collect::<Vec<_>>();
        assert!(placement_roots[0] < placement_roots[1]);

        for apply in [false, true] {
            let mut inspected = Vec::new();
            let result = {
                let mut enumerate_children = |root: &Path| {
                    inspected.push(root.to_path_buf());
                    if root == placement_roots[0].as_path() {
                        Ok(Vec::new())
                    } else {
                        Err(StorageError::Io {
                            path: root.to_path_buf(),
                            source: std::io::Error::other("injected iterator item failure"),
                        })
                    }
                };
                fixture.run_plan(apply, &mut enumerate_children)
            };
            assert!(
                matches!(result.as_ref().err(), Some(StorageError::Io { .. })),
                "planning must refuse a partially enumerated placement cohort: {result:?}"
            );
            assert_eq!(inspected, placement_roots);
            assert!(
                fixture.cache_candidate.is_dir(),
                "an incomplete liveness inventory must preserve cache entries"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn live_root_enumeration_ignores_files_and_refuses_symlinks_or_reserved_files() {
        let temporary = tempfile::tempdir().unwrap();
        let real_root = temporary.path().join("real-root");
        std::fs::create_dir(&real_root).unwrap();
        let symlink_root = temporary.path().join("symlink-root");
        std::os::unix::fs::symlink(&real_root, &symlink_root).unwrap();
        assert!(matches!(
            preparation_live_root_children(&symlink_root),
            Err(StorageError::Unavailable { .. })
        ));

        std::fs::write(real_root.join(WORKTREE_ROOT_MARKER), b"marker").unwrap();
        std::fs::write(real_root.join("unknown-file"), b"ordinary content").unwrap();
        let checkout = real_root.join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        assert_eq!(
            preparation_live_root_children(&real_root).unwrap(),
            vec![checkout]
        );

        let symlink_child_target = temporary.path().join("symlink-child-target");
        std::fs::create_dir(&symlink_child_target).unwrap();
        let symlink_child = real_root.join("symlink-child");
        std::os::unix::fs::symlink(&symlink_child_target, &symlink_child).unwrap();
        assert!(matches!(
            preparation_live_root_children(&real_root),
            Err(StorageError::Unavailable { .. })
        ));

        let reserved_root = temporary.path().join("reserved-root");
        std::fs::create_dir(&reserved_root).unwrap();
        std::fs::write(
            reserved_root.join(crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT),
            b"unexpected file at reserved cache path",
        )
        .unwrap();
        assert!(matches!(
            preparation_live_root_children(&reserved_root),
            Err(StorageError::Unavailable { .. })
        ));
    }

    #[test]
    fn live_root_inventory_refuses_an_unverified_reserved_cache_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let main_root = temporary.path().join("main");
        std::fs::create_dir(&main_root).unwrap();
        let container = temporary.path().join("container");
        let reserved_cache =
            container.join(crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT);
        std::fs::create_dir_all(&reserved_cache).unwrap();

        let mut records = crate::measurement::SizeRecords::default();
        let root = inspect_root(
            &container,
            now_ms(),
            7,
            crate::SizeScan::Recorded,
            &mut records,
            &BTreeSet::new(),
        );
        let mut enumerate_children = preparation_live_root_children;
        let result = preparation_live_roots_with_reader(
            &main_root,
            &[],
            &[root],
            &BTreeSet::new(),
            &mut enumerate_children,
        );
        assert!(
            matches!(
                result.as_ref().err(),
                Some(StorageError::Unavailable { reason })
                    if reason.contains("unverified reserved preparation-cache child")
            ),
            "an unverified reserved directory makes liveness incomplete: {result:?}"
        );
    }

    #[test]
    fn non_container_storage_roots_remain_cache_witnesses_and_unknown_roots_retain_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let main_root = temporary.path().join("main");
        std::fs::create_dir(&main_root).unwrap();
        let checkout = temporary.path().join("root-level-checkout");
        let repository = init_storage_root_test_repository(&checkout);
        std::fs::create_dir_all(repository.root().join(".aethyme")).unwrap();
        std::fs::write(repository.root().join("lock.txt"), "deps\n").unwrap();
        std::fs::write(
            repository.root().join(".aethyme/prepare.toml"),
            "schema_version = 1\n\n[[steps]]\nname = \"deps\"\ncommand = [\"sh\", \"-c\", \"true\"]\ninputs = [\"lock.txt\"]\noutputs = [\"out/\"]\ncache = \"repository_shared\"\nrequired_for_hooks = false\n",
        )
        .unwrap();
        let live_key = crate::preparation::current_cache_key(repository.root())
            .unwrap()
            .expect("the root-level checkout declares a shared preparation step");

        let mut records = crate::measurement::SizeRecords::default();
        let checkout_root = inspect_root(
            repository.root(),
            now_ms(),
            7,
            crate::SizeScan::Recorded,
            &mut records,
            &BTreeSet::new(),
        );
        assert_eq!(
            checkout_root.marker_status,
            StorageMarkerStatus::NotApplicable
        );
        let mut should_not_descend = |_: &Path| -> Result<Vec<PathBuf>, StorageError> {
            panic!("a checkout at the container level must not be enumerated as a container")
        };
        let live_roots = preparation_live_roots_with_reader(
            &main_root,
            &[],
            &[checkout_root],
            &BTreeSet::new(),
            &mut should_not_descend,
        )
        .unwrap();
        assert!(live_roots.contains(&normalise(repository.root())));

        let cache_root = temporary.path().join("preparation-cache");
        let repository_cache = cache_root.join(TEST_PREPARATION_REPOSITORY);
        std::fs::create_dir_all(repository_cache.join(&live_key)).unwrap();
        std::fs::create_dir_all(repository_cache.join("deadbeefdead")).unwrap();
        let (entries, candidates) = inspect_preparation_cache(
            &cache_root,
            &live_roots,
            crate::SizeScan::Recorded,
            &mut records,
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].key, "deadbeefdead");
        assert!(
            entries
                .iter()
                .find(|entry| entry.key == live_key)
                .is_some_and(|entry| !entry.reclaimable)
        );

        let assert_unknown_root_retains_entries =
            |root: StorageRoot,
             expected_path: &Path,
             records: &mut crate::measurement::SizeRecords| {
                let mut should_not_descend = |_: &Path| -> Result<Vec<PathBuf>, StorageError> {
                    panic!("a non-container storage entry must not be enumerated")
                };
                let live_roots = preparation_live_roots_with_reader(
                    &main_root,
                    &[],
                    &[root],
                    &BTreeSet::new(),
                    &mut should_not_descend,
                )
                .unwrap();
                assert!(
                    live_roots
                        .iter()
                        .any(|root| root.as_path() == expected_path)
                );
                let (entries, candidates) = inspect_preparation_cache(
                    &cache_root,
                    &live_roots,
                    crate::SizeScan::Recorded,
                    records,
                );
                assert_eq!(entries.len(), 2);
                assert!(
                    entries.iter().all(|entry| {
                        !entry.reclaimable && entry.reason.contains("could not be verified")
                    }),
                    "an unknown non-container key must retain all entries: {entries:#?}"
                );
                assert!(candidates.is_empty());
            };

        let non_directory_root = temporary.path().join(".DS_Store");
        std::fs::write(&non_directory_root, b"not a checkout directory").unwrap();
        let file_root = inspect_root(
            &non_directory_root,
            now_ms(),
            7,
            crate::SizeScan::Recorded,
            &mut records,
            &BTreeSet::new(),
        );
        assert_eq!(file_root.filesystem_kind, StorageFilesystemKind::File);
        let expected_file_path = file_root.path.clone();
        let mut should_not_descend = |_: &Path| -> Result<Vec<PathBuf>, StorageError> {
            panic!("a regular file is not a storage container")
        };
        let live_roots = preparation_live_roots_with_reader(
            &main_root,
            &[],
            &[file_root],
            &BTreeSet::new(),
            &mut should_not_descend,
        )
        .unwrap();
        assert!(live_roots.iter().any(|root| root == &expected_file_path));
        let (entries, candidates) = inspect_preparation_cache(
            &cache_root,
            &live_roots,
            crate::SizeScan::Recorded,
            &mut records,
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(candidates.len(), 2);
        assert!(entries.iter().all(|entry| entry.reclaimable));
        assert!(
            entries
                .iter()
                .all(|entry| entry.reason.contains("inputs have changed"))
        );

        // Liveness uses a fresh lstat: if the observed file becomes a
        // directory with unreadable preparation config before inspection,
        // the inventory returns to fail-closed behavior.
        std::fs::remove_file(&non_directory_root).unwrap();
        std::fs::create_dir_all(non_directory_root.join(".aethyme")).unwrap();
        std::fs::write(
            non_directory_root.join(".aethyme/prepare.toml"),
            "invalid = [",
        )
        .unwrap();
        let (entries, candidates) = inspect_preparation_cache(
            &cache_root,
            &live_roots,
            crate::SizeScan::Recorded,
            &mut records,
        );
        assert_eq!(entries.len(), 2);
        assert!(
            entries.iter().all(|entry| {
                !entry.reclaimable && entry.reason.contains("could not be verified")
            })
        );
        assert!(candidates.is_empty());

        #[cfg(unix)]
        {
            let symlink_root = temporary.path().join("symlink-root");
            std::os::unix::fs::symlink(repository.root(), &symlink_root).unwrap();
            let link_root = inspect_root(
                &symlink_root,
                now_ms(),
                7,
                crate::SizeScan::Recorded,
                &mut records,
                &BTreeSet::new(),
            );
            assert_eq!(link_root.filesystem_kind, StorageFilesystemKind::Symlink);
            let expected_link_path = link_root.path.clone();
            assert_unknown_root_retains_entries(link_root, &expected_link_path, &mut records);
        }
    }

    #[test]
    fn reviewed_preparation_removal_ignores_current_regular_file_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path().join("preparation-cache");
        let entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("ccccccccccc1");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), b"stale").unwrap();
        let candidate = StoragePreparationCandidate {
            path: entry.clone(),
            repository: TEST_PREPARATION_REPOSITORY.into(),
            key: "ccccccccccc1".into(),
            estimated_bytes: None,
            estimated_inodes: None,
            reason: "no checkout computes this key".into(),
        };
        let ordinary_file = tmp.path().join(".DS_Store");
        std::fs::write(&ordinary_file, b"desktop metadata").unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());

        let removed = apply_preparation_candidate_with_lease_at(
            &candidate,
            &[ordinary_file],
            &resource_db_path,
            &cache_root,
        );

        assert!(
            removed.is_ok(),
            "a regular file cannot be a live checkout: {removed:?}"
        );
        assert!(!entry.exists());
    }

    #[cfg(unix)]
    #[test]
    fn reviewed_preparation_removal_refuses_a_root_replaced_by_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path().join("preparation-cache");
        let entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("ccccccccccc1");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), b"stale").unwrap();
        let candidate = StoragePreparationCandidate {
            path: entry.clone(),
            repository: TEST_PREPARATION_REPOSITORY.into(),
            key: "ccccccccccc1".into(),
            estimated_bytes: None,
            estimated_inodes: None,
            reason: "no checkout computes this key".into(),
        };
        let root = tmp.path().join(".DS_Store");
        std::fs::write(&root, b"desktop metadata").unwrap();
        let target = tmp.path().join("unknown-target");
        std::fs::create_dir(&target).unwrap();
        std::fs::remove_file(&root).unwrap();
        std::os::unix::fs::symlink(&target, &root).unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());

        let result = apply_preparation_candidate_with_lease_at(
            &candidate,
            &[root],
            &resource_db_path,
            &cache_root,
        );

        assert!(
            result.is_err(),
            "a replacement symlink makes liveness unknown"
        );
        assert!(entry.join("cache.bin").is_file());
    }

    #[test]
    fn storage_root_uses_shared_resolver_and_preserves_root_layouts() {
        let tmp = tempfile::tempdir().unwrap();
        let repository_root = tmp.path().join("repository");
        let repository = init_storage_root_test_repository(&repository_root);
        let owner = crate::test_storage::TestStorage::new();
        let host_state = owner.host_state_dir();
        std::fs::create_dir_all(&host_state).unwrap();
        let configured_root = owner.root().join("configured-worktrees");
        let environment_root = owner.root().join("environment-worktrees");
        let direct_root = owner.root().join("direct-worktrees");
        std::fs::create_dir_all(&configured_root).unwrap();
        std::fs::create_dir_all(repository_root.join(".aethyme")).unwrap();
        std::fs::write(
            repository_root.join(".aethyme/config.toml"),
            format!(
                "[worktrees]\nroot = {:?}\nmin_free_bytes = 0\n",
                configured_root.to_string_lossy()
            ),
        )
        .unwrap();

        let mut context = bound_storage_context(&repository, &host_state);
        let plan = crate::broker::lifecycle::plan_worktree_root_with_context(
            &repository_root,
            Some(&repository.git_common_dir().unwrap()),
            &context,
        );
        assert_eq!(
            plan.preferred_source,
            Some(crate::broker::WorktreeRootSource::RepositoryConfig)
        );
        assert_eq!(
            plan.root_container.as_deref(),
            Some(configured_root.as_path())
        );
        assert_eq!(
            storage_layout_with_context(&repository_root, &context)
                .unwrap()
                .path(),
            configured_root.as_path()
        );
        let configured_repo_root = configured_root.join(&plan.repository_key);
        std::fs::create_dir(&configured_repo_root).unwrap();
        let attribution =
            storage_attribute_with_context(&repository_root, false, &context).unwrap();
        assert_eq!(attribution.storage_root, configured_root);
        assert_eq!(attribution.roots.len(), 1);
        assert_eq!(attribution.roots[0].root, configured_repo_root);
        assert!(attribution.roots[0].attributable);
        assert!(!attribution.roots[0].marked);

        context.worktree_root_environment_override = Some(environment_root.clone());
        let plan = crate::broker::lifecycle::plan_worktree_root_with_context(
            &repository_root,
            Some(&repository.git_common_dir().unwrap()),
            &context,
        );
        assert_eq!(
            plan.preferred_source,
            Some(crate::broker::WorktreeRootSource::EnvironmentOverride)
        );
        assert_eq!(
            storage_layout_with_context(&repository_root, &context)
                .unwrap()
                .path(),
            environment_root.as_path()
        );

        context.worktree_root_override = Some(direct_root.clone());
        context.worktree_root_environment_override = None;
        let plan = crate::broker::lifecycle::plan_worktree_root_with_context(
            &repository_root,
            Some(&repository.git_common_dir().unwrap()),
            &context,
        );
        assert_eq!(
            plan.preferred_source,
            Some(crate::broker::WorktreeRootSource::LibraryOverride)
        );
        assert!(plan.root_container.is_none());
        assert_eq!(
            storage_layout_with_context(&repository_root, &context).unwrap(),
            StorageRootLayout::DirectRoot(direct_root.clone())
        );

        context.worktree_root_override = None;
        std::fs::remove_file(repository_root.join(".aethyme/config.toml")).unwrap();
        assert_eq!(
            storage_layout_with_context(&repository_root, &context)
                .unwrap()
                .path(),
            host_state.join("worktrees").as_path()
        );

        let unavailable_root = owner.root().join("not-mounted");
        std::fs::write(
            repository_root.join(".aethyme/config.toml"),
            format!(
                "[worktrees]\nroot = {:?}\nmin_free_bytes = 0\n",
                unavailable_root
            ),
        )
        .unwrap();
        let plan = crate::broker::lifecycle::plan_worktree_root_with_context(
            &repository_root,
            Some(&repository.git_common_dir().unwrap()),
            &context,
        );
        assert_eq!(
            plan.preferred_source,
            Some(crate::broker::WorktreeRootSource::RepositoryConfig)
        );
        assert!(plan.preferred_unavailable_reason.is_some());
        assert_eq!(
            storage_layout_with_context(&repository_root, &context)
                .unwrap()
                .path(),
            host_state.join("worktrees").as_path()
        );
    }

    #[test]
    fn direct_library_root_is_attributed_and_inventoried_at_its_own_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = tmp.path().canonicalize().unwrap();
        let repository_root = tmp_root.as_path().join("repository");
        let repository = init_storage_root_test_repository(&repository_root);
        let owner = crate::test_storage::TestStorage::new();
        let host_state = owner.host_state_dir();
        std::fs::create_dir_all(&host_state).unwrap();
        let direct_root = owner.root().join("direct-worktrees");
        std::fs::create_dir_all(&direct_root).unwrap();
        let registered_checkout = direct_root.join("session-one");
        assert!(
            std::process::Command::new("git")
                .args(["worktree", "add", "--detach"])
                .arg(&registered_checkout)
                .arg("HEAD")
                .current_dir(&repository_root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            repository
                .worktree_paths()
                .unwrap()
                .iter()
                .any(|path| normalise(path) == normalise(&registered_checkout)),
            "the direct-root child must be registered in the repository's Git metadata"
        );
        let mut context = bound_storage_context(&repository, &host_state);
        context.worktree_root_override = Some(direct_root.clone());

        let attribution =
            storage_attribute_with_context(&repository_root, false, &context).unwrap();
        assert_eq!(attribution.storage_root, direct_root);
        assert_eq!(attribution.roots.len(), 1);
        assert_eq!(attribution.roots[0].root, direct_root);
        assert!(attribution.roots[0].attributable);
        assert_eq!(attribution.roots[0].registered_worktree_count, 1);

        let marked = storage_attribute_with_context(&repository_root, true, &context).unwrap();
        assert_eq!(marked.roots.len(), 1);
        assert!(marked.roots[0].marked);
        assert!(direct_root.join(WORKTREE_ROOT_MARKER).is_file());

        let plan = storage_plan_with_context(&repository_root, &context).unwrap();
        assert_eq!(plan.storage_root, direct_root);
        assert_eq!(plan.roots.len(), 1);
        let root = &plan.roots[0];
        assert_eq!(root.path, direct_root);
        assert_eq!(root.marker_status, StorageMarkerStatus::Valid);
        assert_eq!(root.worktree_count, 1);
        assert_eq!(root.reconciliation.on_disk_count, 1);
        assert_eq!(root.reconciliation.entries[0].path, registered_checkout);
        assert!(root.reconciliation.entries[0].git_marker);
        assert!(plan.candidates.is_empty());

        let marker_bytes = std::fs::read(direct_root.join(WORKTREE_ROOT_MARKER)).unwrap();
        let marker = read_marker(&direct_root);
        let marker_sha256 = format!(
            "{:x}",
            Sha256::digest(marker.bytes.as_deref().expect("marker bytes are readable"))
        );
        let marker = marker.marker.expect("attributed marker is valid");
        let candidate = StorageCandidate {
            kind: StorageCandidateKind::StrayDirectory,
            path: registered_checkout.clone(),
            root_path: direct_root.clone(),
            repository_key: marker.repository_key,
            repository_root: marker.repository_root,
            git_marker: true,
            estimated_bytes: None,
            estimated_inodes: None,
            reason: "fixture candidate to exercise layout revalidation".into(),
            marker_sha256,
        };
        let wrong_layout = StorageRootLayout::KeyedContainer(owner.root().join("other-layout"));
        assert_eq!(
            apply_candidate(&candidate, &wrong_layout).unwrap_err(),
            "reviewed root is outside the selected storage layout"
        );
        assert_eq!(
            apply_candidate(
                &candidate,
                &StorageRootLayout::DirectRoot(direct_root.clone())
            )
            .unwrap_err(),
            format!(
                "session ledger is missing or is not a regular file at {}; no stray directory is swept",
                repository_root.join(crate::BROKER_DB_RELPATH).display()
            )
        );
        assert_eq!(
            std::fs::read(direct_root.join(WORKTREE_ROOT_MARKER)).unwrap(),
            marker_bytes
        );
        assert!(registered_checkout.is_dir());
    }

    fn test_host_resource_db(root: &Path) -> PathBuf {
        let path = root.join("host-state/host-resources.db");
        crate::HostResourceCoordinator::open(&path)
            .expect("initialize isolated host resource state");
        path
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn publish_external_rotation_journal_debt(
        cache_root: &Path,
        resource_db_path: &Path,
        repository: &str,
    ) -> Vec<u8> {
        let store = cache_root.join(repository);
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("old-cache"), b"interrupted before recovery").unwrap();
        let root_identity =
            crate::preparation::rotation_debt::DirectoryIdentity::capture(cache_root).unwrap();
        let debt = crate::preparation::rotation_debt::RotationDebt::new(
            repository,
            &crate::preparation::rotation_debt::new_transaction_id(),
            root_identity.clone(),
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&store).unwrap(),
        )
        .unwrap();
        let payload = crate::preparation::rotation_debt::encode(&debt).unwrap();
        let encoded_root_identity = root_identity.encode().unwrap();
        crate::preparation::rotation_journal::publish(
            resource_db_path,
            cache_root,
            &encoded_root_identity,
            repository,
            &payload,
        )
        .unwrap();
        assert!(
            crate::preparation::rotation_debt::read(cache_root, repository)
                .unwrap()
                .is_none(),
            "fixture represents a crash after external publication but before the in-cache witness"
        );
        encoded_root_identity
    }

    fn unguarded_host_cache_binding(
        cache_root: &Path,
    ) -> crate::broker::PreparationCacheRootBinding {
        crate::broker::PreparationCacheRootBinding {
            path: cache_root.to_path_buf(),
            identity: None,
            instance_nonce: None,
            root_guard: None,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn recovery_only_completes_observed_empty_or_absent_cache_without_creating_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let resource_db = root.join("host-resources.db");
        let cache_root = root.join("cache");
        for present in [false, true] {
            if present {
                std::fs::create_dir(&cache_root).unwrap();
            }
            let binding = unguarded_host_cache_binding(&cache_root);
            let report = recover_preparation_caches_for_bindings(
                &[binding],
                &resource_db,
                std::time::Instant::now() + Duration::from_secs(5),
                true,
            );
            assert!(report.complete, "present={present}: {report:?}");
            assert_eq!(report.coverage_deferral, None);
            assert_eq!(report.filesystem_entries_reclaimed, 0);
            assert_eq!(cache_root.exists(), present);
            assert!(!resource_db.exists());
        }
    }

    #[test]
    fn recovery_only_retains_other_repository_and_same_origin_clone_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let cache_root = root.join("preparation-cache");
        let same_origin_key = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("aaaaaaaaaaaa");
        let other_repository_key = cache_root.join("b".repeat(64)).join("bbbbbbbbbbbb");
        let retired = cache_root.join(format!("{TEST_PREPARATION_REPOSITORY}.retired"));
        for entry in [&same_origin_key, &other_repository_key, &retired] {
            std::fs::create_dir_all(entry).unwrap();
            std::fs::write(
                entry.join("cache.bin"),
                b"another consumer still needs this",
            )
            .unwrap();
        }
        let binding = unguarded_host_cache_binding(&cache_root);
        let resource_db_path = test_host_resource_db(&root);
        let report = recover_preparation_caches_for_bindings(
            &[binding],
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
            true,
        );
        assert!(!report.complete, "known roots do not prove all consumers");
        assert_eq!(report.entries_removed, 0);
        assert_eq!(report.filesystem_entries_reclaimed, 0);
        assert_eq!(
            report.coverage_deferral,
            Some(PreparationCacheCoverageDeferral::HostConsumersNotEnumerated)
        );
        for entry in [&same_origin_key, &other_repository_key, &retired] {
            assert_eq!(
                std::fs::read(entry.join("cache.bin")).unwrap(),
                b"another consumer still needs this"
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn recovery_only_acknowledges_completed_journal_without_consumer_pruning() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let cache_root = root.join("preparation-cache");
        let resource_db_path = test_host_resource_db(&root);
        let identity = publish_external_rotation_journal_debt(
            &cache_root,
            &resource_db_path,
            TEST_PREPARATION_REPOSITORY,
        );
        let protected = cache_root.join("b".repeat(64)).join("bbbbbbbbbbbb");
        std::fs::create_dir_all(&protected).unwrap();
        std::fs::write(protected.join("payload"), b"live elsewhere").unwrap();
        let report = recover_preparation_caches_for_bindings(
            &[unguarded_host_cache_binding(&cache_root)],
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
            false,
        );
        assert!(
            !report.complete,
            "binding discovery and consumer coverage remain incomplete"
        );
        assert!(
            report.filesystem_entries_reclaimed > 0,
            "known binding recovery must still progress"
        );
        assert!(
            !cache_root
                .join(TEST_PREPARATION_REPOSITORY)
                .join("old-cache")
                .exists()
        );
        assert_eq!(
            std::fs::read(protected.join("payload")).unwrap(),
            b"live elsewhere"
        );
        assert!(
            matches!(
                crate::preparation::rotation_journal::probe(
                    &resource_db_path,
                    &cache_root,
                    Some(&identity)
                )
                .unwrap(),
                Some(crate::preparation::rotation_journal::JournalProbe::NoDebt)
            ),
            "complete recovery must acknowledge its lap independently of consumer deferral"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn recovery_only_busy_journal_stays_pending_then_recovers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let cache_root = root.join("preparation-cache");
        let resource_db_path = test_host_resource_db(&root);
        let identity = publish_external_rotation_journal_debt(
            &cache_root,
            &resource_db_path,
            TEST_PREPARATION_REPOSITORY,
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let request =
            preparation_cache_lease_request(TEST_PREPARATION_REPOSITORY, &cache_root, deadline)
                .unwrap();
        let mut writer = crate::HostResourceCoordinator::open(&resource_db_path).unwrap();
        let grant = writer.acquire(&request).unwrap();
        let binding = unguarded_host_cache_binding(&cache_root);
        let blocked = recover_preparation_caches_for_bindings(
            std::slice::from_ref(&binding),
            &resource_db_path,
            deadline,
            true,
        );
        assert!(!blocked.complete);
        assert_eq!(blocked.filesystem_entries_reclaimed, 0);
        assert!(
            cache_root
                .join(TEST_PREPARATION_REPOSITORY)
                .join("old-cache")
                .is_file()
        );
        assert!(matches!(
            crate::preparation::rotation_journal::probe(
                &resource_db_path,
                &cache_root,
                Some(&identity)
            )
            .unwrap(),
            Some(crate::preparation::rotation_journal::JournalProbe::Pending)
        ));
        writer
            .release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .unwrap();
        let recovered = recover_preparation_caches_for_bindings(
            &[binding],
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
            true,
        );
        assert!(!recovered.complete);
        assert!(recovered.filesystem_entries_reclaimed > 0);
        assert!(matches!(
            crate::preparation::rotation_journal::probe(
                &resource_db_path,
                &cache_root,
                Some(&identity)
            )
            .unwrap(),
            Some(crate::preparation::rotation_journal::JournalProbe::NoDebt)
        ));
    }

    #[test]
    fn recovery_only_empty_or_expired_inventory_is_incomplete_without_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let resource_db_path = test_host_resource_db(&root);
        let empty = recover_preparation_caches_for_bindings(
            &[],
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
            true,
        );
        assert!(!empty.complete);
        assert_eq!(empty.filesystem_entries_reclaimed, 0);
        let cache_root = root.join("preparation-cache");
        let expired = recover_preparation_caches_for_bindings(
            &[unguarded_host_cache_binding(&cache_root)],
            &resource_db_path,
            std::time::Instant::now(),
            true,
        );
        assert!(!expired.complete);
        assert_eq!(expired.filesystem_entries_reclaimed, 0);
        assert!(!cache_root.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn initialized_storage_cache_root_binding(
        cache_root: &Path,
        resource_db_path: &Path,
    ) -> crate::broker::PreparationCacheRootBinding {
        use crate::preparation::rotation_debt::{
            PreparationCacheRootCaptureState, capture_preparation_cache_root,
            initialize_preparation_cache_root,
        };

        let resource_db_path = resource_db_path.canonicalize().unwrap();
        let captured = capture_preparation_cache_root(cache_root).unwrap();
        let initialized = initialize_preparation_cache_root(
            cache_root,
            &captured.identity,
            &captured.directory,
            &resource_db_path,
            || Ok(false),
        )
        .unwrap();
        let captured = capture_preparation_cache_root(cache_root).unwrap();
        let state = match captured.state {
            PreparationCacheRootCaptureState::Marked {
                coordinator_db_path,
                nonce,
                cache_root_identity: Some(cache_root_identity),
            } => crate::broker::PreparationCacheRootState::Marked {
                coordinator_db_path,
                nonce,
                cache_root_identity: Some(cache_root_identity),
            },
            other => panic!("expected initialized marker, got {other:?}"),
        };
        crate::broker::PreparationCacheRootBinding {
            path: cache_root.join(format!(".aethyme-preparation-cache-{}", initialized.nonce)),
            identity: Some(initialized.cache_root_identity),
            instance_nonce: Some(initialized.nonce),
            root_guard: Some(crate::broker::PreparationCacheRootGuard {
                root_path: cache_root.to_path_buf(),
                root_identity: captured.identity,
                state,
                captured_resource_db_path: resource_db_path,
                root_handle: std::sync::Arc::new(captured.directory),
                placement_root: None,
            }),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn change_cache_root_coordinator(binding: &crate::broker::PreparationCacheRootBinding) {
        let root = &binding.root_guard.as_ref().unwrap().root_path;
        let marker_path = root.join(".aethyme-preparation-cache-root-v2");
        let mut marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
        marker["coordinator_db_path"] = serde_json::Value::from(
            root.join("different-coordinator/host-resources.db")
                .to_string_lossy()
                .into_owned(),
        );
        std::fs::write(marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn recovery_only_invalid_binding_does_not_block_another_binding() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let resource_db_path = test_host_resource_db(&root);
        let invalid_root = root.join("invalid-cache");
        std::fs::create_dir_all(&invalid_root).unwrap();
        let invalid = initialized_storage_cache_root_binding(&invalid_root, &resource_db_path);
        let protected = invalid_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("aaaaaaaaaaaa");
        std::fs::create_dir_all(&protected).unwrap();
        std::fs::write(protected.join("payload"), b"invalid authority").unwrap();
        change_cache_root_coordinator(&invalid);
        assert!(invalid.validate().is_err());
        let valid_root = root.join("valid-cache");
        publish_external_rotation_journal_debt(
            &valid_root,
            &resource_db_path,
            TEST_PREPARATION_REPOSITORY,
        );
        let report = recover_preparation_caches_for_bindings(
            &[invalid, unguarded_host_cache_binding(&valid_root)],
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
            true,
        );
        assert!(!report.complete);
        assert!(report.filesystem_entries_reclaimed > 0);
        assert!(
            !valid_root
                .join(TEST_PREPARATION_REPOSITORY)
                .join("old-cache")
                .exists()
        );
        assert_eq!(
            std::fs::read(protected.join("payload")).unwrap(),
            b"invalid authority"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn write_two_cache_payloads(entry: &Path) {
        std::fs::create_dir_all(entry).unwrap();
        std::fs::write(entry.join("payload-a"), b"first payload").unwrap();
        std::fs::write(entry.join("payload-b"), b"second payload").unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn remaining_cache_payloads(entry: &Path) -> usize {
        ["payload-a", "payload-b"]
            .into_iter()
            .filter(|name| entry.join(name).exists())
            .count()
    }

    fn preparation_sweep_status_fixture(
        root: &Path,
        payload: &[u8],
    ) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let main_root = root.join("main");
        let worktree_root = root.join("worktrees");
        let cache_root = root.join("preparation-cache");
        let entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("cccccccccccc");
        std::fs::create_dir_all(&main_root).unwrap();
        std::fs::create_dir_all(&worktree_root).unwrap();
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), payload).unwrap();
        let marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: "repository".into(),
            repository_root: main_root.clone(),
        };
        std::fs::write(
            worktree_root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        let resource_db_path = test_host_resource_db(root);
        (
            main_root,
            worktree_root,
            cache_root,
            entry,
            resource_db_path,
        )
    }

    #[test]
    fn captured_preparation_sweep_status_counts_a_fully_removed_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = b"manually counted cache payload";
        let (main_root, worktree_root, cache_root, entry, resource_db_path) =
            preparation_sweep_status_fixture(tmp.path(), payload);

        let report = sweep_preparation_cache_for_roots_with_status(
            &main_root,
            &[worktree_root],
            &cache_root,
            None,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );

        assert_eq!(report.entries_removed, 1);
        assert_eq!(report.bytes_reclaimed, payload.len() as u64);
        // The private fixture tree has exactly the candidate directory and
        // cache.bin beneath it.
        assert_eq!(report.filesystem_entries_reclaimed, 2);
        assert!(report.complete);
        assert!(!entry.exists());
    }

    #[test]
    fn preparation_sweep_ignores_regular_file_roots_but_retains_unknown_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = b"cache payload under review";
        let (main_root, worktree_root, cache_root, entry, resource_db_path) =
            preparation_sweep_status_fixture(tmp.path(), payload);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let ordinary_file = worktree_root.join(".DS_Store");
        std::fs::write(&ordinary_file, b"desktop metadata").unwrap();

        // The sweep's key collection also tolerates a directly observed file
        // when given one by an inventory caller.
        let file_keys =
            current_preparation_keys(std::slice::from_ref(&ordinary_file), deadline).unwrap();
        assert!(file_keys.is_empty());

        let report = sweep_preparation_cache_for_roots_with_status(
            &main_root,
            std::slice::from_ref(&worktree_root),
            &cache_root,
            None,
            &resource_db_path,
            deadline,
        );
        assert!(
            report.complete,
            "regular sibling file blocked sweep: {report:?}"
        );
        assert_eq!(report.entries_removed, 1);
        assert!(!entry.exists());
        assert!(ordinary_file.is_file());

        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), payload).unwrap();
        // An empty unmarked directory has no preparation key and is safe to
        // ignore. Malformed metadata makes this checkout's key unknown.
        let malformed_checkout = worktree_root.join("checkout-with-malformed-prep");
        std::fs::create_dir(&malformed_checkout).unwrap();
        let config_dir = malformed_checkout.join(".aethyme");
        std::fs::create_dir(&config_dir).unwrap();
        std::fs::write(config_dir.join("prepare.toml"), "invalid = [").unwrap();
        let malformed_report = sweep_preparation_cache_for_roots_with_status(
            &main_root,
            std::slice::from_ref(&worktree_root),
            &cache_root,
            None,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );
        assert!(!malformed_report.complete);
        assert!(entry.join("cache.bin").is_file());
        std::fs::remove_dir_all(&malformed_checkout).unwrap();

        #[cfg(unix)]
        {
            let target = tmp.path().join("symlink-target");
            std::fs::create_dir(&target).unwrap();
            let symlink = worktree_root.join("symlink-child");
            std::os::unix::fs::symlink(&target, &symlink).unwrap();
            let symlink_report = sweep_preparation_cache_for_roots_with_status(
                &main_root,
                std::slice::from_ref(&worktree_root),
                &cache_root,
                None,
                &resource_db_path,
                std::time::Instant::now() + Duration::from_secs(5),
            );
            assert!(!symlink_report.complete);
            assert!(entry.join("cache.bin").is_file());
        }
    }

    #[test]
    fn captured_preparation_sweep_status_is_incomplete_on_busy_or_bad_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let (main_root, worktree_root, cache_root, entry, resource_db_path) =
            preparation_sweep_status_fixture(tmp.path(), b"retain on incomplete");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let request =
            preparation_cache_lease_request(TEST_PREPARATION_REPOSITORY, &cache_root, deadline)
                .unwrap();
        let mut writer = crate::HostResourceCoordinator::open(&resource_db_path).unwrap();
        let grant = writer.acquire(&request).unwrap();

        let busy = sweep_preparation_cache_for_roots_with_status(
            &main_root,
            std::slice::from_ref(&worktree_root),
            &cache_root,
            None,
            &resource_db_path,
            deadline,
        );
        assert!(!busy.complete);
        assert_eq!(busy.entries_removed, 0);
        assert!(entry.join("cache.bin").is_file());
        writer
            .release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .unwrap();

        let empty_roots = sweep_preparation_cache_for_roots_with_status(
            &main_root,
            &[],
            &cache_root,
            None,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );
        assert!(!empty_roots.complete);
        assert!(entry.join("cache.bin").is_file());

        let invalid_root = tmp.path().join("not-a-worktree-root");
        std::fs::write(&invalid_root, b"not a root directory").unwrap();
        let failed_discovery = sweep_preparation_cache_for_roots_with_status(
            &main_root,
            &[invalid_root],
            &cache_root,
            None,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );
        assert!(!failed_discovery.complete);
        assert!(entry.join("cache.bin").is_file());
    }

    #[test]
    fn captured_preparation_sweep_status_is_incomplete_after_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        let (main_root, worktree_root, cache_root, entry, resource_db_path) =
            preparation_sweep_status_fixture(tmp.path(), b"retain after deadline");

        let report = sweep_preparation_cache_for_roots_with_status(
            &main_root,
            &[worktree_root],
            &cache_root,
            None,
            &resource_db_path,
            std::time::Instant::now() - Duration::from_secs(1),
        );

        assert!(!report.complete);
        assert_eq!(report.entries_removed, 0);
        assert_eq!(report.bytes_reclaimed, 0);
        assert_eq!(report.filesystem_entries_reclaimed, 0);
        assert!(entry.join("cache.bin").is_file());
    }

    /// The live set is the keys checkouts would recompute, so an entry is dead
    /// exactly when no checkout names it -- not when it looks old.
    #[test]
    fn a_cache_entry_no_checkout_names_is_the_reclaimable_one() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("preparation-cache");
        let repo_dir = cache.join(TEST_PREPARATION_REPOSITORY);
        std::fs::create_dir_all(repo_dir.join("aaaaaaaaaaaa")).unwrap();
        std::fs::create_dir_all(repo_dir.join("bbbbbbbbbbbb")).unwrap();
        std::fs::write(repo_dir.join("bbbbbbbbbbbb/blob"), "cached\n").unwrap();

        // No live root declares preparation, so nothing keeps a key alive.
        let mut records = crate::measurement::SizeRecords::default();
        let (entries, candidates) =
            inspect_preparation_cache(&cache, &[], crate::SizeScan::Measure, &mut records);
        assert_eq!(entries.len(), 2, "both entries must be inventoried");
        assert_eq!(candidates.len(), 2, "with no live key, both are dead");
        assert!(
            entries.iter().all(|entry| entry.reclaimable),
            "an entry nobody names cannot be live"
        );
        assert!(
            candidates
                .iter()
                .all(|c| c.reason.contains("inputs have changed")),
            "the reason must say why it is dead, not merely that it is"
        );
        assert!(entries.iter().all(|entry| entry.estimated_inodes.is_some()));
        let summary = summarise(&[], &[], &[], &[], &entries, &candidates);
        assert_eq!(summary.preparation_reclaimable_inodes, Some(3));
    }

    #[test]
    fn unmanaged_preparation_cache_layout_is_retained_by_inventory_sweep_and_apply() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let storage_root = tmp.path().join("worktrees");
        let cache_root = tmp.path().join("preparation-cache");
        std::fs::create_dir_all(&main_root).unwrap();

        let foreign_key = cache_root.join("notes/aaaaaaaaaaaa");
        let uppercase_repository = cache_root
            .join(TEST_PREPARATION_REPOSITORY.to_uppercase())
            .join("bbbbbbbbbbbb");
        for path in [&foreign_key, &uppercase_repository] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(path.join("data"), b"unmanaged").unwrap();
        }

        let mut records = crate::measurement::SizeRecords::default();
        let (entries, candidates) =
            inspect_preparation_cache(&cache_root, &[], crate::SizeScan::Recorded, &mut records);
        assert_eq!(entries.len(), 2);
        assert!(candidates.is_empty());
        for path in [
            cache_root.join("notes"),
            uppercase_repository.parent().unwrap().to_path_buf(),
        ] {
            let entry = entries
                .iter()
                .find(|entry| entry.path == path)
                .expect("unmanaged repository is reported in the inventory");
            assert!(!entry.reclaimable, "unmanaged path must be retained");
            assert!(entry.reason.contains("retained"));
        }

        let uppercase_key_cache_root = tmp.path().join("uppercase-key-cache");
        let uppercase_key = uppercase_key_cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("CCCCCCCCCCCC");
        std::fs::create_dir_all(&uppercase_key).unwrap();
        std::fs::write(uppercase_key.join("data"), b"unmanaged").unwrap();
        let mut records = crate::measurement::SizeRecords::default();
        let (key_entries, key_candidates) = inspect_preparation_cache(
            &uppercase_key_cache_root,
            &[],
            crate::SizeScan::Recorded,
            &mut records,
        );
        assert_eq!(key_entries.len(), 1);
        assert!(key_candidates.is_empty());
        assert_eq!(key_entries[0].path, uppercase_key);
        assert!(key_entries[0].reason.contains("retained"));
        assert!(candidates.is_empty());

        let resource_db_path = tmp.path().join("host-state/host-resources.db");
        let invalid_candidates = [
            (
                StoragePreparationCandidate {
                    path: foreign_key,
                    repository: "notes".into(),
                    key: "aaaaaaaaaaaa".into(),
                    estimated_bytes: Some(9),
                    estimated_inodes: Some(1),
                    reason: "forged foreign candidate".into(),
                },
                cache_root.as_path(),
            ),
            (
                StoragePreparationCandidate {
                    path: uppercase_repository.clone(),
                    repository: TEST_PREPARATION_REPOSITORY.to_uppercase(),
                    key: "bbbbbbbbbbbb".into(),
                    estimated_bytes: Some(9),
                    estimated_inodes: Some(1),
                    reason: "forged uppercase repository candidate".into(),
                },
                cache_root.as_path(),
            ),
            (
                StoragePreparationCandidate {
                    path: uppercase_key.clone(),
                    repository: TEST_PREPARATION_REPOSITORY.into(),
                    key: "CCCCCCCCCCCC".into(),
                    estimated_bytes: Some(9),
                    estimated_inodes: Some(1),
                    reason: "forged uppercase key candidate".into(),
                },
                uppercase_key_cache_root.as_path(),
            ),
        ];
        for (candidate, candidate_cache_root) in invalid_candidates {
            let error = apply_preparation_candidate_with_lease_at(
                &candidate,
                &[],
                &resource_db_path,
                candidate_cache_root,
            )
            .expect_err("manual removal must reject unmanaged path shapes");
            assert!(error.contains("managed"));
            assert!(candidate.path.join("data").is_file());
        }
        assert!(
            !resource_db_path.exists(),
            "shape refusal happens before host lease state is opened"
        );

        let resource_db_path = test_host_resource_db(tmp.path());
        for root in [&cache_root, &uppercase_key_cache_root] {
            let (removed, bytes) = sweep_preparation_cache_at(
                &main_root,
                &storage_root,
                root,
                &resource_db_path,
                std::time::Instant::now() + Duration::from_secs(5),
            );
            assert_eq!((removed, bytes), (0, 0));
        }
        assert!(uppercase_repository.join("data").is_file());
        assert!(uppercase_key.join("data").is_file());
    }

    #[test]
    fn retired_preparation_store_uses_original_repository_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let storage_root = tmp.path().join("worktrees");
        let cache_root = tmp.path().join("preparation-cache");
        std::fs::create_dir_all(&main_root).unwrap();
        let retired_name = format!(
            "{TEST_PREPARATION_REPOSITORY}.retired-1700000000000-{}",
            std::process::id()
        );
        let retired_path = cache_root.join(&retired_name);
        let retired_key = retired_path.join("aaaaaaaaaaaa");
        std::fs::create_dir_all(&retired_key).unwrap();
        std::fs::write(retired_key.join("cache.bin"), b"retired store").unwrap();

        let mut records = crate::measurement::SizeRecords::default();
        let (entries, candidates) =
            inspect_preparation_cache(&cache_root, &[], crate::SizeScan::Recorded, &mut records);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].reclaimable);
        assert_eq!(entries[0].key, "");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].repository, retired_name);

        let resource_db_path = test_host_resource_db(tmp.path());
        let lease_request = preparation_cache_lease_request(
            TEST_PREPARATION_REPOSITORY,
            &cache_root,
            std::time::Instant::now() + Duration::from_secs(5),
        )
        .expect("retired paths use the original writer identity");
        let mut writer = crate::HostResourceCoordinator::open(&resource_db_path).unwrap();
        let grant = writer.acquire(&lease_request).unwrap();
        let leased_report = sweep_preparation_cache_at_with_status(
            &main_root,
            &storage_root,
            &cache_root,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );
        assert!(!leased_report.complete);
        assert_eq!(leased_report.entries_removed, 0);
        assert_eq!(leased_report.bytes_reclaimed, 0);
        assert_eq!(leased_report.filesystem_entries_reclaimed, 0);
        assert!(retired_key.join("cache.bin").is_file());
        writer
            .release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .unwrap();

        let report = sweep_preparation_cache_at_with_status(
            &main_root,
            &storage_root,
            &cache_root,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );
        assert!(report.complete);
        assert_eq!(report.entries_removed, 1);
        assert_eq!(report.bytes_reclaimed, b"retired store".len() as u64);
        assert!(report.filesystem_entries_reclaimed >= 2);
        assert!(!retired_path.exists());
    }

    #[test]
    fn retired_preparation_store_names_match_the_writer_suffix_exactly() {
        let retired_zero_timestamp = format!("{TEST_PREPARATION_REPOSITORY}.retired-0-42");
        assert_eq!(
            preparation_cache_repository_identity(TEST_PREPARATION_REPOSITORY),
            Some((TEST_PREPARATION_REPOSITORY, false))
        );
        assert_eq!(
            preparation_cache_repository_identity(&retired_zero_timestamp),
            Some((TEST_PREPARATION_REPOSITORY, true))
        );

        for invalid_name in [
            format!("{TEST_PREPARATION_REPOSITORY}.retired-01-42"),
            format!("{TEST_PREPARATION_REPOSITORY}.retired-1-0"),
            format!("{TEST_PREPARATION_REPOSITORY}.retired-1-42x"),
            format!("{TEST_PREPARATION_REPOSITORY}.retired-1-42-extra"),
            format!(
                "{}.retired-1-42",
                TEST_PREPARATION_REPOSITORY.to_uppercase()
            ),
        ] {
            assert_eq!(
                preparation_cache_repository_identity(&invalid_name),
                None,
                "unexpectedly accepted {invalid_name}"
            );
        }
    }

    /// A checkout that still computes a key keeps its entry, and only that
    /// entry. This is the half that makes the rule safe rather than merely
    /// aggressive: session worktrees are what run `broker prepare`, so if they
    /// did not count as live the cache could be emptied underneath them.
    #[test]
    fn an_entry_a_live_checkout_still_names_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("checkout");
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(repo.join("lock.txt"), "deps\n").unwrap();
        std::fs::write(
            repo.join(".aethyme/prepare.toml"),
            "schema_version = 1\n\n             [[steps]]\n             name = \"deps\"\n             command = [\"sh\", \"-c\", \"true\"]\n             inputs = [\"lock.txt\"]\n             outputs = [\"out/\"]\n             cache = \"repository_shared\"\n             required_for_hooks = false\n",
        )
        .unwrap();

        let key = crate::preparation::current_cache_key(&repo)
            .unwrap()
            .expect("a checkout declaring a shared step computes a key");

        let cache = tmp
            .path()
            .join("preparation-cache")
            .join(TEST_PREPARATION_REPOSITORY);
        std::fs::create_dir_all(cache.join(&key)).unwrap();
        std::fs::create_dir_all(cache.join("deadbeefdead")).unwrap();

        let mut records = crate::measurement::SizeRecords::default();
        let (entries, candidates) = inspect_preparation_cache(
            &tmp.path().join("preparation-cache"),
            std::slice::from_ref(&repo),
            crate::SizeScan::Recorded,
            &mut records,
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(candidates.len(), 1, "only the unnamed key is dead");
        assert_eq!(candidates[0].key, "deadbeefdead");
        let kept = entries.iter().find(|e| e.key == key).unwrap();
        assert!(
            !kept.reclaimable,
            "the key a checkout computes must survive"
        );
        assert!(kept.reason.contains("still computes"));
        let live_candidate = StoragePreparationCandidate {
            path: cache.join(&key),
            repository: TEST_PREPARATION_REPOSITORY.into(),
            key,
            estimated_bytes: None,
            estimated_inodes: None,
            reason: "previously dead".into(),
        };
        let roots = preparation_live_roots(&repo, &[], &[], &BTreeSet::new()).unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());
        assert!(
            apply_preparation_candidate_with_lease_at(
                &live_candidate,
                &roots,
                &resource_db_path,
                &tmp.path().join("preparation-cache"),
            )
            .is_err()
        );
        assert!(live_candidate.path.is_dir());
    }

    /// Reporting spans the cache; removal re-decides at the moment it acts.
    #[test]
    fn removal_refuses_an_entry_that_became_live_again() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = tmp
            .path()
            .join("preparation-cache")
            .join(TEST_PREPARATION_REPOSITORY)
            .join("ccccccccccc1");
        std::fs::create_dir_all(&entry).unwrap();
        let candidate = StoragePreparationCandidate {
            path: entry.clone(),
            repository: TEST_PREPARATION_REPOSITORY.into(),
            key: "ccccccccccc1".into(),
            estimated_bytes: None,
            estimated_inodes: None,
            reason: "dead".into(),
        };

        // A root that cannot produce a key leaves the entry dead, so it goes.
        let resource_db_path = test_host_resource_db(tmp.path());
        let cache_root = tmp.path().join("preparation-cache");
        let removed = apply_preparation_candidate_with_lease_at(
            &candidate,
            &[tmp.path().to_path_buf()],
            &resource_db_path,
            &cache_root,
        );
        assert!(removed.is_ok(), "a dead entry is removable: {removed:?}");
        assert!(!entry.exists(), "the entry must actually be gone");

        // Removing what is already gone is a refusal, not a silent success.
        assert!(
            apply_preparation_candidate_with_lease_at(
                &candidate,
                &[],
                &resource_db_path,
                &cache_root,
            )
            .is_err()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn manual_preparation_cache_apply_refuses_a_replaced_captured_root() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path().join("preparation-cache");
        let original_entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("cccccccccccc");
        std::fs::create_dir_all(&original_entry).unwrap();
        let root_identity =
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&cache_root)
                .unwrap()
                .encode()
                .unwrap();
        let original_root = tmp.path().join("preparation-cache-original");
        std::fs::rename(&cache_root, &original_root).unwrap();
        let replacement_entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("cccccccccccc");
        std::fs::create_dir_all(&replacement_entry).unwrap();
        std::fs::write(replacement_entry.join("payload"), b"replacement").unwrap();
        let candidate = StoragePreparationCandidate {
            path: replacement_entry.clone(),
            repository: TEST_PREPARATION_REPOSITORY.into(),
            key: "cccccccccccc".into(),
            estimated_bytes: None,
            estimated_inodes: None,
            reason: "previously reviewed dead entry".into(),
        };
        let resource_db_path = tmp.path().join("host-state/host-resources.db");

        let error = apply_preparation_candidate_with_root_identity(
            &candidate,
            &[],
            &resource_db_path,
            &cache_root,
            Some(&root_identity),
        )
        .expect_err("a replacement root must not inherit stale deletion authority");

        assert!(error.contains("captured preparation-cache root changed"));
        assert_eq!(
            std::fs::read(replacement_entry.join("payload")).unwrap(),
            b"replacement"
        );
        assert!(
            original_root
                .join(TEST_PREPARATION_REPOSITORY)
                .join("cccccccccccc")
                .is_dir(),
            "the original root remains intact"
        );
        assert!(
            !resource_db_path.exists(),
            "root mismatch is rejected before opening the lease database"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn assert_sweep_revalidates_outer_binding_between_unlinks(retired: bool) {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = tmp.path().canonicalize().unwrap();
        let outer_root = tmp_root.as_path().join("preparation-cache-root");
        let main_root = tmp_root.as_path().join("main");
        std::fs::create_dir(&outer_root).unwrap();
        std::fs::create_dir(&main_root).unwrap();
        let resource_db_path = test_host_resource_db(tmp_root.as_path());
        let binding = initialized_storage_cache_root_binding(&outer_root, &resource_db_path);
        let store = if retired {
            binding.path.join(format!(
                "{TEST_PREPARATION_REPOSITORY}.retired-1700000000000-42"
            ))
        } else {
            binding.path.join(TEST_PREPARATION_REPOSITORY)
        };
        let entry = store.join("cccccccccccc");
        write_two_cache_payloads(&entry);

        let mut marker_changed = false;
        let mut observer = || {
            if !marker_changed && remaining_cache_payloads(&entry) == 1 {
                change_cache_root_coordinator(&binding);
                marker_changed = true;
            }
            Ok(())
        };
        let report = sweep_preparation_cache_with_discovery_and_observer(
            &binding.path,
            binding.identity.as_deref(),
            Some(&binding),
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
            |_| Ok(vec![main_root.clone()]),
            &mut observer,
        );

        assert!(marker_changed, "the test must alter the marker mid-delete");
        assert!(!report.complete, "a changed outer binding stops the sweep");
        assert!(entry.is_dir(), "the cache entry remains recoverable");
        assert_eq!(remaining_cache_payloads(&entry), 1);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn dead_cache_sweep_revalidates_outer_binding_before_each_unlink() {
        assert_sweep_revalidates_outer_binding_between_unlinks(false);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn retired_cache_sweep_revalidates_outer_binding_before_each_unlink() {
        assert_sweep_revalidates_outer_binding_between_unlinks(true);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn manual_apply_revalidates_outer_binding_before_each_unlink() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = tmp.path().canonicalize().unwrap();
        let outer_root = tmp_root.as_path().join("preparation-cache-root");
        std::fs::create_dir(&outer_root).unwrap();
        let resource_db_path = test_host_resource_db(tmp_root.as_path());
        let binding = initialized_storage_cache_root_binding(&outer_root, &resource_db_path);
        let entry = binding
            .path
            .join(TEST_PREPARATION_REPOSITORY)
            .join("cccccccccccc");
        write_two_cache_payloads(&entry);
        let candidate = StoragePreparationCandidate {
            path: entry.clone(),
            repository: TEST_PREPARATION_REPOSITORY.into(),
            key: "cccccccccccc".into(),
            estimated_bytes: None,
            estimated_inodes: None,
            reason: "previously reviewed dead cache entry".into(),
        };
        let mut marker_changed = false;
        let mut observer = || {
            if !marker_changed && remaining_cache_payloads(&entry) == 1 {
                change_cache_root_coordinator(&binding);
                marker_changed = true;
            }
            Ok(())
        };

        let error = apply_preparation_candidate_with_binding_and_observer(
            &candidate,
            &[],
            &resource_db_path,
            &binding,
            &mut observer,
        )
        .expect_err("manual apply must stop after the captured root marker changes");

        assert!(marker_changed, "the test must alter the marker mid-delete");
        assert!(error.contains("captured preparation-cache root changed"));
        assert!(entry.is_dir());
        assert_eq!(remaining_cache_payloads(&entry), 1);
    }

    #[test]
    fn preparation_cache_sweep_skips_a_repository_with_an_active_writer_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let storage_root = tmp.path().join("worktrees");
        let cache_root = tmp.path().join("preparation-cache");
        std::fs::create_dir_all(&main_root).unwrap();
        std::fs::create_dir_all(&storage_root).unwrap();
        let entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("aaaaaaaaaaaa");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), b"writer protected").unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let request =
            preparation_cache_lease_request(TEST_PREPARATION_REPOSITORY, &cache_root, deadline)
                .expect("lease request is valid");
        let mut writer = crate::HostResourceCoordinator::open(&resource_db_path).unwrap();
        let grant = writer.acquire(&request).unwrap();

        let busy_report = sweep_preparation_cache_at_with_status(
            &main_root,
            &storage_root,
            &cache_root,
            &resource_db_path,
            deadline,
        );

        assert!(!busy_report.complete);
        assert_eq!(busy_report.entries_removed, 0);
        assert_eq!(busy_report.bytes_reclaimed, 0);
        assert_eq!(busy_report.filesystem_entries_reclaimed, 0);
        assert!(entry.join("cache.bin").exists());
        writer
            .release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .unwrap();

        let resumed_report = sweep_preparation_cache_at_with_status(
            &main_root,
            &storage_root,
            &cache_root,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );
        assert!(resumed_report.complete);
        assert_eq!(resumed_report.entries_removed, 1);
        assert_eq!(resumed_report.bytes_reclaimed, 16);
        assert!(resumed_report.filesystem_entries_reclaimed >= 2);
        assert!(!entry.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn preparation_cache_sweep_recovers_rotation_debt_without_a_later_writer() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let main_root = root.join("main");
        let cache_root = root.join("preparation-cache");
        let store = cache_root.join(TEST_PREPARATION_REPOSITORY);
        std::fs::create_dir_all(&main_root).unwrap();
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("old-cache"), b"interrupted old cache").unwrap();

        let cache_root_identity =
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&cache_root).unwrap();
        let store_identity =
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&store).unwrap();
        let debt = crate::preparation::rotation_debt::RotationDebt::new(
            TEST_PREPARATION_REPOSITORY,
            &crate::preparation::rotation_debt::new_transaction_id(),
            cache_root_identity,
            store_identity,
        )
        .unwrap();
        crate::preparation::rotation_debt::publish(&cache_root, debt).unwrap();
        let retained =
            crate::preparation::rotation_debt::read(&cache_root, TEST_PREPARATION_REPOSITORY)
                .unwrap()
                .expect("rotation intent is durable before the rename");
        crate::preparation::rotation_debt::retire_original_store(&cache_root, &retained).unwrap();

        let resource_db_path = test_host_resource_db(&root);
        let report = sweep_preparation_cache_with_discovery(
            &cache_root,
            None,
            None,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
            |_| Ok(vec![main_root.clone()]),
        );

        assert!(report.complete, "the detached sweep finishes durable debt");
        assert_eq!(report.entries_removed, 1);
        assert!(report.filesystem_entries_reclaimed > 0);
        assert!(
            store.is_dir(),
            "recovery leaves a fresh store for the next writer"
        );
        assert_eq!(std::fs::read_dir(&store).unwrap().count(), 0);
        assert!(!store.join("old-cache").exists());
        assert!(
            crate::preparation::rotation_debt::read(&cache_root, TEST_PREPARATION_REPOSITORY,)
                .unwrap()
                .is_none()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn preparation_cache_sweep_recovers_published_external_debt_without_cache_witness() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let main_root = root.join("main");
        let cache_root = root.join("preparation-cache");
        let store = cache_root.join(TEST_PREPARATION_REPOSITORY);
        std::fs::create_dir_all(&main_root).unwrap();
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join("old-cache"),
            b"interrupted before witness publication",
        )
        .unwrap();

        let resource_db_path = test_host_resource_db(&root);
        let root_identity =
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&cache_root).unwrap();
        let debt = crate::preparation::rotation_debt::RotationDebt::new(
            TEST_PREPARATION_REPOSITORY,
            &crate::preparation::rotation_debt::new_transaction_id(),
            root_identity.clone(),
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&store).unwrap(),
        )
        .unwrap();
        let payload = crate::preparation::rotation_debt::encode(&debt).unwrap();
        crate::preparation::rotation_journal::publish(
            &resource_db_path,
            &cache_root,
            &root_identity.encode().unwrap(),
            TEST_PREPARATION_REPOSITORY,
            &payload,
        )
        .unwrap();
        assert!(
            crate::preparation::rotation_debt::read(&cache_root, TEST_PREPARATION_REPOSITORY)
                .unwrap()
                .is_none(),
            "fixture represents a crash after external publication but before the in-cache witness"
        );

        let report = sweep_preparation_cache_with_discovery(
            &cache_root,
            None,
            None,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
            |_| Ok(vec![main_root.clone()]),
        );

        assert!(
            report.complete,
            "the full cache sweep repairs the journaled transition"
        );
        assert_eq!(report.entries_removed, 1);
        assert!(report.filesystem_entries_reclaimed >= 3);
        assert!(
            store.is_dir(),
            "recovery leaves a fresh store for the next writer"
        );
        assert_eq!(std::fs::read_dir(&store).unwrap().count(), 0);
        assert!(!store.join("old-cache").exists());
        assert!(
            crate::preparation::rotation_debt::read(&cache_root, TEST_PREPARATION_REPOSITORY)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            crate::preparation::rotation_journal::probe(
                &resource_db_path,
                &cache_root,
                Some(&root_identity.encode().unwrap()),
            )
            .unwrap(),
            Some(crate::preparation::rotation_journal::JournalProbe::NoDebt),
            "the durable row is acknowledged only after recovery and the complete sweep"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn preparation_cache_sweep_does_not_complete_when_cache_parent_disappears() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let cache_parent = tmp.path().join("host-cache");
        let cache_root = cache_parent.join("preparation-cache");
        std::fs::create_dir_all(&main_root).unwrap();
        std::fs::create_dir_all(&cache_root).unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());

        let report = sweep_preparation_cache_with_discovery(
            &cache_root,
            None,
            None,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
            |_| {
                std::fs::remove_dir(&cache_root).unwrap();
                std::fs::remove_dir(&cache_parent).unwrap();
                Ok(vec![main_root.clone()])
            },
        );

        assert!(!report.complete);
        assert!(!cache_root.exists());
        assert!(!cache_parent.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn preparation_cache_sweep_never_uses_legacy_cleanup_for_unwitnessed_retired_store() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let cache_root = tmp.path().join("preparation-cache");
        let retired = cache_root.join(format!("{}.retired", TEST_PREPARATION_REPOSITORY));
        std::fs::create_dir_all(&main_root).unwrap();
        std::fs::create_dir_all(&retired).unwrap();
        std::fs::write(retired.join("old-cache"), b"preserve unknown ownership").unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());

        let report = sweep_preparation_cache_with_discovery(
            &cache_root,
            None,
            None,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
            |_| Ok(vec![main_root.clone()]),
        );

        assert!(
            !report.complete,
            "unwitnessed ownership keeps the pass pending"
        );
        assert_eq!(report.entries_removed, 0);
        assert_eq!(report.bytes_reclaimed, 0);
        assert_eq!(report.filesystem_entries_reclaimed, 0);
        assert_eq!(
            std::fs::read(retired.join("old-cache")).unwrap(),
            b"preserve unknown ownership"
        );
    }

    #[test]
    fn reviewed_preparation_removal_refuses_an_active_writer_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path().join("preparation-cache");
        let entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("bbbbbbbbbbbb");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), b"writer protected").unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());
        let request = preparation_cache_lease_request(
            TEST_PREPARATION_REPOSITORY,
            &cache_root,
            std::time::Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        let mut writer = crate::HostResourceCoordinator::open(&resource_db_path).unwrap();
        let grant = writer.acquire(&request).unwrap();
        let candidate = StoragePreparationCandidate {
            path: entry.clone(),
            repository: TEST_PREPARATION_REPOSITORY.into(),
            key: "bbbbbbbbbbbb".into(),
            estimated_bytes: Some(16),
            estimated_inodes: Some(1),
            reason: "reviewed dead entry".into(),
        };

        let result = apply_preparation_candidate_with_lease_at(
            &candidate,
            &[],
            &resource_db_path,
            &cache_root,
        );

        assert!(result.unwrap_err().contains("currently in use"));
        assert!(entry.join("cache.bin").exists());
        writer
            .release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .unwrap();
    }

    /// Unknown preparation inputs never authorize cache deletion.
    #[test]
    fn unknown_preparation_liveness_never_authorizes_deletion() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("checkout");
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(repo.join(".aethyme/prepare.toml"), "invalid = [").unwrap();
        let cache = tmp.path().join("preparation-cache");
        let path = cache.join(TEST_PREPARATION_REPOSITORY).join("cccccccccccc");
        std::fs::create_dir_all(&path).unwrap();
        let candidate = StoragePreparationCandidate {
            path: path.clone(),
            repository: TEST_PREPARATION_REPOSITORY.into(),
            key: "cccccccccccc".into(),
            estimated_bytes: None,
            estimated_inodes: None,
            reason: "previously unused".into(),
        };
        let mut records = crate::measurement::SizeRecords::default();
        let (entries, candidates) = inspect_preparation_cache(
            &cache,
            std::slice::from_ref(&repo),
            crate::SizeScan::Recorded,
            &mut records,
        );
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].reclaimable);
        assert!(entries[0].reason.contains("could not be verified"));
        assert!(candidates.is_empty());
        let resource_db_path = test_host_resource_db(tmp.path());
        assert!(
            apply_preparation_candidate_with_lease_at(
                &candidate,
                &[repo],
                &resource_db_path,
                &cache,
            )
            .is_err()
        );
        assert!(path.is_dir());
    }

    #[test]
    fn inline_preparation_sweep_stops_when_its_deadline_is_expired() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let storage_root = tmp.path().join("worktrees");
        let cache_root = tmp.path().join("preparation-cache");
        std::fs::create_dir_all(&main_root).unwrap();
        std::fs::create_dir_all(&storage_root).unwrap();
        let entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("aaaaaaaaaaaa");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), b"keep until verified").unwrap();

        let report = sweep_preparation_cache_at_with_status(
            &main_root,
            &storage_root,
            &cache_root,
            &tmp.path().join("host-resource.db"),
            std::time::Instant::now() - Duration::from_millis(1),
        );

        assert!(!report.complete);
        assert_eq!(report.entries_removed, 0);
        assert_eq!(report.bytes_reclaimed, 0);
        assert_eq!(report.filesystem_entries_reclaimed, 0);
        assert!(entry.join("cache.bin").exists());
    }

    #[test]
    fn inline_preparation_sweep_retains_entries_when_resource_state_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let storage_root = tmp.path().join("worktrees");
        let cache_root = tmp.path().join("preparation-cache");
        let resource_db_path = tmp.path().join("host-state/host-resources.db");
        std::fs::create_dir_all(&main_root).unwrap();
        std::fs::create_dir_all(&storage_root).unwrap();
        let entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("aaaaaaaaaaaa");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), b"retain without lease registry").unwrap();

        let report = sweep_preparation_cache_at_with_status(
            &main_root,
            &storage_root,
            &cache_root,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );

        assert!(!report.complete);
        assert_eq!(report.entries_removed, 0);
        assert_eq!(report.bytes_reclaimed, 0);
        assert_eq!(report.filesystem_entries_reclaimed, 0);
        assert!(entry.join("cache.bin").exists());
        assert!(
            !resource_db_path.exists(),
            "the sweep must not create host state"
        );
    }

    #[test]
    fn storage_cache_root_uses_the_host_resource_database_parent() {
        let database = crate::default_host_resource_db_path().unwrap();
        let expected = database
            .parent()
            .expect("host resource database has a parent")
            .join("preparation-cache");

        assert_eq!(
            crate::preparation::preparation_cache_root().unwrap(),
            expected
        );
    }

    #[test]
    fn worktree_volume_cache_child_is_infrastructure_only_when_marker_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let repository = init_storage_root_test_repository(&tmp.path().join("repository"));
        let owner_root = repository.root().canonicalize().unwrap();
        let storage_root = tmp.path().join("worktrees").join("repository-key");
        std::fs::create_dir_all(&storage_root).unwrap();
        let marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: "repository-key".into(),
            repository_root: owner_root,
        };
        std::fs::write(
            storage_root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        let cache_child =
            storage_root.join(crate::broker::lifecycle::WORKTREE_VOLUME_PREPARATION_CACHE_ROOT);
        std::fs::create_dir(&cache_child).unwrap();

        let mut records = crate::measurement::SizeRecords::default();
        let unknown = inspect_root(
            &storage_root,
            now_ms(),
            7,
            crate::SizeScan::Recorded,
            &mut records,
            &BTreeSet::new(),
        );
        assert!(
            unknown.blockers.iter().any(|blocker| blocker.contains(
                "reserved worktree-volume preparation-cache root without a valid repository-bound marker"
            )),
            "an unknown reserved child must block storage inventory: {:?}",
            unknown.blockers
        );
        assert!(unknown.reconciliation.entries.is_empty());
        assert!(!unknown.empty, "the child cannot make a root look empty");

        let verified_paths = BTreeSet::from([cache_child.clone()]);
        let verified = inspect_root(
            &storage_root,
            now_ms(),
            7,
            crate::SizeScan::Recorded,
            &mut records,
            &verified_paths,
        );
        assert!(
            !verified
                .blockers
                .iter()
                .any(|blocker| blocker.contains("reserved worktree-volume preparation-cache root")),
            "a valid cache child is infrastructure, not a checkout: {:?}",
            verified.blockers
        );
        assert!(
            verified
                .reconciliation
                .entries
                .iter()
                .all(|entry| entry.path != cache_child),
            "the cache child must not enter checkout reconciliation"
        );
        assert!(!verified.empty, "a cache child keeps the root nonempty");
    }

    #[test]
    fn an_unknown_worktree_root_protects_every_inline_cache_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let storage_root = tmp.path().join("worktrees");
        let cache_root = tmp.path().join("preparation-cache");
        std::fs::create_dir_all(&main_root).unwrap();
        let unmarked_root = storage_root.join("unattributed");
        std::fs::create_dir_all(&unmarked_root).unwrap();
        let entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("bbbbbbbbbbbb");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), b"retain on unknown liveness").unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());

        let report = sweep_preparation_cache_at_with_status(
            &main_root,
            &storage_root,
            &cache_root,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );

        assert!(!report.complete);
        assert_eq!(report.entries_removed, 0);
        assert_eq!(report.bytes_reclaimed, 0);
        assert_eq!(report.filesystem_entries_reclaimed, 0);
        assert!(entry.join("cache.bin").exists());
        assert!(unmarked_root.is_dir());
    }

    #[test]
    fn inline_preparation_sweep_removes_a_dead_entry_with_complete_liveness() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let storage_root = tmp.path().join("worktrees");
        let cache_root = tmp.path().join("preparation-cache");
        std::fs::create_dir_all(&main_root).unwrap();
        let worktree_root = storage_root.join("repository");
        std::fs::create_dir_all(&worktree_root).unwrap();
        let marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: "repository".into(),
            repository_root: main_root.clone(),
        };
        std::fs::write(
            worktree_root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        let entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("cccccccccccc");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), b"dead").unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());

        let report = sweep_preparation_cache_at_with_status(
            &main_root,
            &storage_root,
            &cache_root,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );

        assert!(report.complete);
        assert_eq!(report.entries_removed, 1);
        assert_eq!(report.bytes_reclaimed, 4);
        assert_eq!(report.filesystem_entries_reclaimed, 2);
        assert!(!entry.exists());
    }

    #[test]
    fn inline_preparation_sweep_keeps_a_sibling_session_key_and_removes_a_dead_key() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let sibling_repository = tmp.path().join("sibling-repository");
        let storage_root = tmp.path().join("worktrees");
        let cache_root = tmp.path().join("preparation-cache");
        std::fs::create_dir_all(&main_root).unwrap();
        std::fs::create_dir_all(&sibling_repository).unwrap();

        let worktree_root = storage_root.join("sibling");
        let session_root = worktree_root.join("session-1");
        std::fs::create_dir_all(&session_root).unwrap();
        let marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: "sibling".into(),
            repository_root: sibling_repository,
        };
        std::fs::write(
            worktree_root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(session_root.join(".aethyme")).unwrap();
        std::fs::write(session_root.join("lock.txt"), "sibling deps\n").unwrap();
        std::fs::write(
            session_root.join(".aethyme/prepare.toml"),
            "schema_version = 1\n\n[[steps]]\nname = \"deps\"\ncommand = [\"sh\", \"-c\", \"true\"]\ninputs = [\"lock.txt\"]\noutputs = [\"out/\"]\ncache = \"repository_shared\"\nrequired_for_hooks = false\n",
        )
        .unwrap();
        let live_key = crate::preparation::current_cache_key(&session_root)
            .unwrap()
            .expect("sibling session declares a shared preparation step");
        let dead_key = "ffffffffffff";
        assert_ne!(live_key, dead_key);

        let live_entry = cache_root.join(TEST_PREPARATION_REPOSITORY).join(&live_key);
        let dead_entry = cache_root.join(TEST_PREPARATION_REPOSITORY).join(dead_key);
        std::fs::create_dir_all(&live_entry).unwrap();
        std::fs::write(live_entry.join("cache.bin"), b"live").unwrap();
        std::fs::create_dir_all(&dead_entry).unwrap();
        std::fs::write(dead_entry.join("cache.bin"), b"dead").unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());

        let (removed, bytes) = sweep_preparation_cache_at(
            &main_root,
            &storage_root,
            &cache_root,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );

        assert_eq!((removed, bytes), (1, 4));
        assert!(
            live_entry.is_dir(),
            "the sibling session's live key survives"
        );
        assert!(!dead_entry.exists(), "the unnamed cache key is removed");
    }

    #[cfg(unix)]
    #[test]
    fn inline_preparation_sweep_retains_entries_when_config_is_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let storage_root = tmp.path().join("worktrees");
        let cache_root = tmp.path().join("preparation-cache");
        std::fs::create_dir_all(main_root.join(".aethyme")).unwrap();
        std::fs::create_dir_all(&storage_root).unwrap();
        let external_config = tmp.path().join("prepare.toml");
        std::fs::write(&external_config, "schema_version = 1\n").unwrap();
        std::os::unix::fs::symlink(&external_config, main_root.join(".aethyme/prepare.toml"))
            .unwrap();
        let entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("dddddddddddd");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), b"retain").unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());

        let (removed, bytes) = sweep_preparation_cache_at(
            &main_root,
            &storage_root,
            &cache_root,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );

        assert_eq!((removed, bytes), (0, 0));
        assert!(
            entry.is_dir(),
            "unknown config provenance retains cache entries"
        );
    }

    #[test]
    fn inline_preparation_sweep_retains_entries_when_config_is_oversized() {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("main");
        let storage_root = tmp.path().join("worktrees");
        let cache_root = tmp.path().join("preparation-cache");
        std::fs::create_dir_all(main_root.join(".aethyme")).unwrap();
        std::fs::create_dir_all(&storage_root).unwrap();
        std::fs::write(
            main_root.join(".aethyme/prepare.toml"),
            vec![b'x'; 2 * 1024 * 1024],
        )
        .unwrap();
        let entry = cache_root
            .join(TEST_PREPARATION_REPOSITORY)
            .join("eeeeeeeeeeee");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("cache.bin"), b"retain").unwrap();
        let resource_db_path = test_host_resource_db(tmp.path());

        let (removed, bytes) = sweep_preparation_cache_at(
            &main_root,
            &storage_root,
            &cache_root,
            &resource_db_path,
            std::time::Instant::now() + Duration::from_secs(5),
        );

        assert_eq!((removed, bytes), (0, 0));
        assert!(
            entry.is_dir(),
            "unbounded config data retains cache entries"
        );
    }

    /// A primary checkout has no session and no close event, so the only
    /// evidence that nothing is building is that the tree stopped moving.
    /// `target/` is git-ignored, so a running build leaves the checkout clean
    /// and every other condition satisfied.
    #[test]
    fn an_artifact_that_is_still_changing_is_not_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir_all(target.join("debug")).unwrap();

        assert!(
            directory_modified_within(&target, PRIMARY_ARTIFACT_IDLE_MS),
            "a directory just written to must read as busy"
        );

        // Nothing readable must ever read as idle: not knowing is not safety.
        assert!(directory_modified_within(
            &tmp.path().join("absent"),
            PRIMARY_ARTIFACT_IDLE_MS
        ));
    }

    /// A zero-length window is the boundary the guard turns on: with no window
    /// at all, a tree that is not being written to answers "idle".
    #[test]
    fn a_settled_artifact_answers_idle_once_its_window_closes() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        assert!(
            !directory_modified_within(&target, 0),
            "a tree nobody is writing to must become reclaimable"
        );
    }

    #[test]
    fn decision_digest_ignores_measured_bytes() {
        let candidate = StorageCandidate {
            kind: StorageCandidateKind::StrayDirectory,
            path: PathBuf::from("/storage/root/worktree"),
            root_path: PathBuf::from("/storage/root"),
            repository_key: "repo".into(),
            repository_root: PathBuf::from("/repo"),
            git_marker: false,
            estimated_bytes: Some(10),
            estimated_inodes: Some(3),
            reason: "stray".into(),
            marker_sha256: "marker".into(),
        };
        let mut changed = candidate.clone();
        changed.estimated_bytes = Some(100);
        changed.estimated_inodes = Some(99);
        assert_eq!(
            decision_digest(
                Path::new("/storage"),
                DecisionRootMetadata::default(),
                1,
                &[candidate],
                &[],
                &[]
            ),
            decision_digest(
                Path::new("/storage"),
                DecisionRootMetadata::default(),
                1,
                &[changed],
                &[],
                &[]
            )
        );
    }

    #[test]
    fn apply_revalidation_uses_recorded_sizes_and_keeps_unknown_sizes_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let repository_root = tmp.path().join("repository");
        let repository = init_storage_root_test_repository(&repository_root);
        let owner = crate::test_storage::TestStorage::new();
        let raw_storage_root = owner.root().join("orphan-worktrees");
        let candidate_tree = raw_storage_root.join("session-tree");
        std::fs::create_dir_all(&candidate_tree).unwrap();
        let storage_root = normalise(&raw_storage_root);
        let candidate_tree = storage_root.join("session-tree");
        std::fs::write(candidate_tree.join("payload.bin"), vec![b'x'; 4096]).unwrap();
        let marker = crate::broker::WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: "removed-repository".into(),
            repository_root: owner.root().join("removed-repository"),
        };
        std::fs::write(
            storage_root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();

        let root_key = storage_root.to_string_lossy().into_owned();
        let mut records = crate::measurement::SizeRecords::default();
        records.record_usage(&root_key, 0, Some(1), 1);
        let mut enumerate_children = preparation_live_root_children;
        let plan = build_apply_plan(
            repository.root(),
            &StorageRootLayout::DirectRoot(storage_root.clone()),
            None,
            None,
            None,
            0,
            Vec::new(),
            &mut records,
            &[],
            None,
            &mut enumerate_children,
        )
        .unwrap();

        assert_eq!(plan.candidates.len(), 1);
        assert_eq!(plan.candidates[0].kind, StorageCandidateKind::OrphanRoot);
        assert_eq!(plan.candidates[0].estimated_bytes, Some(0));
        assert_eq!(plan.roots[0].estimated_bytes, Some(0));
        assert_eq!(plan.roots[0].reconciliation.entries.len(), 1);
        assert_eq!(
            plan.roots[0].reconciliation.entries[0].estimated_bytes, None,
            "a missing record must remain unknown during apply revalidation"
        );
        assert_eq!(
            records.records.len(),
            1,
            "apply revalidation must not fill records by walking the inventory"
        );
        assert!(
            std::fs::metadata(candidate_tree.join("payload.bin"))
                .unwrap()
                .len()
                > plan.candidates[0].estimated_bytes.unwrap(),
            "stale byte estimates are not a candidate-safety input"
        );
    }

    #[test]
    fn decision_digest_binds_preparation_identity_not_measurements() {
        let candidate = StoragePreparationCandidate {
            path: PathBuf::from("/cache/repository/key"),
            repository: "repository".into(),
            key: "key".into(),
            estimated_bytes: Some(1),
            estimated_inodes: None,
            reason: "unused".into(),
        };
        let digest = |candidates: &[StoragePreparationCandidate]| {
            decision_digest(
                Path::new("/storage"),
                DecisionRootMetadata::default(),
                1,
                &[],
                &[],
                candidates,
            )
        };
        let approved = digest(std::slice::from_ref(&candidate));
        assert_ne!(approved, digest(&[]));
        let root_a = [1_u8, 2, 3];
        let root_b = [1_u8, 2, 4];
        assert_ne!(
            decision_digest(
                Path::new("/storage"),
                DecisionRootMetadata {
                    preparation_cache_root_identity: Some(&root_a),
                    ..DecisionRootMetadata::default()
                },
                1,
                &[],
                &[],
                std::slice::from_ref(&candidate),
            ),
            decision_digest(
                Path::new("/storage"),
                DecisionRootMetadata {
                    preparation_cache_root_identity: Some(&root_b),
                    ..DecisionRootMetadata::default()
                },
                1,
                &[],
                &[],
                std::slice::from_ref(&candidate),
            ),
            "the confirmation must bind the captured cache-root instance",
        );
        assert_ne!(
            decision_digest(
                Path::new("/storage"),
                DecisionRootMetadata {
                    preparation_cache_root_identity: Some(&root_a),
                    preparation_cache_root_nonce: Some("a".repeat(64).as_str()),
                    ..DecisionRootMetadata::default()
                },
                1,
                &[],
                &[],
                std::slice::from_ref(&candidate),
            ),
            decision_digest(
                Path::new("/storage"),
                DecisionRootMetadata {
                    preparation_cache_root_identity: Some(&root_a),
                    preparation_cache_root_nonce: Some("b".repeat(64).as_str()),
                    ..DecisionRootMetadata::default()
                },
                1,
                &[],
                &[],
                std::slice::from_ref(&candidate),
            ),
            "the confirmation must bind the cache-root instance nonce",
        );
        assert_eq!(
            decision_digest(
                Path::new("/storage"),
                DecisionRootMetadata::default(),
                1,
                &[],
                &[],
                &[],
            ),
            "0052d7a07108db59d67115eba0c2f99b6f277169e0dae7cdfbba91bc8168b415",
            "absence of an override preserves the legacy decision payload",
        );
        assert_ne!(
            decision_digest(
                Path::new("/storage"),
                DecisionRootMetadata::default(),
                1,
                &[],
                &[],
                &[],
            ),
            decision_digest(
                Path::new("/storage"),
                DecisionRootMetadata {
                    storage_root_layout: Some("direct_root"),
                    ..DecisionRootMetadata::default()
                },
                1,
                &[],
                &[],
                &[],
            ),
            "the confirmation must bind a direct-root layout",
        );
        let mut measured = candidate.clone();
        measured.estimated_bytes = Some(999);
        measured.estimated_inodes = Some(999);
        measured.reason = "updated explanation".into();
        assert_eq!(approved, digest(&[measured]));
        for field in 0..3 {
            let mut changed = candidate.clone();
            match field {
                0 => changed.path = PathBuf::from("/cache/repository/other"),
                1 => changed.repository = "other".into(),
                _ => changed.key = "other".into(),
            }
            assert_ne!(approved, digest(&[changed]));
        }
    }

    #[test]
    fn confirmation_accepts_only_a_full_hex_digest() {
        assert!(is_sha256(&"a".repeat(64)));
        assert!(!is_sha256("a"));
        assert!(!is_sha256(&"g".repeat(64)));
    }

    #[test]
    fn recorded_size_uses_the_cache_and_leaves_missing_paths_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("target");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("output"), "small").unwrap();

        let mut records = crate::measurement::SizeRecords::default();
        assert_eq!(
            observed_size(&path, crate::SizeScan::Recorded, &mut records),
            None
        );

        let key = path.to_string_lossy().into_owned();
        records.record(&key, 5, 123);
        std::fs::write(path.join("output"), vec![b'x'; 4096]).unwrap();
        assert_eq!(
            observed_size(&path, crate::SizeScan::Recorded, &mut records),
            Some(5),
            "recorded mode must not refresh a directory by walking it"
        );
    }

    #[test]
    fn incomplete_storage_totals_remain_unknown() {
        assert_eq!(sized_sum([Some(1), Some(2)].into_iter()), Some(3));
        assert_eq!(sized_sum([Some(1), None].into_iter()), None);
    }

    /// Between review and apply a `broker start` may put a worktree in the
    /// root. The removal re-checks and leaves everything in place.
    #[test]
    fn removing_an_empty_root_refuses_content_that_appeared_after_review() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join(".cargo")).unwrap();
        std::fs::write(root.join(WORKTREE_ROOT_MARKER), "{}\n").unwrap();
        std::fs::write(
            root.join(".cargo/config.toml"),
            crate::broker::WORKTREE_CARGO_CONFIG,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("new-session")).unwrap();

        let error = remove_empty_root(&root).unwrap_err();
        assert!(error.contains("new-session"), "{error}");
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
        assert!(root.join(".cargo/config.toml").is_file());

        std::fs::remove_dir(root.join("new-session")).unwrap();
        assert!(remove_empty_root(&root).unwrap() > 0);
        assert!(!root.exists());
    }

    #[test]
    fn durable_binding_retains_empty_root_but_allows_stray_child_review() {
        let tmp = tempfile::tempdir().unwrap();
        let owner_root = tmp.path().join("repository");
        init_storage_root_test_repository(&owner_root);
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&owner_root).unwrap();
        broker.prepare_worktree_root_for_submission().unwrap();

        let binding = broker.store().repository_worktree_binding().unwrap();
        assert_eq!(
            binding.state,
            crate::store::repository_binding::RepositoryWorktreeBindingState::Bound
        );
        let root = binding.worktree_root.unwrap();
        let marker_observation = read_marker(&root);
        let marker = marker_observation.marker.unwrap();
        let marker_bytes = marker_observation.bytes.unwrap();
        let stale_empty_candidate = StorageCandidate {
            kind: StorageCandidateKind::EmptyRoot,
            path: root.clone(),
            root_path: root.clone(),
            repository_key: marker.repository_key,
            repository_root: owner_root.clone(),
            git_marker: false,
            estimated_bytes: None,
            estimated_inodes: None,
            reason: "fixture reviewed empty root".into(),
            marker_sha256: format!("{:x}", Sha256::digest(marker_bytes)),
        };

        let mut records = crate::measurement::SizeRecords::default();
        let empty_root = inspect_root(
            &root,
            now_ms(),
            0,
            crate::SizeScan::Recorded,
            &mut records,
            &BTreeSet::new(),
        );
        assert!(empty_root.empty);
        assert!(
            candidates_for_root(&empty_root, 0)
                .iter()
                .all(|candidate| candidate.kind != StorageCandidateKind::EmptyRoot),
            "a durable binding keeps root bookkeeping out of reclamation candidates"
        );
        let error = apply_candidate(
            &stale_empty_candidate,
            &StorageRootLayout::DirectRoot(root.clone()),
        )
        .unwrap_err();
        assert!(
            error.contains("durable worktree-root binding recheck"),
            "{error}"
        );
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());

        let stray = root.join("unregistered-child");
        std::fs::create_dir(&stray).unwrap();
        let nonempty_root = inspect_root(
            &root,
            now_ms(),
            0,
            crate::SizeScan::Recorded,
            &mut records,
            &BTreeSet::new(),
        );
        assert!(!nonempty_root.empty);
        assert!(
            candidates_for_root(&nonempty_root, 0)
                .iter()
                .any(|candidate| {
                    candidate.kind == StorageCandidateKind::StrayDirectory
                        && candidate.path == stray
                }),
            "binding a root must not hide an otherwise eligible stray child"
        );
    }

    #[test]
    fn empty_root_apply_waits_for_start_lifecycle_claim_and_rechecks() {
        let tmp = tempfile::tempdir().unwrap();
        let owner_root = tmp.path().join("repository");
        let owner = init_storage_root_test_repository(&owner_root);
        // The scoped storage snapshot now requires a valid owner binding row.
        // Bind another legitimate root so this fixture can still exercise the
        // reviewed, unselected root's post-lock content recheck.
        let owner_storage = crate::test_storage::TestStorage::new();
        let mut broker = owner_storage.open(&owner_root).unwrap();
        broker.prepare_worktree_root_for_submission().unwrap();
        let selected_root = broker
            .store()
            .repository_worktree_binding()
            .unwrap()
            .worktree_root
            .unwrap();
        let host_state = tmp.path().join("host-state");
        let repository = ThisRepository::observe(&owner_root).unwrap();
        let root = host_state.join(&repository.key);
        assert_ne!(normalise(&selected_root), normalise(&root));
        std::fs::create_dir_all(root.join(".cargo")).unwrap();
        std::fs::write(
            root.join(".cargo/config.toml"),
            crate::broker::WORKTREE_CARGO_CONFIG,
        )
        .unwrap();
        assert!(write_attributed_marker(&root, &repository).unwrap());
        let marker_bytes = std::fs::read(root.join(WORKTREE_ROOT_MARKER)).unwrap();
        let candidate = StorageCandidate {
            kind: StorageCandidateKind::EmptyRoot,
            path: root.clone(),
            root_path: root.clone(),
            repository_key: repository.key,
            repository_root: owner_root.clone(),
            git_marker: false,
            estimated_bytes: None,
            estimated_inodes: None,
            reason: "fixture reviewed empty root".into(),
            marker_sha256: format!("{:x}", Sha256::digest(marker_bytes)),
        };
        let mut context = bound_storage_context(&owner, &host_state);
        context.worktree_root_override = Some(root.clone());
        let layout = storage_layout_with_context(&owner_root, &context).unwrap();

        // A start holds the lifecycle claim from final root preparation through
        // worktree-add and session registration. Storage must wait, then see
        // the new child and refuse to remove the root.
        let start_claim = crate::file_lock::acquire_repository_lifecycle_lock(&owner_root).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let candidate_for_apply = candidate.clone();
        let layout_for_apply = layout.clone();
        let apply = std::thread::spawn(move || {
            entered_tx.send(()).unwrap();
            result_tx
                .send(apply_candidate(&candidate_for_apply, &layout_for_apply))
                .unwrap();
        });
        entered_rx.recv().unwrap();
        match result_rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            unexpected => {
                drop(start_claim);
                let _ = apply.join();
                panic!("storage apply did not wait for the lifecycle claim: {unexpected:?}");
            }
        }

        std::fs::create_dir(root.join("new-session")).unwrap();
        drop(start_claim);
        let error = result_rx.recv().unwrap().unwrap_err();
        apply.join().unwrap();

        assert!(error.contains("new-session"), "{error}");
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
        assert!(root.join(".cargo/config.toml").is_file());
        assert!(root.join("new-session").is_dir());
    }

    #[test]
    fn start_claims_lifecycle_before_recreating_a_removed_empty_root() {
        let tmp = tempfile::tempdir().unwrap();
        let owner_root = tmp.path().join("repository");
        let _owner_repo = init_storage_root_test_repository(&owner_root);
        let test_storage = crate::test_storage::TestStorage::new();
        let repository = ThisRepository::observe(&owner_root).unwrap();
        let root = test_storage.host_state_dir().join(&repository.key);
        let mut options = test_storage.options();
        options.worktree_root = Some(root.clone());
        let mut broker = crate::Broker::open_with_options(&owner_root, options).unwrap();
        assert!(
            !root.exists(),
            "opening the broker must not prepare this root"
        );

        std::fs::create_dir_all(root.join(".cargo")).unwrap();
        std::fs::write(
            root.join(".cargo/config.toml"),
            crate::broker::WORKTREE_CARGO_CONFIG,
        )
        .unwrap();
        assert!(write_attributed_marker(&root, &repository).unwrap());
        let lifecycle_claim =
            crate::file_lock::acquire_repository_lifecycle_lock(&owner_root).unwrap();
        assert!(remove_empty_root(&root).unwrap() > 0);
        assert!(!root.exists());

        let lifecycle_claim = std::sync::Arc::new(std::sync::Mutex::new(Some(lifecycle_claim)));
        let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observed_root = root.clone();
        let claim_at_boundary = std::sync::Arc::clone(&lifecycle_claim);
        let observed_at_boundary = std::sync::Arc::clone(&observed);
        broker.worktree_lifecycle_test_observer = Some(std::sync::Arc::new(move |main_root| {
            *observed_at_boundary.lock().unwrap() =
                Some((main_root.to_path_buf(), observed_root.exists()));
            // Release synchronously at the actual claim boundary so the
            // test exercises the normal entrypoint without a worker that
            // could be left blocked if setup regresses.
            drop(claim_at_boundary.lock().unwrap().take());
        }));

        // This callback runs at the actual start path's lifecycle-claim
        // boundary. With preparation ordered after the claim, the root is
        // still absent. Moving preparation back before the claim makes this
        // observation fail deterministically.
        let session = broker.start_worktree("root race", None).unwrap();
        let (observed_main_root, root_was_prepared_before_claim) = observed
            .lock()
            .unwrap()
            .take()
            .expect("start reached the lifecycle claim boundary");
        assert_eq!(
            observed_main_root,
            owner_root.canonicalize().unwrap(),
            "the observer must be called for this repository"
        );
        assert!(
            !root_was_prepared_before_claim,
            "start recreated the root before claiming the lifecycle lock"
        );
        let worktree = PathBuf::from(session.worktree_path);
        assert!(worktree.starts_with(&root));
        assert!(worktree.is_dir());
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
        assert_eq!(
            std::fs::read(root.join(".cargo/config.toml")).unwrap(),
            crate::broker::WORKTREE_CARGO_CONFIG.as_bytes()
        );
    }

    #[test]
    fn missing_from_names_each_absent_source() {
        let entry = EntryBuilder {
            on_disk: true,
            git_registered: false,
            ledger_claimed: false,
            git_marker: true,
            session_ids: Vec::new(),
            estimated_bytes: Some(1),
            estimated_inodes: None,
        }
        .finish(PathBuf::from("/root/worktree"));
        assert_eq!(
            entry.missing_from,
            vec![StorageSource::GitRegistration, StorageSource::SessionLedger]
        );
    }
}
