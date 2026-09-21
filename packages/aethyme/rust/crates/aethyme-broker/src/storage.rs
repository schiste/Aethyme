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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::broker::directory_size_without_following_links;

use crate::broker::{WORKTREE_ROOT_MARKER, WORKTREE_ROOT_SCHEMA_VERSION, WorktreeRootMarker};
use crate::gc::{TreeRemoval, remove_condemned_tree};
use crate::reclaim::is_artefact_directory_with_extras;
use crate::{BrokerStore, GitRepo};

pub const STORAGE_PLAN_SCHEMA_VERSION: u32 = 2;
pub const STORAGE_RECONCILIATION_SCHEMA_VERSION: u32 = 1;

const DAY_MS: i64 = 86_400_000;

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
        "the reviewed host storage plan no longer matches current state; nothing was removed; review a new plan with `aethyme broker storage plan` and confirm its digest"
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
}

impl StorageCandidateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OrphanRoot => "orphan_root",
            Self::StrayDirectory => "stray_directory",
            Self::PrimaryArtifact => "primary_artifact",
            Self::PreparationEntry => "preparation_entry",
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
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageReconciliation {
    pub schema_version: u32,
    pub on_disk_count: usize,
    pub git_registered_count: usize,
    pub ledger_claimed_count: usize,
    /// The union of the three sets. `missing_from` is empty only when all
    /// three sources agree about the path.
    pub entries: Vec<StorageEntry>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    pub sized: bool,
    pub reconciliation: StorageReconciliation,
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
    pub candidate_count: usize,
    pub primary_checkout_count: usize,
    pub primary_artifact_count: usize,
    pub primary_candidate_count: usize,
    #[serde(default)]
    pub preparation_entry_count: usize,
    #[serde(default)]
    pub preparation_candidate_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation_reclaimable_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reclaimable_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_reclaimable_bytes: Option<u64>,
    pub sized: bool,
}

/// One entry in the shared preparation cache.
///
/// The cache sits beside the worktree container rather than inside it, which
/// is why every other lane here has been blind to it: `storage_container`
/// resolves to `<host>/worktrees`, and these live under
/// `<host>/preparation-cache/<repository>/<key>`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoragePreparationEntry {
    pub path: PathBuf,
    pub repository: String,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
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
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageAppliedItem {
    pub kind: StorageCandidateKind,
    pub path: PathBuf,
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
}

#[derive(Debug, Clone)]
struct OwnerSources {
    git_registered: BTreeSet<PathBuf>,
    ledger_claimed: BTreeMap<PathBuf, Vec<i64>>,
}

/// Build the host inventory for the repository containing `path_inside_repo`.
/// This function performs no broker-store creation, migration, lease refresh,
/// or ownership-changing filesystem writes. It may persist one bounded size
/// measurement when the routine policy permits it.
pub fn storage_plan(path_inside_repo: &Path) -> Result<StoragePlan, StorageError> {
    let checkout = GitRepo::discover(path_inside_repo)?;
    let main_root = checkout.main_root()?;
    let storage_root = storage_container(&main_root)?;
    let (policy, warnings) = host_policy(&main_root);
    let mut records = crate::measurement::load_size_records(&main_root);
    let plan = build_plan(
        &main_root,
        &storage_root,
        policy.orphan_worktree_roots_days,
        warnings.clone(),
        crate::SizeScan::Recorded,
        &mut records,
    )?;
    if warnings.is_empty() {
        warm_one_storage_size_record(&main_root, &plan, &policy, &mut records);
    }
    Ok(plan)
}

/// Apply exactly one reviewed host storage plan. The digest is checked against
/// a fresh, read-only inventory before any candidate is touched.
pub fn storage_apply(
    path_inside_repo: &Path,
    confirm: &str,
) -> Result<StorageApplyReport, StorageError> {
    if !is_sha256(confirm) {
        return Err(StorageError::ConfirmationNotSha256);
    }
    let checkout = GitRepo::discover(path_inside_repo)?;
    let main_root = checkout.main_root()?;
    let storage_root = storage_container(&main_root)?;
    let (policy, warnings) = host_policy(&main_root);
    let mut records = crate::measurement::load_size_records(&main_root);
    let plan = build_plan(
        &main_root,
        &storage_root,
        policy.orphan_worktree_roots_days,
        warnings,
        crate::SizeScan::Measure,
        &mut records,
    )?;
    // The apply revalidation has already paid for a complete walk. Keep those
    // observations so the next routine plan can report them without paying
    // for the same trees again.
    let _ = crate::measurement::save_size_records(&main_root, &records);
    if !plan.digest.eq_ignore_ascii_case(confirm) {
        return Err(StorageError::ConfirmationMismatch);
    }

    let mut applied = Vec::new();
    let mut failures = Vec::new();
    for candidate in &plan.candidates {
        match apply_candidate(candidate, &storage_root) {
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
        let live_roots = preparation_live_roots(&main_root, &plan.primary_checkouts, &plan.roots);
        match apply_preparation_candidate(candidate, &live_roots) {
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
        recovery_action: (!complete).then(|| "aethyme broker storage plan".into()),
    })
}

fn build_plan(
    main_root: &Path,
    storage_root: &Path,
    orphan_worktree_roots_days: u32,
    warnings: Vec<String>,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
) -> Result<StoragePlan, StorageError> {
    let storage_root = normalise(&absolute_path(main_root, storage_root));
    let evaluated_at = now_ms();
    let mut paths = Vec::new();
    match std::fs::symlink_metadata(&storage_root) {
        Ok(metadata) if metadata.is_dir() => {
            let entries = std::fs::read_dir(&storage_root).map_err(|source| StorageError::Io {
                path: storage_root.clone(),
                source,
            })?;
            for entry in entries {
                paths.push(
                    entry
                        .map_err(|source| StorageError::Io {
                            path: storage_root.clone(),
                            source,
                        })?
                        .path(),
                );
            }
        }
        Ok(_) => {
            return Err(StorageError::Unavailable {
                reason: format!("{} is not a directory", storage_root.display()),
            });
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(StorageError::Io {
                path: storage_root,
                source,
            });
        }
    }
    paths.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));

    let mut roots = Vec::with_capacity(paths.len());
    let mut candidates = Vec::new();
    for path in paths {
        let root = inspect_root(
            &path,
            evaluated_at,
            orphan_worktree_roots_days,
            scan,
            records,
        );
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

    // The cache is a sibling of the worktree container, so it is reached from
    // the container rather than rediscovered. Live roots are every enrolled
    // checkout in the inventory plus the one being invoked from.
    let live_roots = preparation_live_roots(main_root, &primary_checkouts, &roots);
    let (preparation_entries, preparation_candidates) = match preparation_cache_root(&storage_root)
    {
        Some(cache_root) => inspect_preparation_cache(&cache_root, &live_roots, scan, records),
        None => (Vec::new(), Vec::new()),
    };
    let digest = decision_digest(
        &storage_root,
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
    })
}

fn inspect_root(
    raw_path: &Path,
    evaluated_at: i64,
    orphan_worktree_roots_days: u32,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
) -> StorageRoot {
    let filesystem_kind = filesystem_kind(raw_path);
    let path = if filesystem_kind == StorageFilesystemKind::Directory {
        normalise(raw_path)
    } else {
        absolute_path(Path::new("/"), raw_path)
    };
    if filesystem_kind != StorageFilesystemKind::Directory {
        let estimated_bytes = observed_size(raw_path, scan, records);
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
            sized: estimated_bytes.is_some(),
            estimated_bytes,
            reconciliation: empty_reconciliation(),
            blockers: vec!["entry is not a real directory and is never swept".into()],
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
                    continue;
                }
                if !is_real_directory(&child) {
                    blockers.push(format!(
                        "{} is not a real directory and is never swept",
                        child.display()
                    ));
                    continue;
                }
                let child = normalise(&child);
                let estimated_bytes = observed_size(&child, scan, records);
                disk.insert(
                    child.clone(),
                    DiskObservation {
                        git_marker: has_git_marker(&child),
                        estimated_bytes,
                    },
                );
            }
        }
        Err(error) => blockers.push(format!("cannot enumerate root: {error}")),
    }

    let mut owner_sources = None;
    if let (Some(owner), Some(true)) = (repository_root.as_deref(), owner_exists) {
        match inspect_owner(owner, &path) {
            Ok(sources) => owner_sources = Some(sources),
            Err(error) => blockers.push(error),
        }
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
        ledger_claimed_count: owner_sources
            .as_ref()
            .map_or(0, |sources| sources.ledger_claimed.len()),
        entries,
    };
    let estimated_bytes = observed_size(&path, scan, records);
    let sized = estimated_bytes.is_some()
        && reconciliation
            .entries
            .iter()
            .filter(|entry| entry.on_disk)
            .all(|entry| entry.estimated_bytes.is_some());
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
        && !age_days.is_some_and(|age| age >= orphan_worktree_roots_days)
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
        estimated_bytes,
        sized,
        reconciliation,
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
/// Bounded by `deadline` and safe to interrupt -- entries are independent, so
/// a partial sweep is simply a smaller sweep. Returns entries removed and
/// bytes reclaimed.
pub(crate) fn sweep_preparation_cache(
    main_root: &Path,
    orphan_worktree_roots_days: u32,
    deadline: std::time::Instant,
) -> (usize, u64) {
    let Ok(storage_root) = storage_container(main_root) else {
        return (0, 0);
    };
    let mut records = crate::measurement::SizeRecords::default();
    // Recorded sizing: the budget should be spent reclaiming, and an entry's
    // size does not change whether it is dead.
    let Ok(plan) = build_plan(
        main_root,
        &storage_root,
        orphan_worktree_roots_days,
        Vec::new(),
        crate::SizeScan::Recorded,
        &mut records,
    ) else {
        return (0, 0);
    };
    // The same population the manual lane uses. It must span every enrolled
    // checkout, not just this repository: the cache is shared, and a live
    // entry belonging to a sibling repository would otherwise look dead here.
    let live_roots = preparation_live_roots(main_root, &plan.primary_checkouts, &plan.roots);
    let mut removed = 0_usize;
    let mut bytes = 0_u64;
    for candidate in &plan.preparation_candidates {
        if std::time::Instant::now() >= deadline {
            break;
        }
        if let Ok(reclaimed) = apply_preparation_candidate(candidate, &live_roots) {
            removed += 1;
            bytes = bytes.saturating_add(reclaimed);
        }
    }
    (removed, bytes)
}

fn preparation_live_roots(
    main_root: &Path,
    primary_checkouts: &[StoragePrimaryCheckout],
    roots: &[StorageRoot],
) -> Vec<PathBuf> {
    let mut live_roots = primary_checkouts
        .iter()
        .map(|checkout| normalise(&checkout.path))
        .collect::<Vec<_>>();
    live_roots.push(normalise(main_root));
    // Use the same population for inventory and immediate deletion checks.
    // Session worktrees, not just primary checkouts, consume shared entries.
    for root in roots {
        if let Ok(children) = std::fs::read_dir(&root.path) {
            for child in children.flatten() {
                if is_real_directory(&child.path()) {
                    live_roots.push(normalise(&child.path()));
                }
            }
        }
    }
    live_roots.sort();
    live_roots.dedup();
    live_roots
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
        let Ok(keys) = std::fs::read_dir(&repository_path) else {
            continue;
        };
        for key_entry in keys.flatten() {
            let path = key_entry.path();
            if !is_real_directory(&path) {
                continue;
            }
            let key = key_entry.file_name().to_string_lossy().into_owned();
            let live = liveness_unknown || live_keys.contains(&key);
            // Measuring is the expensive half, so a live entry is counted but
            // never walked: its size cannot change the decision.
            let estimated_bytes = if !live {
                observed_size(&path, scan, records)
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
            reason: entry.reason.clone(),
        })
        .collect::<Vec<_>>();
    (entries, candidates)
}

/// Remove one dead cache entry, re-deciding at the moment of removal.
///
/// The plan may be minutes old and a checkout may have changed its lockfile
/// since, so the key is re-tested against the live set rather than trusted
/// from the digest that authorised the run.
fn apply_preparation_candidate(
    candidate: &StoragePreparationCandidate,
    live_roots: &[PathBuf],
) -> Result<u64, String> {
    for root in live_roots {
        let key = crate::preparation::current_cache_key(root)
            .map_err(|error| format!("cannot verify preparation liveness: {error}"))?;
        if key.as_deref() == Some(candidate.key.as_str()) {
            return Err("a checkout now computes this key again; it is no longer dead".into());
        }
    }
    if !is_real_directory(&candidate.path) {
        return Err("cache entry is no longer a directory".into());
    }
    let bytes = directory_size_without_following_links(&candidate.path).unwrap_or(0);
    std::fs::remove_dir_all(&candidate.path)
        .map_err(|error| format!("cannot remove cache entry: {error}"))?;
    Ok(bytes)
}

/// Where the shared preparation cache lives, beside the worktree container.
fn preparation_cache_root(storage_root: &Path) -> Option<PathBuf> {
    storage_root
        .parent()
        .map(|base| base.join("preparation-cache"))
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
                    artifacts.push(StoragePrimaryArtifact {
                        path,
                        name,
                        ignored,
                        tracked,
                        reclaimable,
                        estimated_bytes,
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
                reason: "owning repository no longer exists".into(),
                marker_sha256,
            }];
        }
        return Vec::new();
    }
    if root.owner_exists != Some(true) || !root.blockers.is_empty() {
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
        let bytes = crate::broker::directory_size_without_following_links(path).ok()?;
        records.record(&key, bytes, now_ms());
        Some(bytes)
    } else {
        records.get(&key).map(|record| record.bytes)
    }
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
    let Some(bytes) = crate::broker::directory_size_bounded(Path::new(&path), deadline) else {
        return;
    };
    records.record(&path, bytes, now_ms());
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
    let reclaimable_bytes = sized_sum(candidates.iter().map(|candidate| candidate.estimated_bytes));
    let primary_reclaimable_bytes = sized_sum(
        primary_candidates
            .iter()
            .map(|candidate| candidate.estimated_bytes),
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
        estimated_bytes,
        reclaimable_bytes,
        primary_reclaimable_bytes,
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
    let mut git_registered = BTreeSet::new();
    for path in checkout
        .worktree_paths()
        .map_err(|error| format!("Git worktree registrations cannot be inspected: {error}"))?
    {
        let path = normalise_absolute(&path, owner);
        if is_direct_child(&path, host_root) {
            git_registered.insert(path);
        }
    }

    let store_path = owner.join(crate::BROKER_DB_RELPATH);
    if !is_regular_file(&store_path) {
        return Err(format!(
            "session ledger is missing or is not a regular file at {}; no stray directory is swept",
            store_path.display()
        ));
    }
    let store = BrokerStore::open_snapshot_at(&store_path).map_err(|source| {
        format!(
            "session ledger cannot be inspected at {}: {source}",
            store_path.display()
        )
    })?;
    let mut ledger_claimed = BTreeMap::<PathBuf, Vec<i64>>::new();
    let mut sessions = store
        .live_sessions()
        .map_err(|error| format!("live session ledger cannot be read: {error}"))?;
    sessions.extend(
        store
            .cleaned_sessions()
            .map_err(|error| format!("closed session ledger cannot be read: {error}"))?,
    );
    for session in sessions {
        let path = normalise_absolute(Path::new(&session.worktree_path), owner);
        if is_direct_child(&path, host_root) {
            ledger_claimed.entry(path).or_default().push(session.id);
        }
    }
    for session_ids in ledger_claimed.values_mut() {
        session_ids.sort_unstable();
    }
    Ok(OwnerSources {
        git_registered,
        ledger_claimed,
    })
}

fn apply_candidate(candidate: &StorageCandidate, storage_root: &Path) -> Result<u64, String> {
    let current_marker_root = match candidate.kind {
        StorageCandidateKind::OrphanRoot => candidate.path.as_path(),
        StorageCandidateKind::StrayDirectory => candidate.root_path.as_path(),
        StorageCandidateKind::PrimaryArtifact => {
            return Err("primary artifacts use the primary-checkout apply lane".into());
        }
        StorageCandidateKind::PreparationEntry => {
            return Err("preparation cache entries use the preparation apply lane".into());
        }
    };
    if !is_direct_child(&normalise(&candidate.root_path), storage_root) {
        return Err("reviewed root is outside the host storage container".into());
    }
    if candidate.kind == StorageCandidateKind::StrayDirectory
        && !is_direct_child(
            &normalise(&candidate.path),
            &normalise(&candidate.root_path),
        )
    {
        return Err("stray directory is not a direct child of its reviewed root".into());
    }
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
    let before = crate::removal::tree_size(path);
    match remove_condemned_tree(path, witness, None) {
        Ok(TreeRemoval::Complete) => Ok(before),
        Ok(TreeRemoval::Interrupted) => Err("directory removal was interrupted".into()),
        Err(error) => Err(error.to_string()),
    }
}

fn storage_container(main_root: &Path) -> Result<PathBuf, StorageError> {
    let container = if let Some(value) =
        std::env::var_os("AETHYME_WORKTREE_ROOT").filter(|value| !value.is_empty())
    {
        absolute_path(main_root, &PathBuf::from(value))
    } else {
        let Some(base) = crate::host_state::default_host_state_dir() else {
            return Err(StorageError::Unavailable {
                reason: "no host-state directory is available; set AETHYME_WORKTREE_ROOT".into(),
            });
        };
        if !crate::host_state::host_state_dir_is_explicit()
            && crate::host_state::path_is_ephemeral(main_root)
        {
            return Err(StorageError::Unavailable {
                reason: "the repository is under the system temporary directory and the implicit host-state directory is withheld; set AETHYME_WORKTREE_ROOT or AETHYME_HOST_STATE_DIR explicitly".into(),
            });
        }
        absolute_path(main_root, &base).join("worktrees")
    };
    if path_is_inside(main_root, &container) {
        return Err(StorageError::Unavailable {
            reason: format!(
                "{} resolves inside repository {}; choose an external host storage root",
                container.display(),
                main_root.display()
            ),
        });
    }
    Ok(container)
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

fn decision_digest(
    storage_root: &Path,
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
    let bytes = serde_json::to_vec(&Decision {
        storage_root,
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

    /// The live set is the keys checkouts would recompute, so an entry is dead
    /// exactly when no checkout names it -- not when it looks old.
    #[test]
    fn a_cache_entry_no_checkout_names_is_the_reclaimable_one() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("preparation-cache");
        let repo_dir = cache.join("repository-a");
        std::fs::create_dir_all(repo_dir.join("aaaaaaaaaaaa")).unwrap();
        std::fs::create_dir_all(repo_dir.join("bbbbbbbbbbbb")).unwrap();
        std::fs::write(repo_dir.join("bbbbbbbbbbbb/blob"), "cached\n").unwrap();

        // No live root declares preparation, so nothing keeps a key alive.
        let mut records = crate::measurement::SizeRecords::default();
        let (entries, candidates) =
            inspect_preparation_cache(&cache, &[], crate::SizeScan::Recorded, &mut records);
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

        let cache = tmp.path().join("preparation-cache/repository-a");
        std::fs::create_dir_all(cache.join(&key)).unwrap();
        std::fs::create_dir_all(cache.join("deadbeefdead")).unwrap();

        let mut records = crate::measurement::SizeRecords::default();
        let (entries, candidates) = inspect_preparation_cache(
            &tmp.path().join("preparation-cache"),
            &[repo.clone()],
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
            repository: "repository-a".into(),
            key,
            estimated_bytes: None,
            reason: "previously dead".into(),
        };
        let roots = preparation_live_roots(&repo, &[], &[]);
        assert!(apply_preparation_candidate(&live_candidate, &roots).is_err());
        assert!(live_candidate.path.is_dir());
    }

    /// Reporting spans the cache; removal re-decides at the moment it acts.
    #[test]
    fn removal_refuses_an_entry_that_became_live_again() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = tmp
            .path()
            .join("preparation-cache/repository-a/ccccccccccc1");
        std::fs::create_dir_all(&entry).unwrap();
        let candidate = StoragePreparationCandidate {
            path: entry.clone(),
            repository: "repository-a".into(),
            key: "ccccccccccc1".into(),
            estimated_bytes: None,
            reason: "dead".into(),
        };

        // A root that cannot produce a key leaves the entry dead, so it goes.
        let removed = apply_preparation_candidate(&candidate, &[tmp.path().to_path_buf()]);
        assert!(removed.is_ok(), "a dead entry is removable: {removed:?}");
        assert!(!entry.exists(), "the entry must actually be gone");

        // Removing what is already gone is a refusal, not a silent success.
        assert!(apply_preparation_candidate(&candidate, &[]).is_err());
    }

    /// The cache is a sibling of the worktree container, which is the whole
    /// reason it was invisible to this lane.
    #[test]
    fn unknown_preparation_liveness_never_authorizes_deletion() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("checkout");
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(repo.join(".aethyme/prepare.toml"), "invalid = [").unwrap();
        let cache = tmp.path().join("preparation-cache");
        let path = cache.join("repository/key");
        std::fs::create_dir_all(&path).unwrap();
        let candidate = StoragePreparationCandidate {
            path: path.clone(),
            repository: "repository".into(),
            key: "key".into(),
            estimated_bytes: None,
            reason: "previously unused".into(),
        };
        let mut records = crate::measurement::SizeRecords::default();
        let (entries, candidates) = inspect_preparation_cache(
            &cache,
            &[repo.clone()],
            crate::SizeScan::Recorded,
            &mut records,
        );
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].reclaimable);
        assert!(entries[0].reason.contains("could not be verified"));
        assert!(candidates.is_empty());
        assert!(apply_preparation_candidate(&candidate, &[repo]).is_err());
        assert!(path.is_dir());
    }

    #[test]
    fn the_cache_is_found_beside_the_worktree_container() {
        let root = Path::new("/host/state/worktrees");
        assert_eq!(
            preparation_cache_root(root),
            Some(PathBuf::from("/host/state/preparation-cache"))
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
            reason: "stray".into(),
            marker_sha256: "marker".into(),
        };
        let mut changed = candidate.clone();
        changed.estimated_bytes = Some(100);
        assert_eq!(
            decision_digest(Path::new("/storage"), 1, &[candidate], &[], &[]),
            decision_digest(Path::new("/storage"), 1, &[changed], &[], &[])
        );
    }

    #[test]
    fn decision_digest_binds_preparation_identity_not_measurements() {
        let candidate = StoragePreparationCandidate {
            path: PathBuf::from("/cache/repository/key"),
            repository: "repository".into(),
            key: "key".into(),
            estimated_bytes: Some(1),
            reason: "unused".into(),
        };
        let digest = |candidates: &[StoragePreparationCandidate]| {
            decision_digest(Path::new("/storage"), 1, &[], &[], candidates)
        };
        let approved = digest(&[candidate.clone()]);
        assert_ne!(approved, digest(&[]));
        let mut measured = candidate.clone();
        measured.estimated_bytes = Some(999);
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

    #[test]
    fn missing_from_names_each_absent_source() {
        let entry = EntryBuilder {
            on_disk: true,
            git_registered: false,
            ledger_claimed: false,
            git_marker: true,
            session_ids: Vec::new(),
            estimated_bytes: Some(1),
        }
        .finish(PathBuf::from("/root/worktree"));
        assert_eq!(
            entry.missing_from,
            vec![StorageSource::GitRegistration, StorageSource::SessionLedger]
        );
    }
}
