//! Resolving closed worktrees that cleanup will not remove on its own.
//!
//! A closed session whose worktree is `dirty` (uncommitted or untracked
//! changes), holds `pending_commits`, or whose work cannot be proved to have
//! landed stays on disk forever: cleanup refuses it, correctly, because
//! removing it could lose work. The only other exit was `cleanup <id> --force`,
//! which discards. What operators did instead was build recovery kits by hand
//! -- one repository accumulated 2 GiB of them, and 11 dirty plus 8 pending
//! worktrees in another sat untouched because nobody wanted to be the one to
//! discard them.
//!
//! This is the reviewed middle path. `finish cleanup resolve <id> --archive`
//! prints what would be preserved and a digest; confirming that digest writes
//! a recovery archive (a bundle of the commits no delivery target holds, the
//! staged and unstaged patches, a copy of the untracked files Git does not
//! ignore, protected standalone ignored files and links, a manifest and restore
//! steps), reads every part of it back, and only
//! then asks cleanup to remove the worktree under its lifecycle claim. A failed
//! verification leaves the worktree exactly where it was.
//!
//! Ignored directories remain opaque and follow the ordinary cleanup policy.
//! Protected standalone ignored files and links are archived; strict-policy
//! leftovers leave the worktree in place.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::error::BrokerError;
use crate::git::GitRepo;
use crate::types::SessionOrigin;
use crate::{Broker, BrokerOpError, CleanupDisposition};

pub const CLEANUP_RESOLVE_SCHEMA_VERSION: u32 = 3;
const LEGACY_CLEANUP_RESOLVE_SCHEMA_VERSION: u32 = 2;
const LEGACY_RECOVERY_ARCHIVE_SCHEMA_VERSION: u32 = 1;
const PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION: u32 = 2;
const PREVIOUS_BUNDLE_RECOVERY_ARCHIVE_SCHEMA_VERSION: u32 = 3;
const RECOVERY_ARCHIVE_SCHEMA_VERSION: u32 = 4;
const RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION: u32 = 5;
const PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION: u32 = 6;
const RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION: u32 = 2;
const PINNED_RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION: u32 = 3;
const RETIREMENT_ARCHIVE_RECORD_FILE: &str = "retirement.json";
const MAX_RETIREMENT_ARCHIVE_RECORD_BYTES: u64 = 64 * 1024;
const MIN_RECOVERY_ARCHIVE_RETENTION_MS: u64 = 15 * 24 * 60 * 60 * 1_000;
const AUTO_COMPACTION_STAGING_RESERVE: Duration = Duration::from_secs(10);
const MAX_RECOVERY_BUNDLE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_RECOVERY_BUNDLE_STDERR_BYTES: usize = 64 * 1024;
const MAX_RECOVERY_BUNDLE_METADATA_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_RECOVERY_ARCHIVE_PREREQUISITE_REFS: usize = 128;

/// Directory beneath the host state directory holding one subdirectory of
/// archives per repository.
const RECOVERY_ARCHIVE_DIRECTORY: &str = "recovery-archives";
const BUNDLE_FILE: &str = "commits.bundle";
const STAGED_PATCH_FILE: &str = "staged.patch";
const UNSTAGED_PATCH_FILE: &str = "unstaged.patch";
const UNTRACKED_DIRECTORY: &str = "untracked";
const IGNORED_DIRECTORY: &str = "ignored";
pub(crate) const MANIFEST_FILE: &str = "manifest.json";
const README_FILE: &str = "README.md";
const MAX_RECOVERY_README_BYTES: u64 = 64 * 1024;
const MAX_WORKTREE_SPECIAL_SCAN_ENTRIES: usize = 250_000;
const MAX_WORKTREE_SPECIAL_SCAN_DEPTH: usize = 128;
static ARCHIVE_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// One path from Git's collapsed ignored-root listing and the classification
/// assigned by the current cleanup policy.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IgnoredPath {
    pub path: String,
    pub classification: IgnoredPathClassification,
}

/// The disposition of an ignored root in the reviewed cleanup plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IgnoredPathClassification {
    DisposableDirectory,
    DisposableOutput,
    PreservedFile,
}

/// One untracked or protected ignored path the archive preserves.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivedFile {
    /// Relative to the worktree root.
    pub path: String,
    pub bytes: u64,
    /// SHA-256 of the file's content, or of `symlink\0<target>` for a link.
    pub sha256: String,
    /// A symbolic link is archived as the link, never as what it points at.
    pub symlink: bool,
    /// Unix permission bits for a regular file. `None` for symlinks and on
    /// platforms that cannot represent Unix file modes.
    #[serde(default)]
    pub mode: Option<u32>,
}

/// What a confirmed resolve would preserve, and the digest that authorizes it.
pub(crate) struct CleanupResolveRetirementContext {
    delivery_targets: Vec<String>,
    live_checkouts: Vec<(i64, crate::auto_cleanup::ResolvedWorktreePath)>,
    ancestry_memo: PlanAncestryMemo,
    deadline: Option<Instant>,
}

/// A memo scoped to one captured retirement-planning context. Its keys are
/// exact commit IDs, so results cannot leak across plans or changing refs.
/// Apply-time validation deliberately uses the uncached Git helper below.
#[derive(Default)]
pub(crate) struct PlanAncestryMemo {
    delivery_targets: Vec<String>,
    deadline: Option<Instant>,
    results: RefCell<HashMap<(String, String), bool>>,
}

impl PlanAncestryMemo {
    fn for_delivery_targets(delivery_targets: &[String], deadline: Option<Instant>) -> Self {
        Self {
            delivery_targets: delivery_targets.to_vec(),
            deadline,
            results: RefCell::default(),
        }
    }

    pub(crate) fn check(
        &self,
        repository: &GitRepo,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, String> {
        self.check_until(repository, ancestor, descendant, None)
    }

    pub(crate) fn check_until(
        &self,
        repository: &GitRepo,
        ancestor: &str,
        descendant: &str,
        deadline: Option<Instant>,
    ) -> Result<bool, String> {
        let deadline = match (self.deadline, deadline) {
            (Some(context), Some(call)) => Some(context.min(call)),
            (Some(context), None) => Some(context),
            (None, Some(call)) => Some(call),
            (None, None) => None,
        };
        if deadline.is_some_and(|deadline| Instant::now() >= deadline)
            || !self
                .delivery_targets
                .iter()
                .any(|target| target == descendant)
        {
            return repository
                .is_ancestor_checked(ancestor, descendant)
                .map_err(|error| error.to_string());
        }
        self.check_with(ancestor, descendant, || {
            repository
                .is_ancestor_checked(ancestor, descendant)
                .map_err(|error| error.to_string())
        })
    }

    fn check_with(
        &self,
        ancestor: &str,
        descendant: &str,
        check: impl FnOnce() -> Result<bool, String>,
    ) -> Result<bool, String> {
        let key = (ancestor.to_owned(), descendant.to_owned());
        if let Some(result) = self.results.borrow().get(&key) {
            return Ok(*result);
        }

        match check() {
            Ok(result) => {
                self.results.borrow_mut().insert(key, result);
                Ok(result)
            }
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupResolvePlan {
    pub schema_version: u32,
    /// Binds repository, branch, checkout and preserved content. Ordinary
    /// schema-4 confirmations leave delivery snapshots outside this stable
    /// content digest; retirement confirmations additionally bind the exact
    /// delivery-target vector and root-availability proof. Replan on upgrade.
    pub digest: String,
    pub session_id: i64,
    /// Current recorded origin of this exact cleanup target. Batch retirement
    /// confirmations bind it alongside the path and per-item content digest.
    pub target_origin: SessionOrigin,
    /// Archive format selected by the reviewed operation. Retirement uses a
    /// new schema so older binaries refuse it rather than ignore its retention
    /// promise.
    pub recovery_archive_schema_version: u32,
    /// Immutable minimum age before GC may propose this archive, in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_archive_retention_ms: Option<u64>,
    /// The exact explicit mode captured by a retirement plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retirement_allow_adopted: Option<bool>,
    /// Present for automatic retirement and binds the persisted checkout-owner
    /// witness into the durable archive receipt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retirement_witness_sha256: Option<String>,
    pub repository_path: String,
    pub worktree_path: String,
    pub branch_ref: String,
    /// The registered session branch tip captured with the worktree HEAD.
    pub branch_tip: Option<String>,
    /// The symbolic branch currently checked out in the worktree. `None`
    /// means HEAD is detached; when present, its captured tip equals `head`.
    pub checkout_branch_ref: Option<String>,
    pub checkout_branch_tip: Option<String>,
    /// Root refs and the delivery snapshot's proof of whether each needs a
    /// recovery bundle. Landed roots remain recorded without bloating bundles.
    pub recovery_root_proofs: Vec<RecoveryRootProof>,
    pub disposition: CleanupDisposition,
    /// Why cleanup alone refuses this worktree.
    pub reason: String,
    pub head: String,
    /// The commits cleanup judges against; the bundle excludes all of them.
    pub delivery_targets: Vec<String>,
    /// A landing verdict, or an explicit note that recovery planning skipped
    /// the expensive exact-content search.
    pub landing: RecoveryLandingAssessment,
    /// Commits no delivery target holds, newest first. Empty means no bundle.
    pub unlanded_commits: Vec<String>,
    pub staged_patch_bytes: u64,
    pub staged_patch_sha256: String,
    pub unstaged_patch_bytes: u64,
    pub unstaged_patch_sha256: String,
    pub untracked_files: Vec<ArchivedFile>,
    pub untracked_bytes: u64,
    /// Every Git ignored root and the policy classification used to decide
    /// whether it is archived or discarded as generated output.
    pub ignored_paths: Vec<IgnoredPath>,
    /// Protected standalone ignored files and links, stored under `ignored/`.
    pub ignored_files: Vec<ArchivedFile>,
    pub ignored_bytes: u64,
    /// Digest of the strict/regenerable policy inputs bound into `digest`.
    pub ignored_policy_sha256: String,
    /// The directory the archive will be written beneath.
    pub archive_root: PathBuf,
    pub apply_command: String,
    /// The existing discard path, named so the choice is explicit.
    pub discard_command: String,
}

/// Recovery's landing-proof assessment. The known verdict keeps its legacy
/// JSON shape; recovery planning records its intentional omission separately.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum RecoveryLandingAssessment {
    Verdict(crate::LandingVerdict),
    NotEvaluatedForRecovery { state: RecoveryLandingState },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryLandingState {
    NotEvaluatedForRecovery,
}

/// What a confirmed resolve did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupResolveOutcome {
    pub plan: CleanupResolvePlan,
    /// The verified archive. It is kept even when the removal below fails.
    pub archive: PathBuf,
    pub worktree_removed: bool,
    /// Why removal failed after the archive was verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_error: Option<String>,
}

/// Schema v2's exact digest input is retained to validate archives written by
/// v0.8.37. Do not add fields or reorder this serialization.
#[derive(serde::Serialize)]
struct DigestInputV2<'a> {
    schema_version: u32,
    session_id: i64,
    worktree_path: &'a str,
    disposition: &'a str,
    head: &'a str,
    delivery_targets: &'a [String],
    unlanded_commits: &'a [String],
    staged_patch_sha256: &'a str,
    unstaged_patch_sha256: &'a str,
    untracked_files: &'a [ArchivedFile],
    ignored_paths: &'a [IgnoredPath],
    ignored_files: &'a [ArchivedFile],
    ignored_policy: &'a IgnoredPolicyProof<'a>,
}

/// Frozen schema-3 digest serialization. Do not reorder or add fields.
#[derive(serde::Serialize)]
struct DigestInputV3<'a> {
    schema_version: u32,
    recovery_archive_schema_version: u32,
    max_recovery_bundle_bytes: u64,
    session_id: i64,
    worktree_path: &'a str,
    disposition: &'a str,
    head: &'a str,
    delivery_targets: &'a [String],
    unlanded_commits: &'a [String],
    staged_patch_sha256: &'a str,
    unstaged_patch_sha256: &'a str,
    untracked_files: &'a [ArchivedFile],
    ignored_paths: &'a [IgnoredPath],
    ignored_files: &'a [ArchivedFile],
    ignored_policy: &'a IgnoredPolicyProof<'a>,
}

#[derive(serde::Serialize)]
struct DigestInputV4<'a> {
    schema_version: u32,
    recovery_archive_schema_version: u32,
    max_recovery_bundle_bytes: u64,
    session_id: i64,
    worktree_path: &'a str,
    disposition: &'a str,
    head: &'a str,
    delivery_targets: &'a [String],
    unlanded_commits: &'a [String],
    staged_patch_sha256: &'a str,
    unstaged_patch_sha256: &'a str,
    untracked_files: &'a [ArchivedFile],
    ignored_paths: &'a [IgnoredPath],
    ignored_files: &'a [ArchivedFile],
    ignored_policy: &'a IgnoredPolicyProof<'a>,
}

/// V5 binds canonical repository identity, preserved content and checkout
/// identity, while delivery targets, disposition and commit-ancestry results
/// remain the original evidence snapshot in the manifest. Never add those
/// evidence fields here.
#[derive(serde::Serialize)]
struct DigestInputV5<'a> {
    schema_version: u32,
    recovery_archive_schema_version: u32,
    max_recovery_bundle_bytes: u64,
    session_id: i64,
    repository_path: &'a str,
    worktree_path: &'a str,
    branch_ref: &'a str,
    branch_tip: Option<&'a str>,
    head: &'a str,
    staged_patch_sha256: &'a str,
    unstaged_patch_sha256: &'a str,
    untracked_files: &'a [ArchivedFile],
    ignored_paths: &'a [IgnoredPath],
    ignored_files: &'a [ArchivedFile],
    ignored_policy: &'a IgnoredPolicyProof<'a>,
}

/// A commit root that must survive checkout removal, and whether the reviewed
/// delivery snapshot already makes that exact object recoverable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRootProof {
    pub ref_name: String,
    pub object_id: String,
    pub available_from_delivery: bool,
}

/// Stable source identity for restarting owned partial staging when only
/// the delivery snapshot has advanced. It intentionally excludes delivery
/// targets and root availability, but binds the source files and root OIDs.
#[derive(serde::Serialize)]
struct RetirementContentIdentityInputV1<'a> {
    schema_version: u32,
    recovery_archive_schema_version: u32,
    session_id: i64,
    repository_path: &'a str,
    worktree_path: &'a str,
    target_origin: &'a str,
    allow_adopted: bool,
    minimum_archive_retention_ms: u64,
    owner_witness_sha256: Option<&'a str>,
    branch_ref: &'a str,
    branch_tip: Option<&'a str>,
    checkout_branch_ref: Option<&'a str>,
    checkout_branch_tip: Option<&'a str>,
    head: &'a str,
    staged_patch_bytes: u64,
    staged_patch_sha256: &'a str,
    unstaged_patch_bytes: u64,
    unstaged_patch_sha256: &'a str,
    untracked_files: &'a [ArchivedFile],
    untracked_bytes: u64,
    ignored_paths: &'a [IgnoredPath],
    ignored_files: &'a [ArchivedFile],
    ignored_bytes: u64,
    ignored_policy_sha256: &'a str,
    recovery_root_identities: Vec<(&'a str, &'a str)>,
}

#[derive(serde::Serialize)]
struct RetirementDigestInputV1<'a> {
    content_digest: &'a str,
    recovery_archive_schema_version: u32,
    minimum_archive_retention_ms: u64,
    target_origin: &'a str,
    allow_adopted: bool,
    owner_witness_sha256: Option<&'a str>,
    delivery_targets_sha256: &'a str,
    recovery_root_proofs_sha256: &'a str,
}

/// Schema v6 confirmations bind immutable checkout contents and root names,
/// while the captured delivery envelope remains archive evidence that can be
/// refreshed after a forward-only delivery move.
#[derive(serde::Serialize)]
struct RetirementDigestInputV2<'a> {
    content_digest: &'a str,
    recovery_archive_schema_version: u32,
    minimum_archive_retention_ms: u64,
    target_origin: &'a str,
    allow_adopted: bool,
    owner_witness_sha256: Option<&'a str>,
    checkout_branch_ref: Option<&'a str>,
    checkout_branch_tip: Option<&'a str>,
    recovery_root_identities: Vec<(&'a str, &'a str)>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryArchivePrerequisiteRef {
    pub ref_name: String,
    pub object_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RetirementArchiveRecord {
    schema_version: u32,
    session_id: i64,
    plan_digest: String,
    target_origin: String,
    allow_adopted: bool,
    minimum_archive_retention_ms: u64,
    archive_created_at_ms: i64,
    content_identity_sha256: String,
    owner_witness_sha256: Option<String>,
    delivery_targets_sha256: String,
    worktree_path: String,
    branch_ref: String,
    branch_tip: Option<String>,
    checkout_branch_ref: Option<String>,
    checkout_branch_tip: Option<String>,
    recovery_root_proofs_sha256: String,
    head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivery_targets: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prerequisite_refs: Option<Vec<RecoveryArchivePrerequisiteRef>>,
}

#[derive(serde::Serialize)]
struct IgnoredPolicyProof<'a> {
    strict: bool,
    regenerable: &'a [String],
}

#[derive(serde::Serialize)]
struct PatchRecord<'a> {
    file: &'static str,
    bytes: u64,
    sha256: &'a str,
}

/// `manifest.json`: everything needed to tell what the archive holds and
/// whether it is intact, without the broker database that produced it.
#[derive(serde::Serialize)]
struct RecoveryManifest<'a> {
    schema_version: u32,
    plan_digest: &'a str,
    created_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimum_archive_retention_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retirement_record_sha256: Option<&'a str>,
    aethyme_version: &'static str,
    repository: String,
    session_id: i64,
    task: Option<&'a str>,
    worktree_path: &'a str,
    branch_ref: &'a str,
    branch_tip: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checkout_branch_ref: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checkout_branch_tip: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_root_proofs_sha256: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_root_proofs: Option<&'a [RecoveryRootProof]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_targets_sha256: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prerequisite_refs: Option<&'a [RecoveryArchivePrerequisiteRef]>,
    disposition: &'a str,
    head: &'a str,
    /// Where `head` diverged from the primary checkout, when Git can say.
    base: Option<String>,
    delivery_targets: &'a [String],
    landing: &'a RecoveryLandingAssessment,
    bundle: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bundle_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bundle_sha256: Option<&'a str>,
    unlanded_commits: &'a [String],
    staged_patch: PatchRecord<'a>,
    unstaged_patch: PatchRecord<'a>,
    untracked_directory: &'static str,
    untracked_files: &'a [ArchivedFile],
    ignored_directory: &'static str,
    ignored_paths: &'a [IgnoredPath],
    ignored_files: &'a [ArchivedFile],
    ignored_policy_sha256: &'a str,
}

struct RecoveryBundleIntegrity {
    bytes: u64,
    sha256: String,
}

/// Directory disposal is authorized by the bound policy and freshly checked
/// classification. Keep every standalone ignored path in the proof, even when
/// policy calls it disposable output; new standalone files must still refuse.
fn authorization_ignored_paths(paths: &[IgnoredPath]) -> Vec<IgnoredPath> {
    paths
        .iter()
        .filter(|path| path.classification != IgnoredPathClassification::DisposableDirectory)
        .cloned()
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn legacy_v2_cleanup_resolve_digest(
    plan: &CleanupResolvePlan,
    ignored_policy: &IgnoredPolicyProof<'_>,
) -> Result<String, String> {
    if plan.schema_version != CLEANUP_RESOLVE_SCHEMA_VERSION {
        return Err("cannot compare a legacy archive against an unsupported resolve plan".into());
    }
    let digest_input = DigestInputV2 {
        schema_version: LEGACY_CLEANUP_RESOLVE_SCHEMA_VERSION,
        session_id: plan.session_id,
        worktree_path: &plan.worktree_path,
        disposition: plan.disposition.as_str(),
        head: &plan.head,
        delivery_targets: &plan.delivery_targets,
        unlanded_commits: &plan.unlanded_commits,
        staged_patch_sha256: &plan.staged_patch_sha256,
        unstaged_patch_sha256: &plan.unstaged_patch_sha256,
        untracked_files: &plan.untracked_files,
        ignored_paths: &plan.ignored_paths,
        ignored_files: &plan.ignored_files,
        ignored_policy,
    };
    let serialized = serde_json::to_vec(&digest_input)
        .map_err(|error| format!("cannot encode legacy plan digest: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(b"aethyme-cleanup-resolve-v2\0");
    hasher.update(serialized);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Frozen schema-v3 digest serialization. Retain its exact domain and field order.
fn legacy_v3_cleanup_resolve_digest(
    plan: &CleanupResolvePlan,
    ignored_policy: &IgnoredPolicyProof<'_>,
) -> Result<String, String> {
    let input = DigestInputV3 {
        schema_version: LEGACY_CLEANUP_RESOLVE_SCHEMA_VERSION,
        recovery_archive_schema_version: PREVIOUS_BUNDLE_RECOVERY_ARCHIVE_SCHEMA_VERSION,
        max_recovery_bundle_bytes: 1_073_741_824,
        session_id: plan.session_id,
        worktree_path: &plan.worktree_path,
        disposition: plan.disposition.as_str(),
        head: &plan.head,
        delivery_targets: &plan.delivery_targets,
        unlanded_commits: &plan.unlanded_commits,
        staged_patch_sha256: &plan.staged_patch_sha256,
        unstaged_patch_sha256: &plan.unstaged_patch_sha256,
        untracked_files: &plan.untracked_files,
        ignored_paths: &plan.ignored_paths,
        ignored_files: &plan.ignored_files,
        ignored_policy,
    };
    let serialized = serde_json::to_vec(&input)
        .map_err(|error| format!("cannot encode legacy v3 plan digest: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(b"aethyme-cleanup-resolve-v3\0");
    hasher.update(serialized);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Frozen schema-v4 digest serialization used by some schema-v3 archives.
/// Keep its evidence snapshot and field order exact.
fn legacy_v4_cleanup_resolve_digest(
    plan: &CleanupResolvePlan,
    ignored_policy: &IgnoredPolicyProof<'_>,
) -> Result<String, String> {
    let bound_ignored_paths = authorization_ignored_paths(&plan.ignored_paths);
    let input = DigestInputV4 {
        schema_version: LEGACY_CLEANUP_RESOLVE_SCHEMA_VERSION,
        recovery_archive_schema_version: PREVIOUS_BUNDLE_RECOVERY_ARCHIVE_SCHEMA_VERSION,
        max_recovery_bundle_bytes: 1_073_741_824,
        session_id: plan.session_id,
        worktree_path: &plan.worktree_path,
        disposition: plan.disposition.as_str(),
        head: &plan.head,
        delivery_targets: &plan.delivery_targets,
        unlanded_commits: &plan.unlanded_commits,
        staged_patch_sha256: &plan.staged_patch_sha256,
        unstaged_patch_sha256: &plan.unstaged_patch_sha256,
        untracked_files: &plan.untracked_files,
        ignored_paths: &bound_ignored_paths,
        ignored_files: &plan.ignored_files,
        ignored_policy,
    };
    let serialized = serde_json::to_vec(&input)
        .map_err(|error| format!("cannot encode legacy v4 plan digest: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(b"aethyme-cleanup-resolve-v4\0");
    hasher.update(serialized);
    Ok(format!("{:x}", hasher.finalize()))
}

/// The portable mode field is intentionally absent where Unix mode bits are
/// unavailable. Never guess a mode on another platform.
fn check_archive_deadline(deadline: Option<Instant>, label: &str) -> Result<(), String> {
    if effective_archive_deadline(deadline).is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(format!("{label} exceeded its bounded background deadline"));
    }
    Ok(())
}

fn check_archive_deadline_io(deadline: Option<Instant>) -> std::io::Result<()> {
    if effective_archive_deadline(deadline).is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "bounded archive operation deadline expired",
        ));
    }
    Ok(())
}

fn ensure_auto_retirement_staging_reserve(
    deadline: Option<Instant>,
    session_id: i64,
) -> Result<(), BrokerOpError> {
    if deadline.is_some_and(|deadline| {
        deadline.saturating_duration_since(Instant::now()) < AUTO_COMPACTION_STAGING_RESERVE
    }) {
        return Err(BrokerOpError::CleanupResolveRefused {
            id: session_id,
            reason: "automatic retirement reached its bounded staging reserve; retry in a later background pass".into(),
        });
    }
    Ok(())
}

fn effective_archive_deadline(deadline: Option<Instant>) -> Option<Instant> {
    match (deadline, crate::git::active_git_deadline()) {
        (Some(explicit), Some(active)) => Some(explicit.min(active)),
        (Some(explicit), None) => Some(explicit),
        (None, Some(active)) => Some(active),
        (None, None) => None,
    }
}

fn permission_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn open_regular_file_no_follow(path: &Path) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself so metadata below can reject it.
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    Ok((file, metadata))
}

fn read_regular_file_no_follow(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    let (file, metadata) = open_regular_file_no_follow(path)?;
    if metadata.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "regular file exceeds its read bound",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(max_bytes) as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "regular file grew beyond its read bound",
        ));
    }
    Ok(bytes)
}

fn hash_regular_file_no_follow_until(
    path: &Path,
    max_bytes: u64,
    deadline: Option<Instant>,
) -> std::io::Result<(u64, String)> {
    let (mut file, metadata) = open_regular_file_no_follow(path)?;
    if metadata.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "regular file exceeds its hash bound",
        ));
    }
    let expected_len = metadata.len();
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check_archive_deadline_io(deadline)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes.checked_add(count as u64).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "file length overflow")
        })?;
        if bytes > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "regular file grew beyond its hash bound",
            ));
        }
        hasher.update(&buffer[..count]);
    }
    check_archive_deadline_io(deadline)?;
    if bytes != expected_len || file.metadata()?.len() != expected_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "regular file changed while it was hashed",
        ));
    }
    Ok((bytes, format!("{:x}", hasher.finalize())))
}

fn io_error(path: &Path, source: std::io::Error) -> BrokerOpError {
    BrokerError::Io {
        path: path.to_path_buf(),
        source,
    }
    .into()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Hash what an untracked path is, without following a link.
#[cfg(test)]
fn describe_untracked(full: &Path, relative: &str) -> std::io::Result<Option<ArchivedFile>> {
    describe_untracked_until(full, relative, None)
}

fn describe_untracked_until(
    full: &Path,
    relative: &str,
    deadline: Option<Instant>,
) -> std::io::Result<Option<ArchivedFile>> {
    check_archive_deadline_io(deadline)?;
    let metadata = std::fs::symlink_metadata(full)?;
    if metadata.file_type().is_symlink() {
        check_archive_deadline_io(deadline)?;
        let target = std::fs::read_link(full)?;
        check_archive_deadline_io(deadline)?;
        let mut hashed = b"symlink\0".to_vec();
        hashed.extend_from_slice(target.as_os_str().as_encoded_bytes());
        return Ok(Some(ArchivedFile {
            path: relative.to_owned(),
            bytes: 0,
            sha256: sha256_hex(&hashed),
            symlink: true,
            mode: None,
        }));
    }
    if !metadata.is_file() {
        return Ok(None);
    }
    let mut hasher = Sha256::new();
    let (mut file, opened_metadata) = open_regular_file_no_follow(full)?;
    let mode = permission_mode(&opened_metadata);
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        check_archive_deadline_io(deadline)?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes.checked_add(read as u64).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "file size overflows u64 while it was read",
            )
        })?;
        hasher.update(&buffer[..read]);
    }
    check_archive_deadline_io(deadline)?;
    let final_metadata = file.metadata()?;
    if final_metadata.len() != bytes || permission_mode(&final_metadata) != mode {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file identity changed while it was read",
        ));
    }
    Ok(Some(ArchivedFile {
        path: relative.to_owned(),
        bytes,
        sha256: format!("{:x}", hasher.finalize()),
        symlink: false,
        mode,
    }))
}

fn ignored_inventory(
    checkout: &GitRepo,
    worktree: &Path,
    policy: &crate::AutoCleanupPolicy,
    session_id: i64,
    deadline: Option<Instant>,
) -> Result<(Vec<IgnoredPath>, Vec<ArchivedFile>), BrokerOpError> {
    let refuse = |reason: String| BrokerOpError::CleanupResolveRefused {
        id: session_id,
        reason,
    };
    let mut roots = Vec::new();
    let mut files = Vec::new();
    check_archive_deadline(deadline, "ignored-path inventory").map_err(refuse)?;
    for relative in checkout.ignored_paths_readonly()? {
        check_archive_deadline(deadline, "ignored-path inventory").map_err(refuse)?;
        let trimmed = relative.trim_end_matches('/');
        if !is_contained_relative(trimmed) {
            return Err(refuse(format!(
                "Git reported an ignored path outside the worktree: {relative:?}"
            )));
        }
        let full = worktree.join(trimmed);
        let metadata = std::fs::symlink_metadata(&full).map_err(|error| io_error(&full, error))?;
        let file_type = metadata.file_type();
        if !metadata.is_dir() && !metadata.is_file() && !file_type.is_symlink() {
            return Err(refuse(format!(
                "ignored path {relative} is a special file, which an archive cannot preserve"
            )));
        }
        if relative.ends_with('/') && (!metadata.is_dir() || file_type.is_symlink()) {
            return Err(refuse(format!(
                "Git listed ignored path {relative} as a directory, but it is not one"
            )));
        }
        let classification = match crate::auto_cleanup::classify_ignored_for_recovery(
            worktree, &relative, policy,
        )
        .map_err(refuse)?
        {
            crate::auto_cleanup::RecoveryIgnoredClassification::DisposableDirectory => {
                if !metadata.is_dir() || file_type.is_symlink() {
                    return Err(refuse(format!(
                        "ignored path {relative} was classified as a directory but is not one"
                    )));
                }
                IgnoredPathClassification::DisposableDirectory
            }
            crate::auto_cleanup::RecoveryIgnoredClassification::DisposableOutput => {
                if metadata.is_dir() && !file_type.is_symlink() {
                    IgnoredPathClassification::DisposableDirectory
                } else {
                    IgnoredPathClassification::DisposableOutput
                }
            }
            crate::auto_cleanup::RecoveryIgnoredClassification::Preserve(_) => {
                let Some(file) = describe_untracked_until(&full, &relative, deadline)
                    .map_err(|error| io_error(&full, error))?
                else {
                    return Err(refuse(format!(
                        "ignored path {relative} is neither a regular file nor a symbolic link"
                    )));
                };
                files.push(file);
                IgnoredPathClassification::PreservedFile
            }
            crate::auto_cleanup::RecoveryIgnoredClassification::StrictRefusal => {
                return Err(refuse(format!(
                    "strict cleanup requires no ignored leftovers; remove {relative} manually or review an explicit discard"
                )));
            }
        };
        roots.push(IgnoredPath {
            path: relative,
            classification,
        });
    }
    roots.sort_by(|left, right| left.path.cmp(&right.path));
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let opaque_directories = roots
        .iter()
        .filter(|root| root.classification == IgnoredPathClassification::DisposableDirectory)
        .map(|root| root.path.trim_end_matches('/').to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    reject_unarchivable_special_nodes(worktree, &opaque_directories, deadline).map_err(refuse)?;
    Ok((roots, files))
}

/// Git does not report every unsupported filesystem node (for example, an
/// ignored FIFO). Walk only outside policy-approved opaque ignored directories
/// and refuse if the bounded, no-follow scan cannot establish that the tree
/// contains no special nodes.
fn reject_unarchivable_special_nodes(
    worktree: &Path,
    opaque_directories: &std::collections::BTreeSet<String>,
    deadline: Option<Instant>,
) -> Result<(), String> {
    let root_metadata = std::fs::symlink_metadata(worktree)
        .map_err(|error| format!("cannot inspect worktree root: {error}"))?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err("worktree root is not a real directory".into());
    }

    let mut pending = vec![(PathBuf::new(), String::new(), 0_usize)];
    let mut visited = 0_usize;
    while let Some((relative_dir, relative_dir_string, depth)) = pending.pop() {
        check_archive_deadline(deadline, "worktree special-file scan")?;
        if depth > MAX_WORKTREE_SPECIAL_SCAN_DEPTH {
            return Err(format!(
                "worktree directory depth exceeds the special-file scan bound of {MAX_WORKTREE_SPECIAL_SCAN_DEPTH}"
            ));
        }
        let directory = worktree.join(&relative_dir);
        let metadata = std::fs::symlink_metadata(&directory)
            .map_err(|error| format!("cannot inspect {}: {error}", directory.display()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "worktree directory changed during the special-file scan: {}",
                directory.display()
            ));
        }
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        for entry in entries {
            check_archive_deadline(deadline, "worktree special-file scan")?;
            let entry = entry.map_err(|error| {
                format!(
                    "cannot read worktree entry under {}: {error}",
                    directory.display()
                )
            })?;
            visited += 1;
            if visited > MAX_WORKTREE_SPECIAL_SCAN_ENTRIES {
                return Err(format!(
                    "worktree exceeds the special-file scan bound of {MAX_WORKTREE_SPECIAL_SCAN_ENTRIES} entries outside opaque ignored directories"
                ));
            }
            let name = entry.file_name().into_string().map_err(|_| {
                format!(
                    "worktree contains a non-UTF-8 path under {}; cannot prove its filesystem inventory",
                    directory.display()
                )
            })?;
            let relative = if relative_dir_string.is_empty() {
                name.clone()
            } else {
                format!("{relative_dir_string}/{name}")
            };
            let file_type = entry
                .file_type()
                .map_err(|error| format!("cannot inspect {relative}: {error}"))?;
            if file_type.is_dir() {
                // A primary-checkout fixture may have a `.git` directory;
                // linked worktrees have a `.git` file. Git metadata is outside
                // the recoverable worktree payload.
                if relative == ".git" || opaque_directories.contains(&relative) {
                    continue;
                }
                pending.push((relative_dir.join(&name), relative, depth + 1));
            } else if !file_type.is_file() && !file_type.is_symlink() {
                return Err(format!(
                    "worktree contains an unsupported special file: {relative}"
                ));
            }
        }
    }
    Ok(())
}

/// A relative path that stays inside the directory it is joined to.
fn is_contained_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

impl Broker {
    /// Where recovery archives for this repository live: host state, beside
    /// the worktrees, so an archive outlives both the worktree and a deleted
    /// clone. A repository under the system temp directory (a test fixture or
    /// scratch clone) keeps them in its own `.aethyme/` instead, by the same
    /// rule that keeps its worktrees out of durable host state.
    pub(crate) fn recovery_archive_root(&self) -> Result<PathBuf, BrokerOpError> {
        let key = self.worktree_root_plan()?.repository_key;
        let storage = self.storage_context();
        let durable = storage.host_state_dir.as_ref().filter(|_| {
            storage.host_state_dir_is_explicit
                || !crate::host_state::path_is_ephemeral(self.main_root())
        });
        Ok(match durable {
            Some(base) => base.join(RECOVERY_ARCHIVE_DIRECTORY).join(key),
            None => self
                .main_root()
                .join(".aethyme")
                .join(RECOVERY_ARCHIVE_DIRECTORY),
        })
    }

    /// Plan preserving one closed session's worktree before removing it.
    /// Read-only: nothing is written or removed.
    pub fn cleanup_resolve_plan(
        &self,
        session_id: i64,
    ) -> Result<CleanupResolvePlan, BrokerOpError> {
        self.cleanup_resolve_plan_with_limits(
            session_id,
            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
        )
    }

    /// Plan compact recovery followed by retirement of one closed session's
    /// checkout, including one ordinary cleanup can already remove. Adopted
    /// rows require the explicit opt-in again when applying this plan.
    pub fn cleanup_resolve_retirement_plan(
        &self,
        session_id: i64,
        allow_adopted: bool,
    ) -> Result<CleanupResolvePlan, BrokerOpError> {
        let context = self.cleanup_resolve_retirement_context()?;
        self.cleanup_resolve_retirement_plan_with_context(session_id, allow_adopted, &context)
    }

    /// Capture shared immutable inputs once for an exact-target batch.
    pub(crate) fn cleanup_resolve_retirement_context(
        &self,
    ) -> Result<CleanupResolveRetirementContext, BrokerOpError> {
        self.cleanup_resolve_retirement_context_until(None)
    }

    pub(crate) fn cleanup_resolve_retirement_context_until(
        &self,
        deadline: Option<Instant>,
    ) -> Result<CleanupResolveRetirementContext, BrokerOpError> {
        let _git_deadline = deadline.map(crate::git::limit_git_until);
        let delivery_targets = self.cleanup_delivery_targets()?;
        let live_checkouts = self.cleanup_live_checkout_snapshot(deadline)?;
        Ok(CleanupResolveRetirementContext {
            ancestry_memo: PlanAncestryMemo::for_delivery_targets(&delivery_targets, deadline),
            delivery_targets,
            live_checkouts,
            deadline,
        })
    }

    pub(crate) fn cleanup_resolve_retirement_plan_with_context(
        &self,
        session_id: i64,
        allow_adopted: bool,
        context: &CleanupResolveRetirementContext,
    ) -> Result<CleanupResolvePlan, BrokerOpError> {
        let _git_deadline = context.deadline.map(crate::git::limit_git_until);
        self.cleanup_resolve_plan_with_limits_and_eligibility(
            session_id,
            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            true,
            allow_adopted,
            true,
            None,
            Some(context),
        )
    }

    pub(crate) fn cleanup_resolve_retirement_plan_with_witness_and_context(
        &self,
        session_id: i64,
        witness_sha256: &str,
        context: &CleanupResolveRetirementContext,
    ) -> Result<CleanupResolvePlan, BrokerOpError> {
        let _git_deadline = context.deadline.map(crate::git::limit_git_until);
        self.cleanup_resolve_plan_with_limits_and_eligibility(
            session_id,
            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            true,
            false,
            true,
            Some(witness_sha256),
            Some(context),
        )
    }

    fn cleanup_resolve_plan_with_limits(
        &self,
        session_id: i64,
        status_output_bytes: usize,
        status_path_fields: usize,
        inventory_output_bytes: usize,
        inventory_path_fields: usize,
    ) -> Result<CleanupResolvePlan, BrokerOpError> {
        self.cleanup_resolve_plan_with_limits_and_eligibility(
            session_id,
            status_output_bytes,
            status_path_fields,
            inventory_output_bytes,
            inventory_path_fields,
            false,
            false,
            false,
            None,
            None,
        )
    }

    /// Rebuild the full recovery inventory under the lifecycle claim even when
    /// ordinary cleanup can now remove the session. The archive caller still
    /// verifies the original archive and target-history monotonicity.
    pub(crate) fn cleanup_resolve_revalidation_plan(
        &self,
        session_id: i64,
    ) -> Result<CleanupResolvePlan, BrokerOpError> {
        self.cleanup_resolve_plan_with_limits_and_eligibility(
            session_id,
            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            true,
            false,
            false,
            None,
            None,
        )
    }

    pub(crate) fn cleanup_resolve_retirement_revalidation_plan(
        &self,
        session_id: i64,
        allow_adopted: bool,
        witness_sha256: Option<&str>,
        deadline: Instant,
    ) -> Result<CleanupResolvePlan, BrokerOpError> {
        let context = self.cleanup_resolve_retirement_context_until(Some(deadline))?;
        match witness_sha256 {
            Some(witness_sha256) => self.cleanup_resolve_retirement_plan_with_witness_and_context(
                session_id,
                witness_sha256,
                &context,
            ),
            None => self.cleanup_resolve_retirement_plan_with_context(
                session_id,
                allow_adopted,
                &context,
            ),
        }
    }

    // Keep these inputs explicit at the archive proof boundary: each bound and
    // eligibility flag narrows what the plan may authorize.
    #[allow(clippy::too_many_arguments)]
    fn cleanup_resolve_plan_with_limits_and_eligibility(
        &self,
        session_id: i64,
        status_output_bytes: usize,
        status_path_fields: usize,
        inventory_output_bytes: usize,
        inventory_path_fields: usize,
        allow_eligible: bool,
        allow_adopted: bool,
        retirement_mode: bool,
        retirement_witness_sha256: Option<&str>,
        planning_context: Option<&CleanupResolveRetirementContext>,
    ) -> Result<CleanupResolvePlan, BrokerOpError> {
        let refuse = |reason: String| BrokerOpError::CleanupResolveRefused {
            id: session_id,
            reason,
        };
        let session = self.store_ref().session(session_id)?;
        if !session.status.is_closed() {
            return Err(refuse(format!(
                "the session is still {}; close it first with `aethyme broker finish close --session {session_id}`",
                session.status.as_str()
            )));
        }
        if session.origin != SessionOrigin::Spawned
            && !(allow_adopted && session.origin == SessionOrigin::Adopted)
        {
            return Err(refuse(
                "only broker-spawned worktrees are archived and removed; an adopted checkout \
                 belongs to whoever adopted it"
                    .into(),
            ));
        }
        // Pin one delivery-target and live-checkout snapshot before the
        // recovery-specific eligibility pass. Batches share both read-only
        // snapshots; standalone plans build them once here.
        let owned_context;
        let context = if let Some(context) = planning_context {
            context
        } else {
            owned_context = self.cleanup_resolve_retirement_context()?;
            &owned_context
        };
        let _git_deadline = context.deadline.map(crate::git::limit_git_until);
        let delivery_targets = &context.delivery_targets;
        if retirement_mode
            && (delivery_targets.is_empty()
                || delivery_targets.len() > MAX_RECOVERY_ARCHIVE_PREREQUISITE_REFS)
        {
            return Err(refuse(format!(
                "retirement requires 1 to {MAX_RECOVERY_ARCHIVE_PREREQUISITE_REFS} captured delivery prerequisites"
            )));
        }
        let mut records = crate::measurement::SizeRecords::default();
        let Some(item) = self
            .cleanup_item_for_recovery_archive_with_status_limits_and_live_checkouts(
                &session,
                crate::SizeScan::Recorded,
                &mut records,
                status_output_bytes,
                status_path_fields,
                delivery_targets,
                &context.ancestry_memo,
                allow_adopted && session.origin == SessionOrigin::Adopted,
                &context.live_checkouts,
            )?
        else {
            return Err(refuse(
                "nothing is retained: its worktree and branch are already gone".into(),
            ));
        };
        match item.disposition {
            CleanupDisposition::Eligible => {}
            CleanupDisposition::UnsafePath | CleanupDisposition::InspectionFailed => {
                return Err(refuse(format!(
                    "its worktree is {} ({}), which resolve does not archive",
                    item.disposition.as_str(),
                    item.reason
                )));
            }
            CleanupDisposition::Dirty
            | CleanupDisposition::PendingCommits
            | CleanupDisposition::UnprovenProvenance => {}
        }
        let worktree = PathBuf::from(&session.worktree_path);
        if !item.worktree_present || crate::broker::is_orphaned_worktree_directory(&worktree) {
            return Err(refuse(
                "its worktree is not a readable Git checkout, so there is nothing to archive \
                 from; the session branch still records its commits"
                    .into(),
            ));
        }
        let checkout = GitRepo::discover(&worktree)?;
        let tracked_submodules = checkout.tracked_submodule_paths()?;
        if !tracked_submodules.is_empty() {
            return Err(refuse(format!(
                "the recovery archive does not include child Git repositories for tracked submodules {tracked_submodules:?}; preserve their commits and worktrees separately before archiving this session"
            )));
        }
        if checkout.has_submodule_git_metadata()? {
            return Err(refuse(
                "the repository retains submodule Git metadata that the recovery archive cannot inspect or include; preserve child repositories separately before archiving this session".into(),
            ));
        }
        let head = checkout.head_commit()?;
        let branch_tip = item.branch_tip.clone();
        let checkout_branch_ref = checkout.symbolic_ref("HEAD");
        if checkout_branch_ref
            .as_deref()
            .is_some_and(|branch| !branch.starts_with("refs/heads/"))
        {
            return Err(refuse(
                "the worktree HEAD is attached to a non-branch symbolic ref; preserving this checkout is required".into(),
            ));
        }
        let checkout_branch_tip = checkout_branch_ref
            .as_deref()
            .map(|branch| {
                checkout.resolve_ref(branch).ok_or_else(|| {
                    refuse("the checked-out branch no longer resolves to a commit".into())
                })
            })
            .transpose()?;
        if checkout_branch_tip
            .as_deref()
            .is_some_and(|tip| tip != head)
        {
            return Err(refuse(
                "the checked-out branch no longer points to the exact worktree HEAD".into(),
            ));
        }
        let branch_tip_mismatch = branch_tip.as_deref().is_some_and(|tip| tip != head);
        let checkout_branch_mismatch = checkout_branch_ref
            .as_deref()
            .is_some_and(|branch| branch != item.branch_ref);
        if (branch_tip_mismatch || checkout_branch_mismatch) && !retirement_mode {
            return Err(refuse(format!(
                "the registered session branch ({}, {}) differs from the current worktree branch/HEAD ({}, {}); refusing to archive only one identity",
                item.branch_ref,
                branch_tip.as_deref().unwrap_or("(missing)"),
                checkout_branch_ref.as_deref().unwrap_or("(detached)"),
                head
            )));
        }
        if checkout.resolve_ref(&item.branch_ref).as_deref() != branch_tip.as_deref() {
            return Err(refuse(
                "the registered session branch no longer resolves to its reviewed tip".into(),
            ));
        }
        if item.disposition == CleanupDisposition::Eligible && !allow_eligible {
            return Err(refuse(format!(
                "cleanup can already remove it without an archive ({}); run \
                 `aethyme broker finish cleanup {session_id}`",
                item.reason
            )));
        }
        let mut unlanded_commits = checkout.commits_not_on_any(&head, delivery_targets)?;
        if retirement_mode
            && let Some(branch_tip) = branch_tip.as_deref()
            && (branch_tip != head || checkout_branch_mismatch)
        {
            unlanded_commits.extend(checkout.commits_not_on_any(branch_tip, delivery_targets)?);
        }
        if retirement_mode
            && let Some(checkout_tip) = checkout_branch_tip.as_deref()
            && checkout_branch_ref.as_deref() != Some(item.branch_ref.as_str())
        {
            unlanded_commits.extend(checkout.commits_not_on_any(checkout_tip, delivery_targets)?);
        }
        unlanded_commits.sort();
        unlanded_commits.dedup();
        let landing = RecoveryLandingAssessment::NotEvaluatedForRecovery {
            state: RecoveryLandingState::NotEvaluatedForRecovery,
        };
        let (staged_patch_bytes, staged_patch_sha256) = {
            let staged = checkout.recovery_patch(true)?;
            let bytes = u64::try_from(staged.len())
                .map_err(|_| refuse("staged recovery patch is too large to measure".into()))?;
            (bytes, sha256_hex(&staged))
        };
        let (unstaged_patch_bytes, unstaged_patch_sha256) = {
            let unstaged = checkout.recovery_patch(false)?;
            let bytes = u64::try_from(unstaged.len())
                .map_err(|_| refuse("unstaged recovery patch is too large to measure".into()))?;
            (bytes, sha256_hex(&unstaged))
        };

        let inventory_deadline = context.deadline.map_or_else(
            || std::time::Instant::now() + std::time::Duration::from_secs(15),
            |deadline| deadline.min(std::time::Instant::now() + std::time::Duration::from_secs(15)),
        );
        let untracked_paths = checkout
            .untracked_paths_until_with_limits(
                inventory_deadline,
                inventory_output_bytes,
                inventory_path_fields,
            )
            .map_err(|error| {
                refuse(format!(
                    "could not completely list untracked archive paths: {error}"
                ))
            })?;
        let mut untracked_files = Vec::new();
        for relative in untracked_paths {
            if relative.ends_with('/') {
                return Err(refuse(format!(
                    "{relative} is a nested repository, which an archive of this checkout \
                     cannot preserve; move or remove it by hand first"
                )));
            }
            if !is_contained_relative(&relative) {
                return Err(refuse(format!(
                    "Git reported an untracked path outside the worktree: {relative:?}"
                )));
            }
            let full = worktree.join(&relative);
            match describe_untracked_until(&full, &relative, context.deadline)
                .map_err(|error| io_error(&full, error))?
            {
                Some(file) => untracked_files.push(file),
                None => {
                    return Err(refuse(format!(
                        "{relative} is neither a regular file nor a symbolic link, so it \
                         cannot be archived"
                    )));
                }
            }
        }
        untracked_files.sort_by(|left, right| left.path.cmp(&right.path));
        let untracked_bytes = checked_archived_file_bytes(&untracked_files)
            .ok_or_else(|| refuse("the total untracked archive size overflows u64".into()))?;

        let ignored_policy = crate::AutoCleanupPolicy::load(self.main_root())
            .map_err(|reason| refuse(format!("cannot read ignored cleanup policy: {reason}")))?;
        let ignored_policy_proof = IgnoredPolicyProof {
            strict: ignored_policy.strict,
            regenerable: &ignored_policy.regenerable,
        };
        let (ignored_paths, ignored_files) = ignored_inventory(
            &checkout,
            &worktree,
            &ignored_policy,
            session_id,
            context.deadline,
        )?;
        let ignored_bytes = checked_archived_file_bytes(&ignored_files)
            .ok_or_else(|| refuse("the total ignored archive size overflows u64".into()))?;
        let ignored_policy_bytes =
            serde_json::to_vec(&ignored_policy_proof).map_err(|error| refuse(error.to_string()))?;
        let ignored_policy_sha256 = sha256_hex(&ignored_policy_bytes);

        let bound_ignored_paths = authorization_ignored_paths(&ignored_paths);
        let repository_path = self
            .main_root()
            .canonicalize()
            .map_err(|error| refuse(format!("cannot resolve repository identity: {error}")))?
            .to_string_lossy()
            .into_owned();
        let digest_input = DigestInputV5 {
            schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
            recovery_archive_schema_version: RECOVERY_ARCHIVE_SCHEMA_VERSION,
            max_recovery_bundle_bytes: MAX_RECOVERY_BUNDLE_BYTES,
            session_id,
            repository_path: &repository_path,
            worktree_path: &session.worktree_path,
            branch_ref: &item.branch_ref,
            branch_tip: branch_tip.as_deref(),
            head: &head,
            staged_patch_sha256: &staged_patch_sha256,
            unstaged_patch_sha256: &unstaged_patch_sha256,
            untracked_files: &untracked_files,
            ignored_paths: &bound_ignored_paths,
            ignored_files: &ignored_files,
            ignored_policy: &ignored_policy_proof,
        };
        let mut hasher = Sha256::new();
        hasher.update(b"aethyme-cleanup-resolve-v5\0");
        hasher
            .update(serde_json::to_vec(&digest_input).map_err(|error| refuse(error.to_string()))?);
        let content_digest = format!("{:x}", hasher.finalize());
        let recovery_root_proofs = if retirement_mode {
            recovery_root_proofs_for_plan_parts(
                self.repo_handle(),
                &head,
                &item.branch_ref,
                branch_tip.as_deref(),
                (
                    checkout_branch_ref.as_deref(),
                    checkout_branch_tip.as_deref(),
                ),
                delivery_targets,
                &context.ancestry_memo,
            )
            .map_err(refuse)?
        } else {
            Vec::new()
        };
        let recovery_root_proofs_sha256 = if retirement_mode {
            Some(recovery_root_proofs_sha256(&recovery_root_proofs).map_err(refuse)?)
        } else {
            None
        };
        let delivery_targets_sha256 = if retirement_mode {
            Some(delivery_targets_sha256(delivery_targets).map_err(refuse)?)
        } else {
            None
        };
        let minimum_archive_retention_ms = if retirement_mode {
            let retention = crate::load_retention_policy(self.main_root())
                .map_err(|error| refuse(format!("cannot read archive retention floor: {error}")))?;
            Some(retention.compact_archive_minimum_retention_ms())
        } else {
            None
        };
        let recovery_archive_schema_version = if retirement_mode {
            PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        } else {
            RECOVERY_ARCHIVE_SCHEMA_VERSION
        };
        let digest = if retirement_mode
            && recovery_archive_schema_version == RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        {
            let retirement_input = RetirementDigestInputV1 {
                content_digest: &content_digest,
                recovery_archive_schema_version,
                minimum_archive_retention_ms: minimum_archive_retention_ms
                    .expect("retirement plans have a minimum archive retention"),
                target_origin: session.origin.as_str(),
                allow_adopted,
                owner_witness_sha256: retirement_witness_sha256,
                delivery_targets_sha256: delivery_targets_sha256
                    .as_deref()
                    .expect("retirement plans bind delivery roots"),
                recovery_root_proofs_sha256: recovery_root_proofs_sha256
                    .as_deref()
                    .expect("retirement plans have root proof identity"),
            };
            let mut hasher = Sha256::new();
            hasher.update(b"aethyme-cleanup-retirement-v1\0");
            hasher.update(
                serde_json::to_vec(&retirement_input).map_err(|error| refuse(error.to_string()))?,
            );
            format!("{:x}", hasher.finalize())
        } else if retirement_mode {
            let mut root_identities = recovery_root_proofs
                .iter()
                .map(|proof| (proof.ref_name.as_str(), proof.object_id.as_str()))
                .collect::<Vec<_>>();
            root_identities.sort_unstable();
            root_identities.dedup();
            let retirement_input = RetirementDigestInputV2 {
                content_digest: &content_digest,
                recovery_archive_schema_version,
                minimum_archive_retention_ms: minimum_archive_retention_ms
                    .expect("retirement plans have a minimum archive retention"),
                target_origin: session.origin.as_str(),
                allow_adopted,
                owner_witness_sha256: retirement_witness_sha256,
                checkout_branch_ref: checkout_branch_ref.as_deref(),
                checkout_branch_tip: checkout_branch_tip.as_deref(),
                recovery_root_identities: root_identities,
            };
            let mut hasher = Sha256::new();
            hasher.update(b"aethyme-cleanup-retirement-v2\0");
            hasher.update(
                serde_json::to_vec(&retirement_input).map_err(|error| refuse(error.to_string()))?,
            );
            format!("{:x}", hasher.finalize())
        } else {
            content_digest
        };
        let apply_command = if retirement_mode {
            let adopted_opt_in = if allow_adopted {
                " --allow-adopted"
            } else {
                ""
            };
            format!(
                "aethyme broker finish cleanup resolve {session_id} --archive --retire{adopted_opt_in} --confirm {digest}"
            )
        } else {
            format!(
                "aethyme broker finish cleanup resolve {session_id} --archive --confirm {digest}"
            )
        };

        Ok(CleanupResolvePlan {
            schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
            apply_command,
            discard_command: item.force_cleanup_command.clone(),
            digest,
            session_id,
            target_origin: session.origin,
            recovery_archive_schema_version,
            minimum_archive_retention_ms,
            retirement_allow_adopted: retirement_mode.then_some(allow_adopted),
            retirement_witness_sha256: retirement_witness_sha256.map(str::to_owned),
            repository_path,
            worktree_path: session.worktree_path.clone(),
            branch_ref: item.branch_ref,
            branch_tip,
            checkout_branch_ref,
            checkout_branch_tip,
            recovery_root_proofs,
            disposition: item.disposition,
            reason: item.reason,
            head,
            delivery_targets: delivery_targets.to_vec(),
            landing,
            unlanded_commits,
            staged_patch_bytes,
            staged_patch_sha256,
            unstaged_patch_bytes,
            unstaged_patch_sha256,
            untracked_files,
            ignored_paths,
            ignored_files,
            ignored_bytes,
            ignored_policy_sha256,
            untracked_bytes,
            archive_root: self.recovery_archive_root()?,
        })
    }

    /// Apply a reviewed resolve plan: write the archive, verify it by reading
    /// it back, and only then remove the worktree through ordinary cleanup.
    pub fn cleanup_resolve_apply(
        &mut self,
        session_id: i64,
        confirm: &str,
    ) -> Result<CleanupResolveOutcome, BrokerOpError> {
        self.cleanup_resolve_apply_with_post_archive_hook(session_id, confirm, || {})
    }

    /// Apply an explicitly reviewed compact-retirement plan. This uses archive
    /// schema 6, which carries an immutable retention floor and pins the exact
    /// incremental-bundle prerequisites in the repository before removal.
    pub fn cleanup_resolve_retirement_apply(
        &mut self,
        session_id: i64,
        confirm: &str,
        allow_adopted: bool,
    ) -> Result<CleanupResolveOutcome, BrokerOpError> {
        self.cleanup_resolve_apply_with_bundle_writer_mode(
            session_id,
            confirm,
            || {},
            sync_archive_directory,
            |main_root, archive_root| {
                crate::disk_headroom::available_headroom_at_or_above_for(main_root, archive_root)
            },
            true,
            allow_adopted,
            None,
            None,
            None,
            RecoveryBundleWriter {
                max_bytes: MAX_RECOVERY_BUNDLE_BYTES,
                write_to: write_recovery_bundle_to_file,
            },
        )
    }

    /// Compact a complete, safely owned closed checkout under the opt-in
    /// automatic policy. The cohort is checked before writing and again under
    /// the final lifecycle claim; the persisted owner witness is part of the
    /// deterministic retirement digest.
    pub(crate) fn compact_closed_checkout(
        &mut self,
        expected_cohort: &crate::auto_cleanup::ClosedArtifactCheckoutCohort,
        deadline: Instant,
    ) -> Result<Option<CleanupResolveOutcome>, BrokerOpError> {
        let policy = crate::load_retention_policy(self.main_root())?;
        if !policy.compact_closed_worktrees || Instant::now() >= deadline {
            return Ok(None);
        }
        let session_id = expected_cohort.owner_session_id();
        let worktree = expected_cohort.worktree();
        let started = Instant::now();
        if deadline.saturating_duration_since(started)
            < crate::auto_cleanup::AUTO_COMPACTION_MIN_BUDGET
        {
            return Ok(None);
        }
        let attempt_deadline = deadline.min(
            started
                .checked_add(crate::auto_cleanup::AUTO_COMPACTION_MIN_BUDGET)
                .unwrap_or(deadline),
        );
        let current = self.closed_artifact_checkout_cohort_for_session(
            session_id,
            worktree,
            Some(attempt_deadline),
        )?;
        if current.as_ref() != Some(expected_cohort) {
            return Ok(None);
        }
        let Some(witness) = expected_cohort.owner_witness() else {
            return Ok(None);
        };
        let witness_bytes =
            serde_json::to_vec(witness).map_err(|error| BrokerOpError::CleanupResolveRefused {
                id: session_id,
                reason: format!("cannot encode checkout owner witness: {error}"),
            })?;
        let witness_sha256 = sha256_hex(&witness_bytes);
        let _git_deadline = crate::git::limit_git_until(attempt_deadline);
        let context = self.cleanup_resolve_retirement_context_until(Some(attempt_deadline))?;
        let plan = self.cleanup_resolve_retirement_plan_with_witness_and_context(
            session_id,
            &witness_sha256,
            &context,
        )?;
        if attempt_deadline.saturating_duration_since(Instant::now())
            < AUTO_COMPACTION_STAGING_RESERVE
        {
            return Ok(None);
        }
        if plan.target_origin != SessionOrigin::Spawned
            || Path::new(&plan.worktree_path) != worktree
            || plan.retirement_witness_sha256.as_deref() != Some(witness_sha256.as_str())
        {
            return Ok(None);
        }
        let outcome = self.cleanup_resolve_apply_with_bundle_writer_mode(
            session_id,
            &plan.digest,
            || {},
            sync_archive_directory,
            |main_root, archive_root| {
                crate::disk_headroom::available_headroom_at_or_above_for(main_root, archive_root)
            },
            true,
            false,
            Some(&witness_sha256),
            Some(expected_cohort),
            Some(attempt_deadline),
            RecoveryBundleWriter {
                max_bytes: MAX_RECOVERY_BUNDLE_BYTES,
                write_to: write_recovery_bundle_to_file,
            },
        )?;
        Ok(Some(outcome))
    }

    fn cleanup_resolve_apply_with_post_archive_hook<F>(
        &mut self,
        session_id: i64,
        confirm: &str,
        after_archive_verified: F,
    ) -> Result<CleanupResolveOutcome, BrokerOpError>
    where
        F: FnOnce(),
    {
        self.cleanup_resolve_apply_with_hooks(
            session_id,
            confirm,
            after_archive_verified,
            sync_archive_directory,
            |main_root, archive_root| {
                crate::disk_headroom::available_headroom_at_or_above_for(main_root, archive_root)
            },
        )
    }

    fn cleanup_resolve_apply_with_hooks<F, S, H>(
        &mut self,
        session_id: i64,
        confirm: &str,
        after_archive_verified: F,
        sync_directory: S,
        headroom_for: H,
    ) -> Result<CleanupResolveOutcome, BrokerOpError>
    where
        F: FnOnce(),
        S: Fn(&Path) -> std::io::Result<()>,
        H: FnOnce(&Path, &Path) -> Option<crate::disk_headroom::DiskHeadroom>,
    {
        self.cleanup_resolve_apply_with_bundle_writer(
            session_id,
            confirm,
            after_archive_verified,
            sync_directory,
            headroom_for,
            RecoveryBundleWriter {
                max_bytes: MAX_RECOVERY_BUNDLE_BYTES,
                write_to: write_recovery_bundle_to_file,
            },
        )
    }

    fn cleanup_resolve_apply_with_bundle_writer<F, S, H, B>(
        &mut self,
        session_id: i64,
        confirm: &str,
        after_archive_verified: F,
        sync_directory: S,
        headroom_for: H,
        bundle: RecoveryBundleWriter<B>,
    ) -> Result<CleanupResolveOutcome, BrokerOpError>
    where
        F: FnOnce(),
        S: Fn(&Path) -> std::io::Result<()>,
        H: FnOnce(&Path, &Path) -> Option<crate::disk_headroom::DiskHeadroom>,
        B: Fn(
            &GitRepo,
            &[String],
            &[String],
            u64,
            Instant,
            &mut std::fs::File,
        ) -> Result<u64, String>,
    {
        self.cleanup_resolve_apply_with_bundle_writer_mode(
            session_id,
            confirm,
            after_archive_verified,
            sync_directory,
            headroom_for,
            false,
            false,
            None,
            None,
            None,
            bundle,
        )
    }

    // The injectable writers let tests exercise archive durability and failure
    // paths without changing the production checks or their order.
    #[allow(clippy::too_many_arguments)]
    fn cleanup_resolve_apply_with_bundle_writer_mode<F, S, H, B>(
        &mut self,
        session_id: i64,
        confirm: &str,
        after_archive_verified: F,
        sync_directory: S,
        headroom_for: H,
        retirement_mode: bool,
        allow_adopted: bool,
        retirement_witness_sha256: Option<&str>,
        expected_cohort: Option<&crate::auto_cleanup::ClosedArtifactCheckoutCohort>,
        operation_deadline: Option<Instant>,
        bundle: RecoveryBundleWriter<B>,
    ) -> Result<CleanupResolveOutcome, BrokerOpError>
    where
        F: FnOnce(),
        S: Fn(&Path) -> std::io::Result<()>,
        H: FnOnce(&Path, &Path) -> Option<crate::disk_headroom::DiskHeadroom>,
        B: Fn(
            &GitRepo,
            &[String],
            &[String],
            u64,
            Instant,
            &mut std::fs::File,
        ) -> Result<u64, String>,
    {
        let _git_deadline = operation_deadline.map(crate::git::limit_git_until);
        let _retirement_lock = if retirement_mode {
            Some(
                crate::file_lock::try_acquire_retirement_archive_lock(self.main_root(), session_id)
                    .map_err(|error| BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: format!("cannot acquire the per-session retirement archive lock: {error}"),
                    })?
                    .ok_or_else(|| BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: "another process is handling this session's retirement archive; retry after it finishes".into(),
                    })?,
            )
        } else {
            None
        };
        if confirm.len() != 64 || !confirm.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BrokerOpError::CleanupResolveConfirmationNotSha256);
        }
        let retirement_context = if retirement_mode {
            Some(self.cleanup_resolve_retirement_context_until(operation_deadline)?)
        } else {
            None
        };
        let plan = if retirement_mode {
            let context = retirement_context
                .as_ref()
                .expect("retirement mode has context");
            if let Some(witness_sha256) = retirement_witness_sha256 {
                self.cleanup_resolve_retirement_plan_with_witness_and_context(
                    session_id,
                    witness_sha256,
                    context,
                )?
            } else {
                self.cleanup_resolve_retirement_plan_with_context(
                    session_id,
                    allow_adopted,
                    context,
                )?
            }
        } else {
            self.cleanup_resolve_revalidation_plan(session_id)?
        };
        if plan.digest != confirm {
            return Err(BrokerOpError::CleanupResolveConfirmationMismatch {
                id: session_id,
                actual: confirm.to_owned(),
            });
        }
        let prerequisite_deadline = if plan.recovery_archive_schema_version
            == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        {
            let git_deadline = recovery_bundle_deadline().map_err(|reason| {
                BrokerOpError::CleanupResolveRefused {
                    id: session_id,
                    reason: format!("cannot establish prerequisite-ref deadline: {reason}"),
                }
            })?;
            Some(operation_deadline.map_or(git_deadline, |deadline| deadline.min(git_deadline)))
        } else {
            None
        };
        ensure_auto_retirement_staging_reserve(operation_deadline, session_id)?;
        let session = self.store_ref().session(session_id)?;
        let archive_root = prepare_archive_root(&plan.archive_root).map_err(|reason| {
            BrokerOpError::RecoveryArchiveUnverified {
                id: session_id,
                path: plan.archive_root.to_string_lossy().into_owned(),
                reason,
            }
        })?;
        let name = if retirement_mode
            && plan.recovery_archive_schema_version
                == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        {
            pinned_retirement_archive_name(session_id, &plan.head, &plan.digest).map_err(
                |reason| BrokerOpError::RecoveryArchiveUnverified {
                    id: session_id,
                    path: archive_root.to_string_lossy().into_owned(),
                    reason,
                },
            )?
        } else if retirement_mode {
            format!("{session_id}-{}-{}", &plan.head[..12], plan.digest)
        } else {
            format!(
                "{session_id}-{}-{}",
                &plan.head[..12],
                archive_staging_nonce()
            )
        };
        let partial_name = if plan.recovery_archive_schema_version
            == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        {
            format!("{session_id}.retirement-v6.partial")
        } else if retirement_mode {
            format!("{session_id}.retirement.partial")
        } else {
            format!("{name}.partial")
        };
        let partial_path = archive_root.join(&partial_name);
        let archive = archive_root.join(&name);
        let checkout = GitRepo::discover(Path::new(&plan.worktree_path))?;
        let mut archive_reused = false;
        match std::fs::symlink_metadata(&archive) {
            Ok(metadata)
                if retirement_mode && metadata.is_dir() && !metadata.file_type().is_symlink() =>
            {
                if plan.recovery_archive_schema_version
                    == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                {
                    verify_recovery_archive_payload_for_plan(self.repo_handle(), &archive, &plan)
                        .and_then(|()| {
                            install_recovery_prerequisite_refs_in_archive(
                                self.repo_handle(),
                                &archive,
                                prerequisite_deadline
                                    .expect("schema v6 retirement has a Git deadline"),
                            )
                        })
                        .and_then(|()| {
                            verify_recovery_archive_for_plan(self.repo_handle(), &archive, &plan)
                        })
                        .map_err(|reason| BrokerOpError::RecoveryArchiveUnverified {
                            id: session_id,
                            path: archive.to_string_lossy().into_owned(),
                            reason: format!(
                                "existing retirement receipt failed verification: {reason}"
                            ),
                        })?;
                } else {
                    verify_recovery_archive_for_plan(self.repo_handle(), &archive, &plan).map_err(
                        |reason| BrokerOpError::RecoveryArchiveUnverified {
                            id: session_id,
                            path: archive.to_string_lossy().into_owned(),
                            reason: format!(
                                "existing retirement receipt failed verification: {reason}"
                            ),
                        },
                    )?;
                }
                archive_reused = true;
            }
            Ok(_) => {
                return Err(BrokerOpError::RecoveryArchiveUnverified {
                    id: session_id,
                    path: archive.to_string_lossy().into_owned(),
                    reason: "archive destination already exists and cannot be reused".into(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&archive, error)),
        }

        if !archive_reused {
            match std::fs::symlink_metadata(&partial_path) {
                Ok(metadata)
                    if retirement_mode
                        && metadata.is_dir()
                        && !metadata.file_type().is_symlink() =>
                {
                    match verify_recovery_archive_payload_for_plan(
                        self.repo_handle(),
                        &partial_path,
                        &plan,
                    ) {
                        Ok(()) => {
                            if plan.recovery_archive_schema_version
                                == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                            {
                                install_recovery_prerequisite_refs_in_archive(
                                    self.repo_handle(),
                                    &partial_path,
                                    prerequisite_deadline
                                        .expect("schema v6 retirement has a Git deadline"),
                                )
                                .map_err(|reason| {
                                    BrokerOpError::RecoveryArchiveUnverified {
                                        id: session_id,
                                        path: partial_path.to_string_lossy().into_owned(),
                                        reason,
                                    }
                                })?;
                            }
                            sync_archive_directories_bottom_up_until(
                                &partial_path,
                                &sync_directory,
                                operation_deadline,
                            )
                            .map_err(|reason| {
                                BrokerOpError::RecoveryArchiveUnverified {
                                    id: session_id,
                                    path: partial_path.to_string_lossy().into_owned(),
                                    reason,
                                }
                            })?;
                            std::fs::rename(&partial_path, &archive)
                                .map_err(|error| io_error(&archive, error))?;
                        }
                        Err(verification_error) => {
                            verify_retirement_archive_record_for_content_identity(&partial_path, &plan)
                                .map_err(|marker_error| BrokerOpError::RecoveryArchiveUnverified {
                                    id: session_id,
                                    path: partial_path.to_string_lossy().into_owned(),
                                    reason: format!(
                                        "incomplete retirement staging is not proven to belong to the current source identity; it was retained: {verification_error}; {marker_error}"
                                    ),
                                })?;
                            let revalidation_context =
                                self.cleanup_resolve_retirement_context_until(operation_deadline)?;
                            let current_plan =
                                if let Some(witness_sha256) = retirement_witness_sha256 {
                                    self.cleanup_resolve_retirement_plan_with_witness_and_context(
                                        session_id,
                                        witness_sha256,
                                        &revalidation_context,
                                    )?
                                } else {
                                    self.cleanup_resolve_retirement_plan_with_context(
                                        session_id,
                                        allow_adopted,
                                        &revalidation_context,
                                    )?
                                };
                            if current_plan.digest != plan.digest {
                                return Err(BrokerOpError::CleanupResolveConfirmationMismatch {
                                    id: session_id,
                                    actual: confirm.to_owned(),
                                });
                            }
                            if plan.recovery_archive_schema_version
                                == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                            {
                                require_no_recovery_prerequisite_refs_for_partial(
                                    self.repo_handle(),
                                    &partial_path,
                                    prerequisite_deadline.expect("schema v6 retirement has a Git deadline"),
                                )
                                .map_err(|reason| BrokerOpError::RecoveryArchiveUnverified {
                                    id: session_id,
                                    path: partial_path.to_string_lossy().into_owned(),
                                    reason: format!(
                                        "incomplete archive has prerequisite refs and cannot be restarted safely: {reason}"
                                    ),
                                })?;
                            }
                            ensure_auto_retirement_staging_reserve(operation_deadline, session_id)?;
                            std::fs::remove_dir_all(&partial_path).map_err(|error| {
                                BrokerOpError::RecoveryArchiveUnverified {
                                    id: session_id,
                                    path: partial_path.to_string_lossy().into_owned(),
                                    reason: format!(
                                        "could not safely restart the verified incomplete staging attempt: {error}"
                                    ),
                                }
                            })?;
                            ensure_auto_retirement_staging_reserve(operation_deadline, session_id)?;
                            let partial =
                                create_exclusive_archive_directory(&archive_root, &partial_name)
                                    .map_err(|reason| BrokerOpError::RecoveryArchiveUnverified {
                                        id: session_id,
                                        path: partial_path.to_string_lossy().into_owned(),
                                        reason,
                                    })?;
                            if let Err(reason) = create_retirement_archive_record(&partial, &plan) {
                                let cleanup_error = std::fs::remove_dir_all(&partial).err();
                                let reason = match cleanup_error {
                                    Some(error) => format!(
                                        "{reason}; could not remove empty retirement staging directory: {error}"
                                    ),
                                    None => reason,
                                };
                                return Err(BrokerOpError::RecoveryArchiveUnverified {
                                    id: session_id,
                                    path: partial.to_string_lossy().into_owned(),
                                    reason,
                                });
                            }
                            if let Err(reason) = write_verified_archive_with_bundle_writer(
                                self.repo_handle(),
                                &checkout,
                                &plan,
                                session.task.as_deref(),
                                &partial,
                                &bundle,
                                &sync_directory,
                                operation_deadline,
                            ) {
                                return Err(BrokerOpError::RecoveryArchiveUnverified {
                                    id: session_id,
                                    path: partial.to_string_lossy().into_owned(),
                                    reason,
                                });
                            }
                            if plan.recovery_archive_schema_version
                                == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                            {
                                install_recovery_prerequisite_refs_in_archive(
                                    self.repo_handle(),
                                    &partial,
                                    prerequisite_deadline
                                        .expect("schema v6 retirement has a Git deadline"),
                                )
                                .map_err(|reason| {
                                    BrokerOpError::RecoveryArchiveUnverified {
                                        id: session_id,
                                        path: partial.to_string_lossy().into_owned(),
                                        reason,
                                    }
                                })?;
                                sync_archive_directories_bottom_up_until(
                                    &partial,
                                    &sync_directory,
                                    operation_deadline,
                                )
                                .map_err(|reason| {
                                    BrokerOpError::RecoveryArchiveUnverified {
                                        id: session_id,
                                        path: partial.to_string_lossy().into_owned(),
                                        reason,
                                    }
                                })?;
                            }
                            std::fs::rename(&partial, &archive)
                                .map_err(|error| io_error(&archive, error))?;
                        }
                    }
                }
                Ok(_) => {
                    return Err(BrokerOpError::RecoveryArchiveUnverified {
                        id: session_id,
                        path: partial_path.to_string_lossy().into_owned(),
                        reason: "retirement staging path exists but is not a real directory".into(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let known_payload_bytes =
                        known_recovery_archive_payload_bytes(&plan).map_err(|reason| {
                            BrokerOpError::CleanupResolveRefused {
                                id: session_id,
                                reason: format!("archive capacity preflight failed: {reason}"),
                            }
                        })?;
                    let destination_headroom = headroom_for(self.main_root(), &archive_root)
                        .map(|headroom| headroom.bytes);
                    require_recovery_archive_headroom(destination_headroom, known_payload_bytes)
                        .map_err(|reason| BrokerOpError::CleanupResolveRefused {
                            id: session_id,
                            reason: format!("archive capacity preflight failed: {reason}"),
                        })?;
                    ensure_auto_retirement_staging_reserve(operation_deadline, session_id)?;
                    let partial = create_exclusive_archive_directory(&archive_root, &partial_name)
                        .map_err(|reason| BrokerOpError::RecoveryArchiveUnverified {
                            id: session_id,
                            path: partial_path.to_string_lossy().into_owned(),
                            reason,
                        })?;
                    if retirement_mode
                        && let Err(reason) = create_retirement_archive_record(&partial, &plan)
                    {
                        let cleanup_error = std::fs::remove_dir_all(&partial).err();
                        let reason = match cleanup_error {
                            Some(error) => format!(
                                "{reason}; could not remove empty retirement staging directory: {error}"
                            ),
                            None => reason,
                        };
                        return Err(BrokerOpError::RecoveryArchiveUnverified {
                            id: session_id,
                            path: partial.to_string_lossy().into_owned(),
                            reason,
                        });
                    }
                    if let Err(reason) = write_verified_archive_with_bundle_writer(
                        self.repo_handle(),
                        &checkout,
                        &plan,
                        session.task.as_deref(),
                        &partial,
                        &bundle,
                        &sync_directory,
                        operation_deadline,
                    ) {
                        return Err(BrokerOpError::RecoveryArchiveUnverified {
                            id: session_id,
                            path: partial.to_string_lossy().into_owned(),
                            reason,
                        });
                    }
                    if plan.recovery_archive_schema_version
                        == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                    {
                        install_recovery_prerequisite_refs_in_archive(
                            self.repo_handle(),
                            &partial,
                            prerequisite_deadline.expect("schema v6 retirement has a Git deadline"),
                        )
                        .map_err(|reason| {
                            BrokerOpError::RecoveryArchiveUnverified {
                                id: session_id,
                                path: partial.to_string_lossy().into_owned(),
                                reason,
                            }
                        })?;
                        sync_archive_directories_bottom_up_until(
                            &partial,
                            &sync_directory,
                            operation_deadline,
                        )
                        .map_err(|reason| {
                            BrokerOpError::RecoveryArchiveUnverified {
                                id: session_id,
                                path: partial.to_string_lossy().into_owned(),
                                reason,
                            }
                        })?;
                    }
                    std::fs::rename(&partial, &archive)
                        .map_err(|error| io_error(&archive, error))?;
                }
                Err(error) => return Err(io_error(&partial_path, error)),
            }
        }
        let archive_parent =
            archive_root
                .parent()
                .ok_or_else(|| BrokerOpError::RecoveryArchiveUnverified {
                    id: session_id,
                    path: archive_root.to_string_lossy().into_owned(),
                    reason: "archive root has no parent to sync".into(),
                })?;
        let archive_anchor =
            archive_parent
                .parent()
                .ok_or_else(|| BrokerOpError::RecoveryArchiveUnverified {
                    id: session_id,
                    path: archive_parent.to_string_lossy().into_owned(),
                    reason: "archive root has no storage anchor to sync".into(),
                })?;
        for (label, path) in [
            ("archive root", archive_root.as_path()),
            ("archive parent", archive_parent),
            ("archive storage anchor", archive_anchor),
        ] {
            sync_directory(path).map_err(|error| BrokerOpError::RecoveryArchiveUnverified {
                id: session_id,
                path: path.to_string_lossy().into_owned(),
                reason: format!("cannot sync {label} before worktree removal: {error}"),
            })?;
        }
        if operation_deadline.is_some_and(|deadline| {
            deadline.saturating_duration_since(Instant::now()) < AUTO_COMPACTION_STAGING_RESERVE
        }) {
            return Err(BrokerOpError::CleanupResolveRefused {
                id: session_id,
                reason: "automatic retirement finished archive verification without the reserved time for cleanup; the verified archive is retained for a later pass".into(),
            });
        }
        after_archive_verified();

        // The archive is complete and verified. Cleanup recomputes the full
        // plan under the lifecycle claim immediately before removing the tree.
        let cleanup_result = if retirement_mode {
            self.cleanup_after_recovery_archive_for_retirement(
                session_id,
                &plan,
                &archive,
                allow_adopted,
                expected_cohort,
            )
        } else {
            self.cleanup_after_recovery_archive(session_id, &plan, &archive)
        };
        let cleanup_error = cleanup_result.err().map(|error| error.to_string());
        let worktree_removed = cleanup_error.is_none();
        let payload = serde_json::json!({
            "session_id": session_id,
            "archive": archive.to_string_lossy(),
            "head": plan.head,
            "disposition": plan.disposition.as_str(),
            "unlanded_commits": plan.unlanded_commits.len(),
            "untracked_files": plan.untracked_files.len(),
            "ignored_files": plan.ignored_files.len(),
            "worktree_removed": worktree_removed,
        })
        .to_string();
        self.store().append_event(
            crate::events::BROKER_CLEANUP_ARCHIVED,
            Some(session_id),
            Some(&payload),
        )?;
        Ok(CleanupResolveOutcome {
            plan,
            archive,
            worktree_removed,
            cleanup_error,
        })
    }
}

/// The complete manifest shape the broker has written. Unknown fields are
/// refused so a newer archive cannot be treated as empty by an older reader.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredManifest {
    schema_version: u32,
    created_at_ms: i64,
    #[serde(default)]
    minimum_archive_retention_ms: Option<u64>,
    #[serde(default)]
    retirement_record_sha256: Option<String>,
    aethyme_version: String,
    repository: String,
    session_id: i64,
    task: Option<String>,
    worktree_path: String,
    branch_ref: String,
    #[serde(default)]
    branch_tip: Option<String>,
    #[serde(default)]
    checkout_branch_ref: Option<String>,
    #[serde(default)]
    checkout_branch_tip: Option<String>,
    #[serde(default)]
    recovery_root_proofs_sha256: Option<String>,
    #[serde(default)]
    recovery_root_proofs: Option<Vec<RecoveryRootProof>>,
    #[serde(default)]
    delivery_targets_sha256: Option<String>,
    #[serde(default)]
    prerequisite_refs: Option<Vec<RecoveryArchivePrerequisiteRef>>,
    disposition: String,
    head: String,
    base: Option<String>,
    delivery_targets: Vec<String>,
    landing: serde_json::Value,
    bundle: Option<String>,
    #[serde(default)]
    bundle_bytes: Option<u64>,
    #[serde(default)]
    bundle_sha256: Option<String>,
    unlanded_commits: Vec<String>,
    staged_patch: StoredPatch,
    unstaged_patch: StoredPatch,
    untracked_directory: String,
    untracked_files: Vec<ArchivedFile>,
    /// Absent in archive schemas v1 through v3. `None` in schemas v2 through
    /// v4 is incomplete and therefore cannot establish that ignored data is absent.
    #[serde(default)]
    ignored_directory: Option<String>,
    #[serde(default)]
    ignored_paths: Option<Vec<IgnoredPath>>,
    #[serde(default)]
    ignored_files: Option<Vec<ArchivedFile>>,
    #[serde(default)]
    ignored_policy_sha256: Option<String>,
    #[serde(default)]
    plan_digest: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPatch {
    file: String,
    bytes: u64,
    sha256: String,
}

/// An archive the broker can vouch for: this repository wrote it, for a
/// session this repository records.
#[derive(Debug)]
pub(crate) struct OwnedRecoveryArchive {
    pub path: PathBuf,
    pub session_id: i64,
    pub head: String,
    /// Every exact recovery root that must reach a current delivery target before GC can call the archive redundant.
    pub recovery_roots: Vec<RecoveryRootProof>,
    pub created_at_ms: i64,
    /// Immutable minimum archive age copied from the reviewed retirement plan.
    pub minimum_archive_retention_ms: Option<u64>,
    /// Exact repository-local refs retaining incremental-bundle prerequisites.
    /// These may only be removed after the complete archive tree is removed.
    pub prerequisite_refs: Vec<RecoveryArchivePrerequisiteRef>,
    /// Captured v6 archive identity used to validate the exact ref namespace.
    pub retirement_digest: Option<String>,
    pub prerequisite_targets: Vec<String>,
    /// Staged, unstaged, untracked or protected ignored changes -- work no
    /// commit carries, so no delivery target can ever hold it.
    pub holds_uncommitted: bool,
}

/// This repository's archive directory and what is in it.
#[derive(Debug)]
pub(crate) struct RecoveryArchiveScan {
    pub root: PathBuf,
    pub owned: Vec<OwnedRecoveryArchive>,
    /// Everything else, with why the broker cannot vouch for it.
    pub unowned: Vec<(PathBuf, String)>,
}

fn same_path(left: &Path, right: &Path) -> bool {
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical(left) == canonical(right)
}

const MAX_RECOVERY_ARCHIVE_TREE_ENTRIES: usize = 250_000;
const MAX_RECOVERY_ARCHIVE_TREE_DEPTH: usize = 128;
const MAX_RECOVERY_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;

/// Check the known archive payload plus the configured reserve requirement
/// against the destination volume. When needed, the bundle's enforced maximum
/// is included; filesystem allocation overhead and concurrent writes are not,
/// so this known-minimum admission check does not guarantee the reserve remains.
fn require_recovery_archive_headroom(
    available_bytes: Option<u64>,
    known_payload_bytes: u64,
) -> Result<(), String> {
    let required = known_payload_bytes
        .checked_add(crate::DEFAULT_GATE_HEADROOM_BYTES)
        .ok_or_else(|| "known archive size plus the storage reserve overflows u64".to_owned())?;
    let available_bytes = available_bytes.ok_or_else(|| {
        "cannot measure free space on the archive destination volume; the worktree was left in place"
            .to_owned()
    })?;
    if available_bytes < required {
        return Err(format!(
            "archive destination has {available_bytes} free bytes, below the {required} bytes required for the known payload plus the {}-byte reserve. The bundle maximum is included when needed; filesystem allocation overhead and concurrent writes are not, so this known-minimum check does not guarantee the reserve will remain available; the worktree was left in place",
            crate::DEFAULT_GATE_HEADROOM_BYTES
        ));
    }
    Ok(())
}

fn add_archive_size(total: &mut u64, bytes: u64, label: &str) -> Result<(), String> {
    *total = total.checked_add(bytes).ok_or_else(|| {
        format!("known recovery archive size overflows u64 while counting {label}")
    })?;
    Ok(())
}

fn known_recovery_archive_payload_bytes(plan: &CleanupResolvePlan) -> Result<u64, String> {
    let mut total = 0_u64;
    add_archive_size(&mut total, plan.staged_patch_bytes, STAGED_PATCH_FILE)?;
    add_archive_size(&mut total, plan.unstaged_patch_bytes, UNSTAGED_PATCH_FILE)?;
    if recovery_bundle_required(plan) {
        add_archive_size(&mut total, MAX_RECOVERY_BUNDLE_BYTES, BUNDLE_FILE)?;
    }

    let worktree = Path::new(&plan.worktree_path);
    for file in plan.untracked_files.iter().chain(&plan.ignored_files) {
        let bytes = if file.symlink {
            let path = worktree.join(&file.path);
            let target = std::fs::read_link(&path)
                .map_err(|error| format!("cannot measure archived link {}: {error}", file.path))?;
            u64::try_from(target.as_os_str().as_encoded_bytes().len())
                .map_err(|_| format!("archived link target is too large: {}", file.path))?
        } else {
            file.bytes
        };
        add_archive_size(&mut total, bytes, &file.path)?;
    }

    // The manifest reader's limit is the conservative upper bound reserved
    // for its encoded contents. The generated README has a smaller fixed cap.
    add_archive_size(&mut total, MAX_RECOVERY_MANIFEST_BYTES, MANIFEST_FILE)?;
    add_archive_size(&mut total, MAX_RECOVERY_README_BYTES, README_FILE)?;
    Ok(total)
}

fn retirement_content_identity_sha256(plan: &CleanupResolvePlan) -> Result<String, String> {
    let minimum_archive_retention_ms = plan
        .minimum_archive_retention_ms
        .ok_or_else(|| "retirement plan has no immutable archive retention floor".to_owned())?;
    let allow_adopted = plan
        .retirement_allow_adopted
        .ok_or_else(|| "retirement plan has no explicit adopted-origin mode".to_owned())?;
    let authorization_ignored_paths = authorization_ignored_paths(&plan.ignored_paths);
    let mut recovery_root_identities = plan
        .recovery_root_proofs
        .iter()
        .map(|proof| (proof.ref_name.as_str(), proof.object_id.as_str()))
        .collect::<Vec<_>>();
    recovery_root_identities.sort_unstable();
    recovery_root_identities.dedup();
    let input = RetirementContentIdentityInputV1 {
        schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
        recovery_archive_schema_version: plan.recovery_archive_schema_version,
        session_id: plan.session_id,
        repository_path: &plan.repository_path,
        worktree_path: &plan.worktree_path,
        target_origin: plan.target_origin.as_str(),
        allow_adopted,
        minimum_archive_retention_ms,
        owner_witness_sha256: plan.retirement_witness_sha256.as_deref(),
        branch_ref: &plan.branch_ref,
        branch_tip: plan.branch_tip.as_deref(),
        checkout_branch_ref: plan.checkout_branch_ref.as_deref(),
        checkout_branch_tip: plan.checkout_branch_tip.as_deref(),
        head: &plan.head,
        staged_patch_bytes: plan.staged_patch_bytes,
        staged_patch_sha256: &plan.staged_patch_sha256,
        unstaged_patch_bytes: plan.unstaged_patch_bytes,
        unstaged_patch_sha256: &plan.unstaged_patch_sha256,
        untracked_files: &plan.untracked_files,
        untracked_bytes: plan.untracked_bytes,
        ignored_paths: &authorization_ignored_paths,
        ignored_files: &plan.ignored_files,
        ignored_bytes: plan.ignored_bytes,
        ignored_policy_sha256: &plan.ignored_policy_sha256,
        recovery_root_identities,
    };
    let bytes = serde_json::to_vec(&input)
        .map_err(|error| format!("cannot encode retirement content identity: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(b"aethyme-cleanup-retirement-content-v1\0");
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn retirement_archive_record_for_plan(
    plan: &CleanupResolvePlan,
    archive_created_at_ms: i64,
) -> Result<RetirementArchiveRecord, String> {
    let minimum_archive_retention_ms = plan
        .minimum_archive_retention_ms
        .ok_or_else(|| "retirement plan has no immutable archive retention floor".to_owned())?;
    let allow_adopted = plan
        .retirement_allow_adopted
        .ok_or_else(|| "retirement plan has no explicit adopted-origin mode".to_owned())?;
    let is_pinned_retirement =
        plan.recovery_archive_schema_version == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION;
    let record = RetirementArchiveRecord {
        schema_version: if is_pinned_retirement {
            PINNED_RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION
        } else {
            RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION
        },
        session_id: plan.session_id,
        plan_digest: plan.digest.clone(),
        target_origin: plan.target_origin.as_str().to_owned(),
        allow_adopted,
        minimum_archive_retention_ms,
        archive_created_at_ms,
        content_identity_sha256: retirement_content_identity_sha256(plan)?,
        owner_witness_sha256: plan.retirement_witness_sha256.clone(),
        delivery_targets_sha256: delivery_targets_sha256(&plan.delivery_targets)?,
        worktree_path: plan.worktree_path.clone(),
        branch_ref: plan.branch_ref.clone(),
        branch_tip: plan.branch_tip.clone(),
        checkout_branch_ref: plan.checkout_branch_ref.clone(),
        checkout_branch_tip: plan.checkout_branch_tip.clone(),
        recovery_root_proofs_sha256: recovery_root_proofs_sha256(&plan.recovery_root_proofs)?,
        head: plan.head.clone(),
        delivery_targets: is_pinned_retirement.then(|| plan.delivery_targets.clone()),
        prerequisite_refs: is_pinned_retirement
            .then(|| {
                recovery_archive_prerequisite_refs(
                    plan.session_id,
                    &plan.digest,
                    &plan.delivery_targets,
                )
            })
            .transpose()?,
    };
    validate_retirement_archive_record_fields(&record)?;
    Ok(record)
}

fn retirement_archive_record_bytes(
    plan: &CleanupResolvePlan,
    archive_created_at_ms: i64,
) -> Result<Vec<u8>, String> {
    let record = retirement_archive_record_for_plan(plan, archive_created_at_ms)?;
    serde_json::to_vec(&record).map_err(|error| format!("cannot encode retirement record: {error}"))
}

fn retirement_archive_record_sha256(
    root: &Path,
    plan: &CleanupResolvePlan,
) -> Result<String, String> {
    verify_retirement_archive_record_for_plan(root, plan)?;
    let (_, bytes) = read_retirement_archive_record(root)?;
    Ok(sha256_hex(&bytes))
}

fn create_retirement_archive_record(root: &Path, plan: &CleanupResolvePlan) -> Result<(), String> {
    let bytes = retirement_archive_record_bytes(plan, now_ms())?;
    create_new_archive_file(root, RETIREMENT_ARCHIVE_RECORD_FILE, &bytes).map(|_| ())
}

fn validate_retirement_archive_record_fields(
    record: &RetirementArchiveRecord,
) -> Result<(), String> {
    if !matches!(
        record.schema_version,
        RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION | PINNED_RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION
    ) || record.session_id <= 0
        || !valid_sha256(&record.plan_digest)
        || !matches!(record.target_origin.as_str(), "spawned" | "adopted")
        || (record.target_origin == "adopted" && !record.allow_adopted)
        || record.minimum_archive_retention_ms < MIN_RECOVERY_ARCHIVE_RETENTION_MS
        || record.archive_created_at_ms < 0
        || !valid_sha256(&record.content_identity_sha256)
        || record
            .owner_witness_sha256
            .as_deref()
            .is_some_and(|value| !valid_sha256(value))
        || record.worktree_path.is_empty()
        || !Path::new(&record.worktree_path).is_absolute()
        || !record.branch_ref.starts_with("refs/heads/")
        || record
            .branch_tip
            .as_deref()
            .is_some_and(|tip| !valid_git_object_id(tip))
        || record
            .checkout_branch_ref
            .as_deref()
            .is_some_and(|branch| !branch.starts_with("refs/heads/") || branch == "HEAD")
        || record
            .checkout_branch_tip
            .as_deref()
            .is_some_and(|tip| !valid_git_object_id(tip) || tip != record.head)
        || record.checkout_branch_ref.is_some() != record.checkout_branch_tip.is_some()
        || !valid_sha256(&record.delivery_targets_sha256)
        || !valid_sha256(&record.recovery_root_proofs_sha256)
        || !valid_git_object_id(&record.head)
        || (record.schema_version == RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION
            && (record.delivery_targets.is_some() || record.prerequisite_refs.is_some()))
        || (record.schema_version == PINNED_RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION
            && validate_retirement_record_prerequisites(record).is_err())
    {
        return Err("retirement record is incomplete or malformed".into());
    }
    Ok(())
}

fn validate_retirement_record_prerequisites(
    record: &RetirementArchiveRecord,
) -> Result<(), String> {
    let targets = record.delivery_targets.as_deref().ok_or_else(|| {
        "schema v6 retirement record is missing captured delivery targets".to_owned()
    })?;
    if !targets.iter().all(|target| valid_git_object_id(target))
        || delivery_targets_sha256(targets)? != record.delivery_targets_sha256
    {
        return Err("schema v6 retirement record delivery targets are malformed".into());
    }
    let expected =
        recovery_archive_prerequisite_refs(record.session_id, &record.plan_digest, targets)?;
    if record.prerequisite_refs.as_deref() != Some(expected.as_slice()) {
        return Err(
            "schema v6 retirement record prerequisite refs do not match its delivery snapshot"
                .into(),
        );
    }
    Ok(())
}

fn read_retirement_archive_record(
    root: &Path,
) -> Result<(RetirementArchiveRecord, Vec<u8>), String> {
    let path = root.join(RETIREMENT_ARCHIVE_RECORD_FILE);
    let bytes = read_regular_file_no_follow(&path, MAX_RETIREMENT_ARCHIVE_RECORD_BYTES)
        .map_err(|error| format!("cannot safely read retirement record: {error}"))?;
    let record: RetirementArchiveRecord = serde_json::from_slice(&bytes)
        .map_err(|error| format!("unreadable retirement record: {error}"))?;
    validate_retirement_archive_record_fields(&record)?;
    Ok((record, bytes))
}

fn verify_retirement_archive_record_for_plan(
    root: &Path,
    plan: &CleanupResolvePlan,
) -> Result<(), String> {
    let (actual, _) = read_retirement_archive_record(root)?;
    let expected = retirement_archive_record_for_plan(plan, actual.archive_created_at_ms)?;
    if actual != expected {
        return Err("retirement record does not match the reviewed plan".into());
    }
    Ok(())
}

fn verify_retirement_archive_record_for_content_identity(
    root: &Path,
    plan: &CleanupResolvePlan,
) -> Result<(), String> {
    let (actual, _) = read_retirement_archive_record(root)?;
    let mut expected = retirement_archive_record_for_plan(plan, actual.archive_created_at_ms)?;
    let mut actual = actual;
    // A target may advance while a verified incomplete staging directory is
    // waiting for its next attempt. Its delivery envelope must be rebuilt, but
    // the archive directory is ours to restart only if its source identity is
    // unchanged. The refreshed delivery vectors remain covered by the normal
    // exact-plan confirmation below before any source mutation.
    actual.plan_digest.clear();
    expected.plan_digest.clear();
    actual.delivery_targets_sha256.clear();
    expected.delivery_targets_sha256.clear();
    actual.recovery_root_proofs_sha256.clear();
    expected.recovery_root_proofs_sha256.clear();
    if plan.recovery_archive_schema_version == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION {
        actual.delivery_targets = None;
        expected.delivery_targets = None;
        actual.prerequisite_refs = None;
        expected.prerequisite_refs = None;
    }
    if actual != expected {
        return Err(
            "retirement staging record does not match the current source content identity".into(),
        );
    }
    Ok(())
}

fn verify_retirement_archive_record_for_manifest(
    root: &Path,
    manifest: &StoredManifest,
) -> Result<(), String> {
    let (record, bytes) = read_retirement_archive_record(root)?;
    let record_sha256 = sha256_hex(&bytes);
    if Some(record_sha256.as_str()) != manifest.retirement_record_sha256.as_deref()
        || Some(record.session_id) != Some(manifest.session_id)
        || Some(record.plan_digest.as_str()) != manifest.plan_digest.as_deref()
        || Some(record.minimum_archive_retention_ms) != manifest.minimum_archive_retention_ms
        || record.archive_created_at_ms != manifest.created_at_ms
        || record.worktree_path != manifest.worktree_path
        || record.branch_ref != manifest.branch_ref
        || record.branch_tip != manifest.branch_tip
        || record.checkout_branch_ref != manifest.checkout_branch_ref
        || record.checkout_branch_tip != manifest.checkout_branch_tip
        || Some(record.recovery_root_proofs_sha256.as_str())
            != manifest.recovery_root_proofs_sha256.as_deref()
        || Some(record.delivery_targets_sha256.as_str())
            != manifest.delivery_targets_sha256.as_deref()
        || record.head != manifest.head
        || record.prerequisite_refs.as_deref() != manifest.prerequisite_refs.as_deref()
        || record
            .delivery_targets
            .as_deref()
            .is_some_and(|targets| targets != manifest.delivery_targets)
        || (manifest.schema_version == RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
            && record.schema_version != RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION)
        || (manifest.schema_version == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
            && record.schema_version != PINNED_RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION)
    {
        return Err("retirement record does not match its archive manifest".into());
    }
    Ok(())
}

fn recovery_bundle_required(plan: &CleanupResolvePlan) -> bool {
    !recovery_bundle_roots(plan).is_empty()
}

fn recovery_root_identities_for_plan_parts(
    head: &str,
    branch_ref: &str,
    branch_tip: Option<&str>,
    checkout_branch_ref: Option<&str>,
    checkout_branch_tip: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let mut roots = std::collections::BTreeMap::from([("HEAD".to_owned(), head.to_owned())]);
    if let Some(tip) = branch_tip {
        match roots.insert(branch_ref.to_owned(), tip.to_owned()) {
            Some(previous) if previous != tip => {
                return Err("one branch ref resolved to two different archive roots".into());
            }
            _ => {}
        }
    }
    if let (Some(reference), Some(tip)) = (checkout_branch_ref, checkout_branch_tip) {
        match roots.insert(reference.to_owned(), tip.to_owned()) {
            Some(previous) if previous != tip => {
                return Err("one branch ref resolved to two different archive roots".into());
            }
            _ => {}
        }
    }
    Ok(roots.into_iter().collect())
}

/// Prefer an exact captured commit before walking unrelated delivery histories.
/// This only orders witnesses: callers still ask Git to validate ancestry, even
/// for identical strings, so malformed or missing objects remain refusals.
pub(crate) fn delivery_targets_prefer_exact<'a>(
    object_id: &'a str,
    delivery_targets: &'a [String],
) -> impl Iterator<Item = &'a String> {
    delivery_targets
        .iter()
        .filter(move |target| target.as_str() == object_id)
        .chain(
            delivery_targets
                .iter()
                .filter(move |target| target.as_str() != object_id),
        )
}

/// An exact bundled root needs no additional current-delivery ancestry proof.
/// The caller still verifies the archive payload and advertised bundle roots.
fn recovery_root_is_available(
    reference: &str,
    object_id: &str,
    bundled_roots: &std::collections::BTreeSet<(String, String)>,
    check_delivery: impl FnOnce() -> Result<bool, String>,
) -> Result<bool, String> {
    if bundled_roots.contains(&(reference.to_owned(), object_id.to_owned())) {
        Ok(true)
    } else {
        check_delivery()
    }
}

fn root_available_from_delivery(
    repository: &GitRepo,
    object_id: &str,
    delivery_targets: &[String],
) -> Result<bool, String> {
    for target in delivery_targets_prefer_exact(object_id, delivery_targets) {
        match repository.is_ancestor_checked(object_id, target) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(error) => {
                return Err(format!(
                    "cannot check recovery root {object_id} against delivery target {target}: {error}"
                ));
            }
        }
    }
    Ok(false)
}

fn recovery_root_proofs_for_plan_parts(
    repository: &GitRepo,
    head: &str,
    branch_ref: &str,
    branch_tip: Option<&str>,
    checkout_branch: (Option<&str>, Option<&str>),
    delivery_targets: &[String],
    ancestry_memo: &PlanAncestryMemo,
) -> Result<Vec<RecoveryRootProof>, String> {
    let (checkout_branch_ref, checkout_branch_tip) = checkout_branch;
    recovery_root_identities_for_plan_parts(
        head,
        branch_ref,
        branch_tip,
        checkout_branch_ref,
        checkout_branch_tip,
    )?
    .into_iter()
    .map(|(ref_name, object_id)| {
        Ok(RecoveryRootProof {
            available_from_delivery: root_available_from_delivery_during_plan(
                repository,
                &object_id,
                delivery_targets,
                ancestry_memo,
            )?,
            ref_name,
            object_id,
        })
    })
    .collect()
}

fn root_available_from_delivery_during_plan(
    repository: &GitRepo,
    object_id: &str,
    delivery_targets: &[String],
    ancestry_memo: &PlanAncestryMemo,
) -> Result<bool, String> {
    for target in delivery_targets_prefer_exact(object_id, delivery_targets) {
        match ancestry_memo.check(repository, object_id, target) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(error) => {
                return Err(format!(
                    "cannot check recovery root {object_id} against delivery target {target}: {error}"
                ));
            }
        }
    }
    Ok(false)
}

fn validate_root_proof_identities(
    proofs: &[RecoveryRootProof],
    identities: &[(String, String)],
) -> Result<(), String> {
    if proofs.len() != identities.len()
        || proofs
            .iter()
            .zip(identities)
            .any(|(proof, (reference, object_id))| {
                proof.ref_name != reference.as_str()
                    || proof.object_id != object_id.as_str()
                    || !valid_git_object_id(&proof.object_id)
            })
    {
        return Err("schema v5 recovery root proof does not match its branch identities".into());
    }
    Ok(())
}

fn validate_root_proof_delivery_availability(
    repository: &GitRepo,
    delivery_targets: &[String],
    proofs: &[RecoveryRootProof],
) -> Result<(), String> {
    for proof in proofs {
        let available =
            root_available_from_delivery(repository, &proof.object_id, delivery_targets)?;
        if available != proof.available_from_delivery {
            return Err(format!(
                "schema v5 recovery root {} has an incorrect delivery-availability proof",
                proof.ref_name
            ));
        }
    }
    Ok(())
}

fn validate_available_root_proofs_from_delivery(
    repository: &GitRepo,
    delivery_targets: &[String],
    proofs: &[RecoveryRootProof],
) -> Result<(), String> {
    for proof in proofs.iter().filter(|proof| proof.available_from_delivery) {
        if !root_available_from_delivery(repository, &proof.object_id, delivery_targets)? {
            return Err(format!(
                "schema v5 recovery root {} is marked available but is not held by its recorded delivery targets",
                proof.ref_name
            ));
        }
    }
    Ok(())
}

fn validate_delivery_prerequisites_remain_current(
    repository: &GitRepo,
    captured_targets: &[String],
    current_targets: &[String],
) -> Result<(), String> {
    for prerequisite in captured_targets {
        let mut retained = false;
        let mut error_detail = None;
        for target in delivery_targets_prefer_exact(prerequisite, current_targets) {
            match repository.is_ancestor_checked(prerequisite, target) {
                Ok(true) => {
                    retained = true;
                    break;
                }
                Ok(false) => {}
                Err(error) => error_detail = Some(error.to_string()),
            }
        }
        if !retained {
            return Err(match error_detail {
                Some(error) => format!(
                    "captured archive prerequisite {prerequisite} could not be verified against current delivery targets: {error}"
                ),
                None => format!(
                    "captured archive prerequisite {prerequisite} is no longer retained by current delivery history"
                ),
            });
        }
    }
    Ok(())
}

fn recovery_root_proofs_sha256(proofs: &[RecoveryRootProof]) -> Result<String, String> {
    let bytes = serde_json::to_vec(proofs)
        .map_err(|error| format!("cannot encode recovery root proof: {error}"))?;
    Ok(sha256_hex(&bytes))
}

fn delivery_targets_sha256(targets: &[String]) -> Result<String, String> {
    let bytes = serde_json::to_vec(targets)
        .map_err(|error| format!("cannot encode delivery roots: {error}"))?;
    Ok(sha256_hex(&bytes))
}

fn is_retirement_archive_schema(schema_version: u32) -> bool {
    matches!(
        schema_version,
        RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
            | PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
    )
}

fn pinned_retirement_archive_name(
    session_id: i64,
    head: &str,
    retirement_digest: &str,
) -> Result<String, String> {
    if session_id <= 0 || !valid_git_object_id(head) || !valid_sha256(retirement_digest) {
        return Err("cannot name a schema v6 archive from an invalid identity".into());
    }
    Ok(format!(
        "{session_id}-{}-retirement-v6-{retirement_digest}",
        &head[..12]
    ))
}

fn recovery_archive_prerequisite_refs(
    session_id: i64,
    retirement_digest: &str,
    delivery_targets: &[String],
) -> Result<Vec<RecoveryArchivePrerequisiteRef>, String> {
    if session_id <= 0 || !valid_sha256(retirement_digest) {
        return Err(
            "cannot name archive prerequisite refs from an invalid retirement identity".into(),
        );
    }
    let target_digest = delivery_targets_sha256(delivery_targets)?;
    let object_ids = delivery_targets
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if object_ids.is_empty() {
        return Err("retirement archive has no delivery prerequisites to pin".into());
    }
    if object_ids.len() > MAX_RECOVERY_ARCHIVE_PREREQUISITE_REFS {
        return Err(format!(
            "delivery snapshot has {} distinct prerequisites; the supported limit is {}",
            object_ids.len(),
            MAX_RECOVERY_ARCHIVE_PREREQUISITE_REFS
        ));
    }
    if object_ids
        .iter()
        .any(|object_id| !valid_git_object_id(object_id))
    {
        return Err("delivery snapshot contains an invalid prerequisite object id".into());
    }
    Ok(object_ids
        .into_iter()
        .enumerate()
        .map(|(index, object_id)| RecoveryArchivePrerequisiteRef {
            ref_name: format!(
                "refs/aethyme/recovery-prerequisites/v1/{session_id}/{retirement_digest}/{target_digest}/{index}"
            ),
            object_id: object_id.to_owned(),
        })
        .collect())
}

fn recovery_bundle_roots(plan: &CleanupResolvePlan) -> Vec<(String, String)> {
    if is_retirement_archive_schema(plan.recovery_archive_schema_version) {
        return plan
            .recovery_root_proofs
            .iter()
            .filter(|proof| !proof.available_from_delivery)
            .map(|proof| (proof.ref_name.clone(), proof.object_id.clone()))
            .collect();
    }
    if plan.unlanded_commits.is_empty() {
        Vec::new()
    } else {
        vec![("HEAD".to_owned(), plan.head.clone())]
    }
}

fn recovery_bundle_refs(plan: &CleanupResolvePlan) -> Vec<String> {
    recovery_bundle_roots(plan)
        .into_iter()
        .filter_map(|(name, _)| (name != "HEAD").then_some(name))
        .collect()
}

fn recovery_bundle_roots_from_manifest(
    manifest: &StoredManifest,
) -> Result<Vec<(String, String)>, String> {
    if is_retirement_archive_schema(manifest.schema_version) {
        let proofs = manifest
            .recovery_root_proofs
            .as_ref()
            .ok_or_else(|| "retirement archive is missing its recovery root proofs".to_owned())?;
        let digest = recovery_root_proofs_sha256(proofs)?;
        if Some(digest.as_str()) != manifest.recovery_root_proofs_sha256.as_deref() {
            return Err(
                "recovery roots or their delivery availability changed in the manifest".into(),
            );
        }
        return Ok(proofs
            .iter()
            .filter(|proof| !proof.available_from_delivery)
            .map(|proof| (proof.ref_name.clone(), proof.object_id.clone()))
            .collect());
    }
    if manifest.unlanded_commits.is_empty() {
        Ok(Vec::new())
    } else {
        Ok(vec![("HEAD".to_owned(), manifest.head.clone())])
    }
}

fn checked_archived_file_bytes(files: &[ArchivedFile]) -> Option<u64> {
    files
        .iter()
        .try_fold(0_u64, |total, file| total.checked_add(file.bytes))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn read_recovery_manifest(path: &Path) -> Result<Vec<u8>, String> {
    let (file, metadata) = open_regular_file_no_follow(path)
        .map_err(|error| format!("cannot safely open manifest: {error}"))?;
    if metadata.len() > MAX_RECOVERY_MANIFEST_BYTES {
        return Err("manifest exceeds the ownership check byte bound".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_RECOVERY_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read manifest: {error}"))?;
    if bytes.len() as u64 > MAX_RECOVERY_MANIFEST_BYTES {
        return Err("manifest grew beyond the ownership check byte bound".into());
    }
    Ok(bytes)
}

fn validate_manifest_identity(manifest: &StoredManifest) -> Result<(), String> {
    match manifest.schema_version {
        LEGACY_RECOVERY_ARCHIVE_SCHEMA_VERSION | PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            if manifest.bundle_bytes.is_some() || manifest.bundle_sha256.is_some() {
                return Err("legacy manifest unexpectedly contains bundle integrity fields".into());
            }
        }
        PREVIOUS_BUNDLE_RECOVERY_ARCHIVE_SCHEMA_VERSION
        | RECOVERY_ARCHIVE_SCHEMA_VERSION
        | RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        | PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION => match manifest.bundle.as_deref() {
            Some(_) => {
                let Some(bytes) = manifest.bundle_bytes else {
                    return Err("bundle archive is missing its byte length".into());
                };
                if bytes == 0 || bytes > MAX_RECOVERY_BUNDLE_BYTES {
                    return Err("bundle archive length is outside its supported bound".into());
                }
                if !manifest.bundle_sha256.as_deref().is_some_and(valid_sha256) {
                    return Err("bundle archive is missing a valid SHA-256".into());
                }
            }
            None => {
                if manifest.bundle_bytes.is_some() || manifest.bundle_sha256.is_some() {
                    return Err("bundle archive has integrity fields without a bundle".into());
                }
            }
        },
        version => return Err(format!("manifest schema {version} is not supported")),
    }

    if is_retirement_archive_schema(manifest.schema_version) {
        if manifest
            .minimum_archive_retention_ms
            .is_none_or(|floor| floor < MIN_RECOVERY_ARCHIVE_RETENTION_MS)
        {
            return Err(
                "retirement manifest is missing a valid minimum archive retention floor".into(),
            );
        }
        if !manifest
            .retirement_record_sha256
            .as_deref()
            .is_some_and(valid_sha256)
            || !manifest
                .recovery_root_proofs_sha256
                .as_deref()
                .is_some_and(valid_sha256)
            || manifest.recovery_root_proofs.is_none()
            || !manifest
                .delivery_targets_sha256
                .as_deref()
                .is_some_and(valid_sha256)
        {
            return Err(
                "retirement manifest is missing its retirement or root-proof identity".into(),
            );
        }
        if manifest
            .checkout_branch_ref
            .as_deref()
            .is_some_and(|branch| !branch.starts_with("refs/heads/"))
            || manifest
                .checkout_branch_tip
                .as_deref()
                .is_some_and(|tip| !valid_git_object_id(tip) || tip != manifest.head)
            || manifest.checkout_branch_ref.is_some() != manifest.checkout_branch_tip.is_some()
        {
            return Err("retirement manifest has an invalid checked-out branch identity".into());
        }
        let delivery_hash = delivery_targets_sha256(&manifest.delivery_targets)?;
        if Some(delivery_hash.as_str()) != manifest.delivery_targets_sha256.as_deref() {
            return Err(
                "retirement delivery target roots do not match their recorded identity".into(),
            );
        }
        let identities = recovery_root_identities_for_plan_parts(
            &manifest.head,
            &manifest.branch_ref,
            manifest.branch_tip.as_deref(),
            manifest.checkout_branch_ref.as_deref(),
            manifest.checkout_branch_tip.as_deref(),
        )?;
        let proofs = manifest
            .recovery_root_proofs
            .as_deref()
            .ok_or_else(|| "retirement archive is missing its recovery root proofs".to_owned())?;
        validate_root_proof_identities(proofs, &identities)?;
        let root_proofs_hash = recovery_root_proofs_sha256(proofs)?;
        if Some(root_proofs_hash.as_str()) != manifest.recovery_root_proofs_sha256.as_deref() {
            return Err(
                "retirement manifest root proof does not match its branch identities".into(),
            );
        }
        if manifest.schema_version == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION {
            let plan_digest = manifest
                .plan_digest
                .as_deref()
                .ok_or_else(|| "schema v6 manifest is missing its plan digest".to_owned())?;
            let expected = recovery_archive_prerequisite_refs(
                manifest.session_id,
                plan_digest,
                &manifest.delivery_targets,
            )?;
            if manifest.prerequisite_refs.as_deref() != Some(expected.as_slice()) {
                return Err("schema v6 manifest prerequisite refs do not match its captured delivery snapshot".into());
            }
        } else if manifest.prerequisite_refs.is_some() {
            return Err("schema v5 manifest unexpectedly contains prerequisite refs".into());
        }
    } else if manifest.minimum_archive_retention_ms.is_some()
        || manifest.retirement_record_sha256.is_some()
        || manifest.checkout_branch_ref.is_some()
        || manifest.checkout_branch_tip.is_some()
        || manifest.recovery_root_proofs_sha256.is_some()
        || manifest.recovery_root_proofs.is_some()
        || manifest.delivery_targets_sha256.is_some()
        || manifest.prerequisite_refs.is_some()
    {
        return Err("only retirement archives may declare retirement metadata".into());
    }

    if manifest.session_id <= 0
        || manifest.created_at_ms < 0
        || manifest.aethyme_version.is_empty()
        || manifest.worktree_path.is_empty()
        || !Path::new(&manifest.worktree_path).is_absolute()
        || manifest.branch_ref.is_empty()
        || manifest
            .branch_tip
            .as_deref()
            .is_some_and(|tip| !valid_git_object_id(tip))
        || !(matches!(
            manifest.disposition.as_str(),
            "dirty" | "pending_commits" | "unproven_provenance"
        ) || (matches!(
            manifest.schema_version,
            RECOVERY_ARCHIVE_SCHEMA_VERSION
                | RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                | PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        ) && manifest.disposition == "eligible"))
        || !valid_git_object_id(&manifest.head)
        || manifest
            .base
            .as_deref()
            .is_some_and(|base| !valid_git_object_id(base))
        || manifest
            .task
            .as_deref()
            .is_some_and(|task| task.contains('\0'))
        || manifest.landing.is_null()
        || (matches!(
            manifest.schema_version,
            RECOVERY_ARCHIVE_SCHEMA_VERSION
                | RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                | PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        ) && !matches!(
            manifest
                .landing
                .get("state")
                .and_then(serde_json::Value::as_str),
            Some("landed" | "not_landed" | "not_evaluated_for_recovery")
        ))
        || !manifest
            .delivery_targets
            .iter()
            .all(|target| valid_git_object_id(target))
        || !manifest
            .unlanded_commits
            .iter()
            .all(|commit| valid_git_object_id(commit))
        || (if is_retirement_archive_schema(manifest.schema_version) {
            manifest.bundle.is_some() == recovery_bundle_roots_from_manifest(manifest)?.is_empty()
        } else {
            manifest.bundle.is_some() == manifest.unlanded_commits.is_empty()
        })
    {
        return Err("manifest has an incomplete or malformed archive identity".into());
    }
    Ok(())
}

fn validate_archived_files(label: &str, files: &[ArchivedFile]) -> Result<(), String> {
    let mut previous: Option<&str> = None;
    for file in files {
        if !is_contained_relative(&file.path) {
            return Err(format!(
                "{label} path {:?} is not a contained relative path",
                file.path
            ));
        }
        if previous.is_some_and(|previous| previous >= file.path.as_str()) {
            return Err(format!("{label} paths are not unique and sorted"));
        }
        if previous.is_some_and(|previous| {
            file.path
                .strip_prefix(previous)
                .is_some_and(|suffix| suffix.starts_with('/'))
        }) {
            return Err(format!("{label} paths overlap as parent and child"));
        }
        if !valid_sha256(&file.sha256)
            || (file.symlink && (file.bytes != 0 || file.mode.is_some()))
            || file.mode.is_some_and(|mode| mode & !0o7777 != 0)
        {
            return Err(format!(
                "{label} entry {:?} has an invalid identity",
                file.path
            ));
        }
        previous = Some(&file.path);
    }
    Ok(())
}

fn validate_archived_file_modes(
    label: &str,
    files: &[ArchivedFile],
    schema_version: u32,
) -> Result<(), String> {
    match schema_version {
        LEGACY_RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            if files.iter().any(|file| file.mode.is_some()) {
                return Err(format!(
                    "legacy {label} entries unexpectedly contain file modes"
                ));
            }
        }
        PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION
        | PREVIOUS_BUNDLE_RECOVERY_ARCHIVE_SCHEMA_VERSION
        | RECOVERY_ARCHIVE_SCHEMA_VERSION
        | RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        | PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            #[cfg(unix)]
            if files
                .iter()
                .any(|file| !file.symlink && file.mode.is_none())
            {
                return Err(format!(
                    "schema v2/v3/v4/v5/v6 {label} entry is missing its Unix file mode"
                ));
            }
            #[cfg(not(unix))]
            if files.iter().any(|file| file.mode.is_some()) {
                return Err(format!(
                    "schema v2/v3/v4/v5/v6 {label} file modes cannot be verified on this platform"
                ));
            }
        }
        version => return Err(format!("manifest schema {version} is not supported")),
    }
    Ok(())
}

fn recovery_archive_expected_files(
    manifest: &StoredManifest,
) -> Result<std::collections::BTreeSet<PathBuf>, String> {
    if manifest.untracked_directory != UNTRACKED_DIRECTORY
        || manifest.staged_patch.file != STAGED_PATCH_FILE
        || manifest.unstaged_patch.file != UNSTAGED_PATCH_FILE
        || !valid_sha256(&manifest.staged_patch.sha256)
        || !valid_sha256(&manifest.unstaged_patch.sha256)
    {
        return Err("manifest names an unexpected archive component".into());
    }
    if manifest
        .bundle
        .as_deref()
        .is_some_and(|bundle| bundle != BUNDLE_FILE)
    {
        return Err("manifest names an unexpected commit bundle".into());
    }
    validate_archived_files("untracked", &manifest.untracked_files)?;

    let ignored_files = match manifest.schema_version {
        LEGACY_RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            validate_archived_file_modes(
                "untracked",
                &manifest.untracked_files,
                manifest.schema_version,
            )?;
            if manifest.ignored_directory.is_some()
                || manifest.ignored_paths.is_some()
                || manifest.ignored_files.is_some()
                || manifest.ignored_policy_sha256.is_some()
                || manifest.plan_digest.is_some()
            {
                return Err("legacy manifest unexpectedly contains ignored archive fields".into());
            }
            &[][..]
        }
        PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION
        | PREVIOUS_BUNDLE_RECOVERY_ARCHIVE_SCHEMA_VERSION
        | RECOVERY_ARCHIVE_SCHEMA_VERSION
        | RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        | PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            if manifest.ignored_directory.as_deref() != Some(IGNORED_DIRECTORY) {
                return Err(
                    "schema v2/v3/v4/v5 manifest is missing its ignored directory identity".into(),
                );
            }
            validate_archived_file_modes(
                "untracked",
                &manifest.untracked_files,
                manifest.schema_version,
            )?;
            let files = manifest.ignored_files.as_deref().ok_or_else(|| {
                "schema v2/v3/v4/v5 manifest is missing ignored_files".to_string()
            })?;
            validate_archived_files("ignored", files)?;
            validate_archived_file_modes("ignored", files, manifest.schema_version)?;
            let ignored_paths = manifest.ignored_paths.as_deref().ok_or_else(|| {
                "schema v2/v3/v4/v5 manifest is missing ignored_paths".to_string()
            })?;
            if !manifest
                .ignored_policy_sha256
                .as_deref()
                .is_some_and(valid_sha256)
                || !manifest.plan_digest.as_deref().is_some_and(valid_sha256)
            {
                return Err(
                    "schema v2/v3/v4/v5 manifest has an invalid plan or ignored policy identity"
                        .into(),
                );
            }
            let mut previous: Option<&str> = None;
            let mut preserved = Vec::new();
            for path in ignored_paths {
                let trimmed = path.path.trim_end_matches('/');
                if !is_contained_relative(trimmed)
                    || previous.is_some_and(|previous| previous >= path.path.as_str())
                {
                    return Err("schema v2/v3/v4/v5 ignored roots are invalid or unsorted".into());
                }
                if path.classification == IgnoredPathClassification::PreservedFile {
                    if path.path.ends_with('/') {
                        return Err(
                            "schema v2/v3/v4/v5 preserved ignored path is a directory".into()
                        );
                    }
                    preserved.push(path.path.as_str());
                }
                previous = Some(&path.path);
            }
            if preserved.len() != files.len()
                || preserved
                    .iter()
                    .zip(files)
                    .any(|(path, file)| *path != file.path)
            {
                return Err(
                    "schema v2/v3 ignored classifications do not match ignored_files".into(),
                );
            }
            files
        }
        version => return Err(format!("manifest schema {version} is not supported")),
    };

    let mut expected = std::collections::BTreeSet::from([
        PathBuf::from(MANIFEST_FILE),
        PathBuf::from(README_FILE),
        PathBuf::from(STAGED_PATCH_FILE),
        PathBuf::from(UNSTAGED_PATCH_FILE),
    ]);
    if manifest.bundle.is_some() {
        expected.insert(PathBuf::from(BUNDLE_FILE));
    }
    if is_retirement_archive_schema(manifest.schema_version) {
        expected.insert(PathBuf::from(RETIREMENT_ARCHIVE_RECORD_FILE));
    }
    expected.extend(
        manifest
            .untracked_files
            .iter()
            .map(|file| PathBuf::from(UNTRACKED_DIRECTORY).join(&file.path)),
    );
    expected.extend(
        ignored_files
            .iter()
            .map(|file| PathBuf::from(IGNORED_DIRECTORY).join(&file.path)),
    );
    Ok(expected)
}

/// Enumerate archive files without following links. Incomplete or unexpectedly
/// large trees stay unowned so retention never assumes their contents away.
fn recovery_archive_file_set_until(
    root: &Path,
    deadline: Option<Instant>,
) -> Result<std::collections::BTreeSet<PathBuf>, String> {
    let deadline = effective_archive_deadline(deadline);
    let mut files = std::collections::BTreeSet::new();
    let mut pending = vec![(PathBuf::new(), 0_usize)];
    let mut visited = 0_usize;
    while let Some((relative_dir, depth)) = pending.pop() {
        check_archive_deadline(deadline, "archive inventory")?;
        if depth > MAX_RECOVERY_ARCHIVE_TREE_DEPTH {
            return Err("archive directory depth exceeds the ownership check bound".into());
        }
        let directory = root.join(&relative_dir);
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        for entry in entries {
            check_archive_deadline(deadline, "archive durability scan")?;
            let entry = entry.map_err(|error| format!("cannot read archive entry: {error}"))?;
            visited += 1;
            if visited > MAX_RECOVERY_ARCHIVE_TREE_ENTRIES {
                return Err("archive entry count exceeds the ownership check bound".into());
            }
            let name = entry.file_name();
            let relative = relative_dir.join(name);
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|error| format!("cannot inspect {}: {error}", entry.path().display()))?;
            if metadata.is_dir() {
                pending.push((relative, depth + 1));
            } else if metadata.is_file() || metadata.file_type().is_symlink() {
                files.insert(relative);
            } else {
                return Err(format!(
                    "archive contains a special file: {}",
                    entry.path().display()
                ));
            }
        }
    }
    Ok(files)
}

fn verify_recovery_archive_tree(root: &Path, manifest: &StoredManifest) -> Result<(), String> {
    verify_recovery_archive_tree_until(root, manifest, None)
}

fn verify_recovery_archive_tree_until(
    root: &Path,
    manifest: &StoredManifest,
    deadline: Option<Instant>,
) -> Result<(), String> {
    let deadline = effective_archive_deadline(deadline);
    check_archive_deadline(deadline, "archive verification")?;
    validate_manifest_identity(manifest)?;
    let expected = recovery_archive_expected_files(manifest)?;
    let actual = recovery_archive_file_set_until(root, deadline)?;
    if actual != expected {
        let unexpected = actual.difference(&expected).next();
        let missing = expected.difference(&actual).next();
        return Err(match (unexpected, missing) {
            (Some(path), _) => format!("archive contains an unlisted file: {}", path.display()),
            (_, Some(path)) => format!("archive is missing a listed file: {}", path.display()),
            _ => "archive file inventory does not match its manifest".into(),
        });
    }
    for path in [
        MANIFEST_FILE,
        README_FILE,
        STAGED_PATCH_FILE,
        UNSTAGED_PATCH_FILE,
    ]
    .into_iter()
    .chain(manifest.bundle.as_deref())
    {
        check_archive_deadline(deadline, "archive verification")?;
        let path = root.join(path);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(format!(
                "archive component is not a regular file: {}",
                path.display()
            ));
        }
    }
    for (name, expected_bytes) in [
        (STAGED_PATCH_FILE, manifest.staged_patch.bytes),
        (UNSTAGED_PATCH_FILE, manifest.unstaged_patch.bytes),
    ] {
        let path = root.join(name);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if metadata.len() != expected_bytes {
            return Err(format!(
                "{name} byte length does not match its manifest entry"
            ));
        }
    }
    for (directory, files) in [
        (UNTRACKED_DIRECTORY, manifest.untracked_files.as_slice()),
        (
            IGNORED_DIRECTORY,
            manifest.ignored_files.as_deref().unwrap_or_default(),
        ),
    ] {
        for file in files {
            check_archive_deadline(deadline, "archive payload verification")?;
            let path = root.join(directory).join(&file.path);
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
            if metadata.file_type().is_symlink() != file.symlink
                || (!file.symlink && !metadata.is_file())
            {
                return Err(format!(
                    "archive entry {} has a different file type than its manifest",
                    path.display()
                ));
            }
            if !file.symlink && metadata.len() != file.bytes {
                return Err(format!(
                    "archive entry {} has a different byte length than its manifest",
                    path.display()
                ));
            }
            if !file.symlink
                && matches!(
                    manifest.schema_version,
                    PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION
                        | PREVIOUS_BUNDLE_RECOVERY_ARCHIVE_SCHEMA_VERSION
                        | RECOVERY_ARCHIVE_SCHEMA_VERSION
                        | RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                        | PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                )
                && permission_mode(&metadata) != file.mode
            {
                return Err(format!(
                    "archive entry {} has a different file mode than its manifest",
                    path.display()
                ));
            }
        }
    }
    if matches!(
        manifest.schema_version,
        PREVIOUS_BUNDLE_RECOVERY_ARCHIVE_SCHEMA_VERSION
            | RECOVERY_ARCHIVE_SCHEMA_VERSION
            | RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
            | PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
    ) && let Some(bundle) = manifest.bundle.as_deref()
    {
        let path = root.join(bundle);
        let (bytes, sha256) =
            hash_regular_file_no_follow_until(&path, MAX_RECOVERY_BUNDLE_BYTES, deadline)
                .map_err(|error| format!("cannot verify {BUNDLE_FILE} integrity: {error}"))?;
        if Some(bytes) != manifest.bundle_bytes
            || Some(sha256.as_str()) != manifest.bundle_sha256.as_deref()
        {
            return Err(format!(
                "{BUNDLE_FILE} differs from its manifest integrity record"
            ));
        }
    }
    Ok(())
}

/// Verify that the archive still contains the exact reviewed plan. Called
/// after writing and again under the lifecycle claim immediately before the
/// worktree is removed.
pub(crate) fn verify_recovery_archive_for_plan(
    repository: &GitRepo,
    root: &Path,
    plan: &CleanupResolvePlan,
) -> Result<(), String> {
    let root_metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("cannot inspect archive directory: {error}"))?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err("archive path is not a real directory".into());
    }
    if root
        .extension()
        .is_some_and(|extension| extension == "partial")
    {
        return Err("archive is incomplete".into());
    }
    if !root
        .parent()
        .is_some_and(|parent| same_path(parent, &plan.archive_root))
    {
        return Err("archive is outside the reviewed archive root".into());
    }
    verify_recovery_archive_payload_for_plan_until(
        repository,
        root,
        plan,
        effective_archive_deadline(None),
    )?;
    if plan.recovery_archive_schema_version == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION {
        let manifest_bytes = read_recovery_manifest(&root.join(MANIFEST_FILE))?;
        let manifest: StoredManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| format!("unreadable {MANIFEST_FILE}: {error}"))?;
        let refs = manifest
            .prerequisite_refs
            .as_deref()
            .ok_or_else(|| "schema v6 archive is missing its prerequisite refs".to_owned())?;
        verify_recovery_prerequisite_refs_installed(repository, refs, recovery_bundle_deadline()?)?;
    }
    Ok(())
}

fn install_recovery_prerequisite_refs_in_archive(
    repository: &GitRepo,
    root: &Path,
    deadline: Instant,
) -> Result<(), String> {
    let manifest_bytes = read_recovery_manifest(&root.join(MANIFEST_FILE))?;
    let manifest: StoredManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("unreadable {MANIFEST_FILE}: {error}"))?;
    validate_manifest_identity(&manifest)?;
    if manifest.schema_version != PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION {
        return Ok(());
    }
    let refs = manifest
        .prerequisite_refs
        .as_deref()
        .ok_or_else(|| "schema v6 archive is missing its prerequisite refs".to_owned())?;
    ensure_recovery_prerequisite_refs_installed(repository, refs, deadline)?;
    verify_recovery_prerequisite_refs_installed(repository, refs, deadline)
}

fn require_no_recovery_prerequisite_refs_for_partial(
    repository: &GitRepo,
    root: &Path,
    deadline: Instant,
) -> Result<(), String> {
    let (record, _) = read_retirement_archive_record(root)?;
    if record.schema_version != PINNED_RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION {
        return Err("partial archive is not a schema v6 retirement record".into());
    }
    let refs = record
        .prerequisite_refs
        .as_deref()
        .ok_or_else(|| "partial archive has no prerequisite-ref identity".to_owned())?;
    if !list_recovery_prerequisite_refs(repository, refs, deadline)?.is_empty() {
        return Err("one or more prerequisite refs already exist".into());
    }
    Ok(())
}

/// Verify archive payload and manifest against a plan without imposing a
/// final archive path. The writer uses this only for its owned `.partial`
/// directory; the final removal path uses the stricter wrapper above.
fn verify_recovery_archive_payload_for_plan(
    repository: &GitRepo,
    root: &Path,
    plan: &CleanupResolvePlan,
) -> Result<(), String> {
    verify_recovery_archive_payload_for_plan_until(
        repository,
        root,
        plan,
        effective_archive_deadline(None),
    )
}

fn verify_recovery_archive_payload_for_plan_until(
    repository: &GitRepo,
    root: &Path,
    plan: &CleanupResolvePlan,
    deadline: Option<Instant>,
) -> Result<(), String> {
    let deadline = effective_archive_deadline(deadline);
    check_archive_deadline(deadline, "archive verification")?;
    let bytes = read_recovery_manifest(&root.join(MANIFEST_FILE))?;
    let manifest: StoredManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("unreadable {MANIFEST_FILE}: {error}"))?;
    validate_manifest_identity(&manifest)?;
    verify_recovery_archive_tree_until(root, &manifest, deadline)?;
    if manifest.schema_version == RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION {
        verify_retirement_archive_record_for_manifest(root, &manifest)?;
        let proofs = manifest
            .recovery_root_proofs
            .as_deref()
            .ok_or_else(|| "schema v5 archive is missing its recovery root proofs".to_owned())?;
        validate_root_proof_delivery_availability(repository, &manifest.delivery_targets, proofs)?;
        if proofs != plan.recovery_root_proofs.as_slice() {
            return Err("archive recovery root proofs differ from the reviewed plan".into());
        }
    } else if manifest.schema_version == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION {
        verify_retirement_archive_record_for_manifest(root, &manifest)?;
        verify_retirement_archive_record_for_content_identity(root, plan)?;
        let proofs = manifest
            .recovery_root_proofs
            .as_deref()
            .ok_or_else(|| "schema v6 archive is missing its recovery root proofs".to_owned())?;
        validate_root_proof_delivery_availability(repository, &manifest.delivery_targets, proofs)?;
        let identities = recovery_root_identities_for_plan_parts(
            &plan.head,
            &plan.branch_ref,
            plan.branch_tip.as_deref(),
            plan.checkout_branch_ref.as_deref(),
            plan.checkout_branch_tip.as_deref(),
        )?;
        validate_root_proof_identities(proofs, &identities)?;
        validate_delivery_prerequisites_remain_current(
            repository,
            &manifest.delivery_targets,
            &plan.delivery_targets,
        )?;
        let bundled_roots = recovery_bundle_roots_from_manifest(&manifest)?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        for (reference, object_id) in &identities {
            if !recovery_root_is_available(reference, object_id, &bundled_roots, || {
                root_available_from_delivery(repository, object_id, &plan.delivery_targets)
            })? {
                return Err(format!(
                    "schema v6 archive neither currently delivers nor bundles required root {reference} at {object_id}"
                ));
            }
        }
    }
    let mut informational_ignored_directories = false;
    let expected_plan_digest = match manifest.schema_version {
        PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            let policy = crate::AutoCleanupPolicy::load(repository.root()).map_err(|reason| {
                format!("cannot verify legacy ignored cleanup policy: {reason}")
            })?;
            let proof = IgnoredPolicyProof {
                strict: policy.strict,
                regenerable: &policy.regenerable,
            };
            let proof_bytes = serde_json::to_vec(&proof)
                .map_err(|error| format!("cannot encode legacy ignored policy: {error}"))?;
            if sha256_hex(&proof_bytes) != plan.ignored_policy_sha256 {
                return Err("legacy archive ignored policy differs from the current plan".into());
            }
            Some(legacy_v2_cleanup_resolve_digest(plan, &proof)?)
        }
        PREVIOUS_BUNDLE_RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            let policy = crate::AutoCleanupPolicy::load(repository.root()).map_err(|reason| {
                format!("cannot verify legacy ignored cleanup policy: {reason}")
            })?;
            let proof = IgnoredPolicyProof {
                strict: policy.strict,
                regenerable: &policy.regenerable,
            };
            let proof_bytes = serde_json::to_vec(&proof)
                .map_err(|error| format!("cannot encode legacy ignored policy: {error}"))?;
            if sha256_hex(&proof_bytes) != plan.ignored_policy_sha256 {
                return Err("legacy archive ignored policy differs from the current plan".into());
            }
            let archived_digest = manifest.plan_digest.as_deref();
            let v4 = legacy_v4_cleanup_resolve_digest(plan, &proof)?;
            if archived_digest == Some(v4.as_str()) {
                informational_ignored_directories = true;
                Some(v4)
            } else {
                Some(legacy_v3_cleanup_resolve_digest(plan, &proof)?)
            }
        }
        RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            informational_ignored_directories = true;
            (manifest.plan_digest.as_deref() == Some(plan.digest.as_str()))
                .then(|| plan.digest.clone())
        }
        RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            informational_ignored_directories = true;
            (manifest.plan_digest.as_deref() == Some(plan.digest.as_str()))
                .then(|| plan.digest.clone())
        }
        PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            informational_ignored_directories = true;
            (manifest.plan_digest.as_deref() == Some(plan.digest.as_str()))
                .then(|| plan.digest.clone())
        }
        _ => None,
    };
    let expected_landing = serde_json::to_value(&plan.landing)
        .map_err(|error| format!("cannot encode reviewed landing verdict: {error}"))?;
    let expected_retirement_record_sha256 = match plan.recovery_archive_schema_version {
        RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            Some(retirement_archive_record_sha256(root, plan)?)
        }
        PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            verify_retirement_archive_record_for_content_identity(root, plan)?;
            let (_, record_bytes) = read_retirement_archive_record(root)?;
            Some(sha256_hex(&record_bytes))
        }
        _ => None,
    };
    let expected_root_proofs_sha256 =
        if plan.recovery_archive_schema_version == RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION {
            Some(recovery_root_proofs_sha256(&plan.recovery_root_proofs)?)
        } else {
            None
        };
    let expected_delivery_targets_sha256 =
        if plan.recovery_archive_schema_version == RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION {
            Some(delivery_targets_sha256(&plan.delivery_targets)?)
        } else {
            None
        };
    if expected_plan_digest
        .as_deref()
        .is_none_or(|expected| manifest.plan_digest.as_deref() != Some(expected))
        || (plan.recovery_archive_schema_version == RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
            && (manifest.schema_version != RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                || manifest.minimum_archive_retention_ms != plan.minimum_archive_retention_ms
                || manifest.retirement_record_sha256.as_deref()
                    != expected_retirement_record_sha256.as_deref()
                || manifest.checkout_branch_ref != plan.checkout_branch_ref
                || manifest.checkout_branch_tip != plan.checkout_branch_tip
                || manifest.recovery_root_proofs_sha256.as_deref()
                    != expected_root_proofs_sha256.as_deref()
                || manifest.delivery_targets_sha256.as_deref()
                    != expected_delivery_targets_sha256.as_deref()))
        || (plan.recovery_archive_schema_version
            == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
            && (manifest.schema_version != PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                || manifest.minimum_archive_retention_ms != plan.minimum_archive_retention_ms
                || manifest.retirement_record_sha256.as_deref()
                    != expected_retirement_record_sha256.as_deref()
                || manifest.checkout_branch_ref != plan.checkout_branch_ref
                || manifest.checkout_branch_tip != plan.checkout_branch_tip))
        || manifest.session_id != plan.session_id
        || !same_path(Path::new(&manifest.repository), repository.root())
        || (matches!(
            manifest.schema_version,
            RECOVERY_ARCHIVE_SCHEMA_VERSION
                | RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                | PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        ) && !same_path(
            Path::new(&manifest.repository),
            Path::new(&plan.repository_path),
        ))
        || manifest.worktree_path != plan.worktree_path
        || manifest.branch_ref != plan.branch_ref
        || (matches!(
            manifest.schema_version,
            RECOVERY_ARCHIVE_SCHEMA_VERSION
                | RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                | PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        ) && manifest.branch_tip != plan.branch_tip)
        || (manifest.schema_version != PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
            && manifest.disposition != plan.disposition.as_str())
        || manifest.head != plan.head
        || (manifest.schema_version != PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
            && (manifest.delivery_targets != plan.delivery_targets
                || manifest.unlanded_commits != plan.unlanded_commits))
        || (matches!(
            manifest.schema_version,
            RECOVERY_ARCHIVE_SCHEMA_VERSION | RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        ) && manifest.landing != expected_landing)
        || manifest.staged_patch.bytes != plan.staged_patch_bytes
        || manifest.staged_patch.sha256 != plan.staged_patch_sha256
        || manifest.unstaged_patch.bytes != plan.unstaged_patch_bytes
        || manifest.unstaged_patch.sha256 != plan.unstaged_patch_sha256
        || manifest.untracked_files != plan.untracked_files
        || !manifest.ignored_paths.as_deref().is_some_and(|paths| {
            if informational_ignored_directories {
                authorization_ignored_paths(paths)
                    == authorization_ignored_paths(&plan.ignored_paths)
            } else {
                paths == plan.ignored_paths.as_slice()
            }
        })
        || manifest.ignored_files.as_deref() != Some(plan.ignored_files.as_slice())
        || manifest.ignored_policy_sha256.as_deref() != Some(plan.ignored_policy_sha256.as_str())
    {
        return Err("archive manifest does not match the reviewed resolve plan".into());
    }

    for (name, expected_bytes, expected_sha256) in [
        (
            STAGED_PATCH_FILE,
            plan.staged_patch_bytes,
            plan.staged_patch_sha256.as_str(),
        ),
        (
            UNSTAGED_PATCH_FILE,
            plan.unstaged_patch_bytes,
            plan.unstaged_patch_sha256.as_str(),
        ),
    ] {
        let path = root.join(name);
        let actual = describe_untracked_until(&path, name, deadline)
            .map_err(|error| format!("cannot verify {name}: {error}"))?
            .ok_or_else(|| format!("{name} is not a regular file"))?;
        if actual.bytes != expected_bytes || actual.sha256 != expected_sha256 {
            return Err(format!("{name} differs from the reviewed plan"));
        }
    }
    for (directory, files) in [
        (UNTRACKED_DIRECTORY, plan.untracked_files.as_slice()),
        (IGNORED_DIRECTORY, plan.ignored_files.as_slice()),
    ] {
        for expected in files {
            check_archive_deadline(deadline, "archive payload verification")?;
            let path = root.join(directory).join(&expected.path);
            let actual = describe_untracked_until(&path, &expected.path, deadline)
                .map_err(|error| format!("cannot verify {}: {error}", path.display()))?
                .ok_or_else(|| format!("{} is not a regular file or symlink", path.display()))?;
            if &actual != expected {
                return Err(format!("{} differs from the reviewed plan", path.display()));
            }
        }
    }
    if manifest.bundle.is_some() {
        let bundle = root.join(BUNDLE_FILE);
        let bundle_deadline = recovery_bundle_deadline()?;
        let deadline = deadline.map_or(bundle_deadline, |operation| operation.min(bundle_deadline));
        verify_recovery_bundle(
            repository,
            &bundle,
            &recovery_bundle_roots_from_manifest(&manifest)?,
            deadline,
        )?;
    }
    let expected_readme = restore_readme(plan, manifest.bundle.is_some());
    let readme_path = root.join(README_FILE);
    let readme_metadata = std::fs::symlink_metadata(&readme_path)
        .map_err(|error| format!("cannot inspect {README_FILE}: {error}"))?;
    if readme_metadata.len() != expected_readme.len() as u64 {
        return Err(format!(
            "{README_FILE} differs from the reviewed restore steps"
        ));
    }
    let readme = read_regular_file_no_follow(&readme_path, MAX_RECOVERY_README_BYTES)
        .map_err(|error| format!("cannot safely read {README_FILE}: {error}"))?;
    if readme != expected_readme.as_bytes() {
        return Err(format!(
            "{README_FILE} differs from the reviewed restore steps"
        ));
    }
    Ok(())
}

impl Broker {
    /// Prove `dir` is a complete recovery archive this repository's broker
    /// wrote, or say why not. Used both to propose an archive and, again, to
    /// remove one, so nothing is deleted on evidence checked only at review.
    pub(crate) fn owned_recovery_archive(
        &self,
        dir: &Path,
    ) -> Result<OwnedRecoveryArchive, String> {
        let configured_root = self
            .recovery_archive_root()
            .map_err(|error| format!("no recovery archive root: {error}"))?;
        let root = inspect_archive_root(&configured_root)
            .map_err(|error| format!("unsafe recovery archive root: {}", error.reason))?
            .ok_or_else(|| "recovery archive root is missing".to_string())?;
        let metadata =
            std::fs::symlink_metadata(dir).map_err(|error| format!("cannot read: {error}"))?;
        if !metadata.is_dir() {
            return Err("not a directory".into());
        }
        if dir
            .extension()
            .is_some_and(|extension| extension == "partial")
        {
            return Err("an archive that was never completed".into());
        }
        if !dir.parent().is_some_and(|parent| same_path(parent, &root)) {
            return Err(format!("not directly under {}", root.display()));
        }
        let manifest_path = dir.join(MANIFEST_FILE);
        let bytes = read_recovery_manifest(&manifest_path)
            .map_err(|reason| format!("no broker manifest; {reason}"))?;
        let manifest: StoredManifest = serde_json::from_slice(&bytes)
            .map_err(|error| format!("unreadable manifest: {error}"))?;
        if !matches!(
            manifest.schema_version,
            LEGACY_RECOVERY_ARCHIVE_SCHEMA_VERSION
                | PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION
                | PREVIOUS_BUNDLE_RECOVERY_ARCHIVE_SCHEMA_VERSION
                | RECOVERY_ARCHIVE_SCHEMA_VERSION
                | RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
                | PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        ) {
            return Err(format!(
                "manifest schema {} is not supported",
                manifest.schema_version
            ));
        }
        validate_manifest_identity(&manifest)?;
        if manifest.schema_version == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION {
            let retirement_digest = manifest
                .plan_digest
                .as_deref()
                .ok_or_else(|| "schema v6 archive is missing its plan digest".to_owned())?;
            let expected_name = pinned_retirement_archive_name(
                manifest.session_id,
                &manifest.head,
                retirement_digest,
            )?;
            if dir.file_name().and_then(std::ffi::OsStr::to_str) != Some(expected_name.as_str()) {
                return Err(format!(
                    "schema v6 archive directory name is not its canonical retirement identity: expected {expected_name}"
                ));
            }
        }
        verify_recovery_archive_tree(dir, &manifest)?;
        if is_retirement_archive_schema(manifest.schema_version) {
            verify_retirement_archive_record_for_manifest(dir, &manifest)?;
        }
        if !same_path(Path::new(&manifest.repository), self.main_root()) {
            return Err(format!(
                "manifest names another repository: {}",
                manifest.repository
            ));
        }
        let session = self
            .store_ref()
            .session(manifest.session_id)
            .map_err(|_| format!("session {} is not recorded here", manifest.session_id))?;
        let short = |branch: &str| branch.trim_start_matches("refs/heads/").to_owned();
        if short(&session.branch) != short(&manifest.branch_ref) {
            return Err(format!(
                "session {} ran on {}, not {}",
                manifest.session_id, session.branch, manifest.branch_ref
            ));
        }
        if manifest.schema_version == RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION {
            let proofs = manifest.recovery_root_proofs.as_deref().ok_or_else(|| {
                "schema v5 archive is missing its recovery root proofs".to_owned()
            })?;
            validate_available_root_proofs_from_delivery(
                self.repo_handle(),
                &manifest.delivery_targets,
                proofs,
            )?;
            let bundled_roots = recovery_bundle_roots_from_manifest(&manifest)?;
            if !bundled_roots.is_empty() {
                verify_recovery_bundle(
                    self.repo_handle(),
                    &dir.join(BUNDLE_FILE),
                    &bundled_roots,
                    recovery_bundle_deadline()?,
                )?;
            }
        } else if manifest.schema_version == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION {
            let proofs = manifest.recovery_root_proofs.as_deref().ok_or_else(|| {
                "schema v6 archive is missing its recovery root proofs".to_owned()
            })?;
            validate_available_root_proofs_from_delivery(
                self.repo_handle(),
                &manifest.delivery_targets,
                proofs,
            )?;
            let bundled_roots = recovery_bundle_roots_from_manifest(&manifest)?;
            if !bundled_roots.is_empty() {
                verify_recovery_bundle(
                    self.repo_handle(),
                    &dir.join(BUNDLE_FILE),
                    &bundled_roots,
                    recovery_bundle_deadline()?,
                )?;
            }
            let prerequisite_refs = manifest
                .prerequisite_refs
                .as_deref()
                .ok_or_else(|| "schema v6 archive is missing its prerequisite refs".to_owned())?;
            verify_recovery_prerequisite_refs_installed(
                self.repo_handle(),
                prerequisite_refs,
                recovery_bundle_deadline()?,
            )?;
        }
        let recovery_roots = if is_retirement_archive_schema(manifest.schema_version) {
            manifest
                .recovery_root_proofs
                .clone()
                .ok_or_else(|| "schema v5 archive is missing its recovery root proofs".to_owned())?
        } else {
            vec![RecoveryRootProof {
                ref_name: "HEAD".into(),
                object_id: manifest.head.clone(),
                available_from_delivery: !manifest.unlanded_commits.contains(&manifest.head),
            }]
        };
        Ok(OwnedRecoveryArchive {
            path: dir.to_path_buf(),
            session_id: manifest.session_id,
            head: manifest.head,
            recovery_roots,
            created_at_ms: manifest.created_at_ms,
            minimum_archive_retention_ms: manifest.minimum_archive_retention_ms,
            prerequisite_refs: manifest.prerequisite_refs.clone().unwrap_or_default(),
            retirement_digest: (manifest.schema_version
                == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION)
                .then(|| manifest.plan_digest.clone())
                .flatten(),
            prerequisite_targets: if manifest.schema_version
                == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
            {
                manifest.delivery_targets.clone()
            } else {
                Vec::new()
            },
            holds_uncommitted: manifest.staged_patch.bytes > 0
                || manifest.unstaged_patch.bytes > 0
                || !manifest.untracked_files.is_empty()
                || manifest
                    .ignored_files
                    .as_deref()
                    .is_some_and(|files| !files.is_empty()),
        })
    }

    /// Release v6 incremental-bundle prerequisite refs only after the caller
    /// has removed the complete archive tree. Every ref is deleted with its
    /// manifest-bound expected OID in one Git ref transaction.
    pub(crate) fn release_recovery_archive_prerequisite_refs_after_removal(
        &self,
        archive: &OwnedRecoveryArchive,
    ) -> Result<(), String> {
        let configured_root = self
            .recovery_archive_root()
            .map_err(|error| format!("cannot resolve recovery archive root: {error}"))?;
        let root = inspect_archive_root(&configured_root).map_err(|error| {
            format!(
                "cannot safely inspect recovery archive root: {}",
                error.reason
            )
        })?;
        remove_recovery_prerequisite_refs_after_archive_removal(
            self.repo_handle(),
            archive,
            root.as_deref(),
        )
    }

    /// Whether every preserved root reached the primary checkout or upstream,
    /// and the archive holds no uncommitted changes. Schema-4 and older archives
    /// expose only HEAD; schema-5 archives also include recorded/current branch roots.
    pub(crate) fn recovery_archive_landed(&self, archive: &OwnedRecoveryArchive) -> bool {
        if archive.holds_uncommitted {
            return false;
        }
        let repo = self.repo_handle();
        let mut targets = Vec::new();
        if let Ok(primary) = repo.head_commit() {
            targets.push(primary);
        }
        if let Some((_upstream, head)) = repo.tracking_upstream() {
            targets.push(head);
        }
        !archive.recovery_roots.is_empty()
            && archive.recovery_roots.iter().all(|root| {
                targets.iter().any(|target| {
                    matches!(
                        crate::work_landed(repo, &root.object_id, target),
                        Ok(crate::LandingVerdict::Landed { .. })
                    )
                })
            })
    }

    /// Every archive under this repository's recovery archive directory,
    /// split into the ones the broker can vouch for and the rest (with why).
    pub(crate) fn recovery_archives(&self) -> Result<RecoveryArchiveScan, BrokerOpError> {
        let configured_root = self.recovery_archive_root()?;
        let root = match inspect_archive_root(&configured_root) {
            Ok(Some(root)) => root,
            Ok(None) => {
                return Ok(RecoveryArchiveScan {
                    root: configured_root,
                    owned: Vec::new(),
                    unowned: Vec::new(),
                });
            }
            Err(error) => {
                return Ok(RecoveryArchiveScan {
                    root: configured_root.clone(),
                    owned: Vec::new(),
                    unowned: vec![(error.path, format!("unsafe archive root: {}", error.reason))],
                });
            }
        };
        let mut owned = Vec::new();
        let mut unowned = Vec::new();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) => {
                return Ok(RecoveryArchiveScan {
                    root: configured_root,
                    owned,
                    unowned: vec![(root, format!("cannot read archive root: {error}"))],
                });
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    return Ok(RecoveryArchiveScan {
                        root: configured_root.clone(),
                        owned: Vec::new(),
                        unowned: vec![(root, format!("cannot read archive entry: {error}"))],
                    });
                }
            };
            let path = entry.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    return Ok(RecoveryArchiveScan {
                        root: configured_root.clone(),
                        owned: Vec::new(),
                        unowned: vec![(root, format!("cannot inspect archive entry: {error}"))],
                    });
                }
            };
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                paths.push(path);
            }
        }
        paths.sort();
        for path in paths {
            match self.owned_recovery_archive(&path) {
                Ok(archive) => owned.push(archive),
                Err(reason) => unowned.push((path, reason)),
            }
        }
        Ok(RecoveryArchiveScan {
            root: configured_root,
            owned,
            unowned,
        })
    }
}

fn require_real_directory(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!("{} is not a real directory", path.display()));
    }
    Ok(())
}

/// Validate the configured archive path without following a symlink at the
/// owned storage anchor, archive parent, or repository-specific root. The
/// canonical paths must retain the exact expected parent chain.
#[derive(Debug)]
struct ArchiveRootError {
    path: PathBuf,
    reason: String,
}

fn inspect_archive_root(path: &Path) -> Result<Option<PathBuf>, ArchiveRootError> {
    let parent = path.parent().ok_or_else(|| ArchiveRootError {
        path: path.to_path_buf(),
        reason: "archive root has no parent directory".into(),
    })?;
    let anchor = parent.parent().ok_or_else(|| ArchiveRootError {
        path: path.to_path_buf(),
        reason: "archive root has no owned storage anchor".into(),
    })?;

    for (label, component) in [
        ("archive storage anchor", anchor),
        ("archive parent", parent),
        ("archive root", path),
    ] {
        match std::fs::symlink_metadata(component) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(ArchiveRootError {
                    path: component.to_path_buf(),
                    reason: format!("{label} is not a real directory"),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(ArchiveRootError {
                    path: component.to_path_buf(),
                    reason: format!("cannot inspect {label}: {error}"),
                });
            }
        }
    }

    let canonical_anchor = std::fs::canonicalize(anchor).map_err(|error| ArchiveRootError {
        path: anchor.to_path_buf(),
        reason: format!("cannot resolve archive storage anchor: {error}"),
    })?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| ArchiveRootError {
        path: parent.to_path_buf(),
        reason: format!("cannot resolve archive parent: {error}"),
    })?;
    let canonical_root = std::fs::canonicalize(path).map_err(|error| ArchiveRootError {
        path: path.to_path_buf(),
        reason: format!("cannot resolve archive root: {error}"),
    })?;
    if canonical_parent.parent() != Some(canonical_anchor.as_path())
        || canonical_root.parent() != Some(canonical_parent.as_path())
    {
        return Err(ArchiveRootError {
            path: path.to_path_buf(),
            reason: "archive root does not remain directly beneath its owned storage anchor".into(),
        });
    }
    Ok(Some(canonical_root))
}

/// Resolve the configured archive root once and reject a symlink at the
/// archive boundary before any payload write can follow it.
fn prepare_archive_root(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "archive root has no parent directory".to_string())?;
    let anchor = parent
        .parent()
        .ok_or_else(|| "archive root has no owned storage anchor".to_string())?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            require_real_directory(anchor)?;
            require_real_directory(parent)?;
            require_real_directory(path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            require_real_directory(anchor)?;
            match std::fs::symlink_metadata(parent) {
                Ok(_) => require_real_directory(parent)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(parent)
                        .map_err(|error| format!("cannot create archive parent: {error}"))?;
                    require_real_directory(parent)?;
                }
                Err(error) => {
                    return Err(format!("cannot inspect archive parent: {error}"));
                }
            }
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!("cannot create archive root: {error}"));
                }
            }
            require_real_directory(path)?;
        }
        Err(error) => return Err(format!("cannot inspect archive root: {error}")),
    }
    inspect_archive_root(path)
        .map_err(|error| error.reason)?
        .ok_or_else(|| "archive root disappeared during setup".into())
}

fn create_exclusive_archive_directory(parent: &Path, name: &str) -> Result<PathBuf, String> {
    require_real_directory(parent)?;
    if !is_contained_relative(name) || name.contains('/') {
        return Err("archive staging directory name is invalid".into());
    }
    let path = parent.join(name);
    std::fs::create_dir(&path)
        .map_err(|error| format!("cannot exclusively create {}: {error}", path.display()))?;
    require_real_directory(&path)?;
    Ok(path)
}

fn ensure_archive_directory(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    require_real_directory(root)?;
    if relative.as_os_str().is_empty() {
        return Ok(root.to_path_buf());
    }
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "archive directory path is not contained: {}",
            relative.display()
        ));
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err("archive directory path is not contained".into());
        };
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(format!(
                        "archive parent is not a real directory: {}",
                        current.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(format!(
                            "cannot create archive directory {}: {error}",
                            current.display()
                        ));
                    }
                }
                require_real_directory(&current)?;
            }
            Err(error) => {
                return Err(format!(
                    "cannot inspect archive directory {}: {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(current)
}

fn create_new_archive_file(root: &Path, relative: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let (path, mut file) = create_new_archive_file_handle(root, relative)?;
    file.write_all(bytes)
        .map_err(|error| format!("cannot write archive file {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync archive file {}: {error}", path.display()))?;
    Ok(path)
}

fn create_new_archive_file_handle(
    root: &Path,
    relative: &str,
) -> Result<(PathBuf, std::fs::File), String> {
    if !is_contained_relative(relative) {
        return Err(format!("archive file path is not contained: {relative:?}"));
    }
    let relative_path = Path::new(relative);
    let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
    let parent = ensure_archive_directory(root, parent)?;
    let name = relative_path
        .file_name()
        .ok_or_else(|| format!("archive file path has no name: {relative:?}"))?;
    let path = parent.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|error| format!("cannot create archive file {}: {error}", path.display()))?;
    Ok((path, file))
}

fn recovery_bundle_deadline() -> Result<Instant, String> {
    let requested = Instant::now()
        .checked_add(crate::git::git_timeout())
        .ok_or_else(|| "recovery bundle deadline overflow".to_owned())?;
    Ok(crate::git::active_git_deadline().map_or(requested, |active| active.min(requested)))
}

fn recovery_git_budget(deadline: Instant) -> Result<(Instant, Duration), String> {
    let effective_deadline =
        crate::git::active_git_deadline().map_or(deadline, |active| active.min(deadline));
    let remaining = effective_deadline
        .saturating_duration_since(Instant::now())
        .min(crate::git::git_timeout());
    if remaining.is_zero() {
        return Err("recovery bundle Git deadline expired".into());
    }
    Ok((effective_deadline, remaining))
}

fn stream_recovery_command_stdout(
    command: &mut Command,
    writer: &mut std::fs::File,
    max_bundle_bytes: u64,
    deadline: Instant,
) -> Result<u64, String> {
    let (effective_deadline, budget) = recovery_git_budget(deadline)?;
    let output = crate::bounded_output::stream_stdout_to_writer_limited_in_owned_group(
        command,
        budget,
        max_bundle_bytes,
        MAX_RECOVERY_BUNDLE_STDERR_BYTES,
        writer,
    )
    .map_err(|error| format!("cannot stream recovery bundle output: {error}"))?;
    match output {
        crate::bounded_output::LimitedStreamOutput::Completed {
            status,
            stdout_bytes,
            stderr,
        } => {
            if Instant::now() >= effective_deadline {
                return Err("recovery bundle Git deadline expired".into());
            }
            if !status.success() {
                return Err(format!(
                    "Git bundle command failed: {}",
                    String::from_utf8_lossy(&stderr).trim()
                ));
            }
            if stdout_bytes == 0 {
                return Err("Git bundle command produced an empty bundle".into());
            }
            Ok(stdout_bytes)
        }
        crate::bounded_output::LimitedStreamOutput::Deadline => {
            Err("Git bundle command exceeded its deadline; source checkout retained".into())
        }
        crate::bounded_output::LimitedStreamOutput::StdoutLimit => Err(format!(
            "Git bundle exceeded its hard {max_bundle_bytes}-byte cap; source checkout retained and bounded partial archive kept"
        )),
        crate::bounded_output::LimitedStreamOutput::StderrLimit => Err(format!(
            "Git bundle stderr exceeded its {MAX_RECOVERY_BUNDLE_STDERR_BYTES}-byte cap; source checkout retained"
        )),
    }
}

fn write_recovery_bundle_to_file(
    checkout: &GitRepo,
    excluded: &[String],
    additional_roots: &[String],
    max_bundle_bytes: u64,
    deadline: Instant,
    file: &mut std::fs::File,
) -> Result<u64, String> {
    let mut command = crate::git::git_command();
    command
        // Recovery runs alongside live agents; avoid Git's automatic packing
        // parallelism and bound its delta search/cache memory.
        .args([
            "-c",
            "pack.threads=1",
            "-c",
            "pack.windowMemory=32m",
            "-c",
            "pack.deltaCacheSize=16m",
        ])
        .args(["bundle", "create", "-", "HEAD"])
        .args(additional_roots.iter().map(String::as_str))
        .arg("--not")
        .args(excluded.iter().map(String::as_str))
        .current_dir(checkout.root())
        .env("GIT_OPTIONAL_LOCKS", "0");
    stream_recovery_command_stdout(&mut command, file, max_bundle_bytes, deadline)
}

fn run_recovery_git_output_limited(
    command: &mut Command,
    deadline: Instant,
    label: &str,
) -> Result<std::process::Output, String> {
    let (effective_deadline, budget) = recovery_git_budget(deadline)?;
    let result = crate::bounded_output::output_limited_in_owned_group(
        command,
        budget,
        MAX_RECOVERY_BUNDLE_METADATA_OUTPUT_BYTES,
    )
    .map_err(|error| format!("cannot run {label}: {error}"))?;
    let output = match result {
        crate::bounded_output::LimitedOutput::Completed(output) => output,
        crate::bounded_output::LimitedOutput::Deadline => {
            return Err(format!("{label} exceeded its Git deadline"));
        }
        crate::bounded_output::LimitedOutput::OutputLimit => {
            return Err(format!(
                "{label} output exceeded its {MAX_RECOVERY_BUNDLE_METADATA_OUTPUT_BYTES}-byte cap"
            ));
        }
    };
    if Instant::now() >= effective_deadline {
        return Err(format!("{label} exceeded its Git deadline"));
    }
    if !output.status.success() {
        return Err(format!(
            "{label} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output)
}

fn run_recovery_git_output_with_input_limited(
    command: &mut Command,
    input: &[u8],
    deadline: Instant,
    label: &str,
) -> Result<std::process::Output, String> {
    let (effective_deadline, budget) = recovery_git_budget(deadline)?;
    let mut input_file =
        tempfile::tempfile().map_err(|error| format!("cannot prepare {label} input: {error}"))?;
    input_file
        .write_all(input)
        .and_then(|()| input_file.seek(std::io::SeekFrom::Start(0)).map(|_| ()))
        .map_err(|error| format!("cannot stage {label} input: {error}"))?;
    let result = crate::bounded_output::output_with_input_limited(
        command,
        input_file,
        budget,
        MAX_RECOVERY_BUNDLE_METADATA_OUTPUT_BYTES,
    )
    .map_err(|error| format!("cannot run {label}: {error}"))?;
    let output = match result {
        crate::bounded_output::LimitedOutput::Completed(output) => output,
        crate::bounded_output::LimitedOutput::Deadline => {
            return Err(format!("{label} exceeded its Git deadline"));
        }
        crate::bounded_output::LimitedOutput::OutputLimit => {
            return Err(format!(
                "{label} output exceeded its {MAX_RECOVERY_BUNDLE_METADATA_OUTPUT_BYTES}-byte cap"
            ));
        }
    };
    if Instant::now() >= effective_deadline {
        return Err(format!("{label} exceeded its Git deadline"));
    }
    if !output.status.success() {
        return Err(format!(
            "{label} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output)
}

fn list_recovery_prerequisite_refs(
    repository: &GitRepo,
    expected: &[RecoveryArchivePrerequisiteRef],
    deadline: Instant,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let Some(first) = expected.first() else {
        return Err("schema v6 retirement archive has no prerequisite refs".into());
    };
    let (prefix, _) = first
        .ref_name
        .rsplit_once('/')
        .ok_or_else(|| "schema v6 prerequisite ref has no sequence component".to_owned())?;
    let prefix = format!("{prefix}/");
    if expected
        .iter()
        .any(|reference| !reference.ref_name.starts_with(&prefix))
    {
        return Err("schema v6 prerequisite refs do not share one exact namespace".into());
    }
    let mut command = crate::git::git_command();
    command
        .args([
            "for-each-ref",
            "--format=%(refname) %(objectname) %(symref)",
        ])
        .arg(&prefix)
        .current_dir(repository.root())
        .env("GIT_OPTIONAL_LOCKS", "0");
    let output = run_recovery_git_output_limited(
        &mut command,
        deadline,
        "recovery prerequisite ref inspection",
    )?;
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| "recovery prerequisite ref inspection returned non-UTF-8 output".to_owned())?;
    let mut refs = std::collections::BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.splitn(3, ' ');
        let name = fields.next().unwrap_or_default();
        let object_id = fields.next().unwrap_or_default();
        let symbolic_target = fields.next().unwrap_or_default();
        if !name.starts_with(&prefix)
            || !valid_git_object_id(object_id)
            || !symbolic_target.is_empty()
            || refs.insert(name.to_owned(), object_id.to_owned()).is_some()
        {
            return Err(
                "recovery prerequisite namespace contains a malformed or symbolic ref".into(),
            );
        }
    }
    Ok(refs)
}

fn recovery_prerequisite_ref_map(
    refs: &[RecoveryArchivePrerequisiteRef],
) -> std::collections::BTreeMap<String, String> {
    refs.iter()
        .map(|reference| (reference.ref_name.clone(), reference.object_id.clone()))
        .collect()
}

fn recovery_prerequisite_ref_transaction(
    repository: &GitRepo,
    operations: &[(&str, &RecoveryArchivePrerequisiteRef)],
    deadline: Instant,
) -> Result<(), String> {
    if operations.is_empty() {
        return Ok(());
    }
    let mut input = String::from("start\noption no-deref\n");
    for (operation, reference) in operations {
        if !matches!(*operation, "create" | "delete")
            || !reference
                .ref_name
                .starts_with("refs/aethyme/recovery-prerequisites/v1/")
            || !valid_git_object_id(&reference.object_id)
        {
            return Err("refused malformed recovery prerequisite ref transaction".into());
        }
        input.push_str(operation);
        input.push(' ');
        input.push_str(&reference.ref_name);
        input.push(' ');
        input.push_str(&reference.object_id);
        input.push('\n');
    }
    input.push_str("prepare\ncommit\n");
    let mut command = crate::git::git_command();
    command
        .args([
            "-c",
            "core.fsync=reference",
            "-c",
            "core.fsyncMethod=fsync",
            "update-ref",
            "--stdin",
        ])
        .current_dir(repository.root())
        .env("GIT_OPTIONAL_LOCKS", "0");
    run_recovery_git_output_with_input_limited(
        &mut command,
        input.as_bytes(),
        deadline,
        "recovery prerequisite ref transaction",
    )?;
    Ok(())
}

fn ensure_recovery_prerequisite_refs_installed(
    repository: &GitRepo,
    expected: &[RecoveryArchivePrerequisiteRef],
    deadline: Instant,
) -> Result<(), String> {
    let expected_map = recovery_prerequisite_ref_map(expected);
    let actual = list_recovery_prerequisite_refs(repository, expected, deadline)?;
    for (name, object_id) in &actual {
        if expected_map.get(name) != Some(object_id) {
            return Err(format!(
                "recovery prerequisite ref {name} has a conflicting object id"
            ));
        }
    }
    let missing = expected
        .iter()
        .filter(|reference| !actual.contains_key(&reference.ref_name))
        .map(|reference| ("create", reference))
        .collect::<Vec<_>>();
    if !missing.is_empty()
        && let Err(first_error) =
            recovery_prerequisite_ref_transaction(repository, &missing, deadline)
    {
        // Another writer may have installed the same complete set in the
        // meantime. Accept only that exact result; never overwrite it.
        let after = list_recovery_prerequisite_refs(repository, expected, deadline)?;
        if after != expected_map {
            return Err(first_error);
        }
    }
    let installed = list_recovery_prerequisite_refs(repository, expected, deadline)?;
    if installed != expected_map {
        return Err(
            "recovery prerequisite refs were not durably installed at their expected object ids"
                .into(),
        );
    }
    Ok(())
}

fn verify_recovery_prerequisite_refs_installed(
    repository: &GitRepo,
    expected: &[RecoveryArchivePrerequisiteRef],
    deadline: Instant,
) -> Result<(), String> {
    let actual = list_recovery_prerequisite_refs(repository, expected, deadline)?;
    if actual != recovery_prerequisite_ref_map(expected) {
        return Err(
            "recovery prerequisite refs are missing or differ from their manifest-bound object ids"
                .into(),
        );
    }
    Ok(())
}

fn remove_recovery_prerequisite_refs_after_archive_removal(
    repository: &GitRepo,
    archive: &OwnedRecoveryArchive,
    archive_root: Option<&Path>,
) -> Result<(), String> {
    match std::fs::symlink_metadata(&archive.path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot confirm archive removal: {error}")),
        Ok(_) => return Err("archive tree still exists; prerequisite refs were retained".into()),
    }
    if archive.prerequisite_refs.is_empty() {
        return Ok(());
    }
    let retirement_digest = archive
        .retirement_digest
        .as_deref()
        .ok_or_else(|| "archive prerequisite refs have no retirement digest".to_owned())?;
    let expected = recovery_archive_prerequisite_refs(
        archive.session_id,
        retirement_digest,
        &archive.prerequisite_targets,
    )?;
    if expected != archive.prerequisite_refs {
        return Err(
            "archive prerequisite refs no longer match their verified archive identity".into(),
        );
    }
    let deadline = recovery_bundle_deadline()?;
    match archive_root {
        Some(root) => {
            if !archive
                .path
                .parent()
                .is_some_and(|parent| same_path(parent, root))
            {
                return Err(
                    "archive path no longer belongs directly under the configured recovery root; prerequisite refs were retained".into(),
                );
            }
            refuse_duplicate_recovery_prerequisite_refs(root, &archive.path, &expected, deadline)?;
        }
        None if archive.path.parent().is_some_and(|parent| parent.exists()) => {
            return Err(
                "configured recovery root is missing while its archive parent still exists; prerequisite refs were retained".into(),
            );
        }
        None => {}
    }
    let actual = list_recovery_prerequisite_refs(repository, &expected, deadline)?;
    let expected_map = recovery_prerequisite_ref_map(&expected);
    for (name, object_id) in &actual {
        if expected_map.get(name) != Some(object_id) {
            return Err(format!(
                "recovery prerequisite ref {name} changed; no prerequisite refs were removed"
            ));
        }
    }
    let present = expected
        .iter()
        .filter(|reference| actual.contains_key(&reference.ref_name))
        .map(|reference| ("delete", reference))
        .collect::<Vec<_>>();
    recovery_prerequisite_ref_transaction(repository, &present, deadline)?;
    if !list_recovery_prerequisite_refs(repository, &expected, deadline)?.is_empty() {
        return Err(
            "recovery prerequisite refs remain after their exact removal transaction".into(),
        );
    }
    Ok(())
}

fn refuse_duplicate_recovery_prerequisite_refs(
    root: &Path,
    removed_archive: &Path,
    expected_refs: &[RecoveryArchivePrerequisiteRef],
    deadline: Instant,
) -> Result<(), String> {
    require_real_directory(root)?;
    let expected_names = expected_refs
        .iter()
        .map(|reference| reference.ref_name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("cannot scan archive siblings before releasing refs: {error}"))?;
    let mut scanned = 0_usize;
    for entry in entries {
        if Instant::now() >= deadline {
            return Err(
                "archive sibling scan exceeded its deadline; prerequisite refs were retained"
                    .into(),
            );
        }
        scanned = scanned.saturating_add(1);
        if scanned > MAX_RECOVERY_ARCHIVE_TREE_ENTRIES {
            return Err(format!(
                "archive sibling scan exceeded its {}-entry bound; prerequisite refs were retained",
                MAX_RECOVERY_ARCHIVE_TREE_ENTRIES
            ));
        }
        let entry = entry.map_err(|error| {
            format!("cannot read archive sibling entry; prerequisite refs were retained: {error}")
        })?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            format!("cannot inspect archive sibling; prerequisite refs were retained: {error}")
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "archive sibling {} is a symlink; prerequisite refs were retained because duplicate use cannot be ruled out",
                path.display()
            ));
        }
        if metadata.is_file() {
            continue;
        }
        if !metadata.is_dir() {
            return Err(format!(
                "archive sibling {} has an unsupported file type; prerequisite refs were retained",
                path.display()
            ));
        }
        if same_path(&path, removed_archive) {
            return Err(
                "archive path reappeared during sibling scan; prerequisite refs were retained"
                    .into(),
            );
        }
        for (file_name, max_bytes, schema_version) in [
            (
                MANIFEST_FILE,
                MAX_RECOVERY_MANIFEST_BYTES,
                PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION,
            ),
            (
                RETIREMENT_ARCHIVE_RECORD_FILE,
                MAX_RETIREMENT_ARCHIVE_RECORD_BYTES,
                PINNED_RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION,
            ),
        ] {
            let file_path = path.join(file_name);
            let bytes = match read_regular_file_no_follow(&file_path, max_bytes) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!(
                        "cannot safely read archive sibling {} while checking prerequisite refs; refs were retained: {error}",
                        file_path.display()
                    ));
                }
            };
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
                format!(
                    "cannot parse archive sibling {} while checking prerequisite refs; refs were retained: {error}",
                    file_path.display()
                )
            })?;
            let is_v6 = value
                .get("schema_version")
                .and_then(serde_json::Value::as_u64)
                == Some(u64::from(schema_version));
            let Some(refs_value) = value.get("prerequisite_refs") else {
                if is_v6 {
                    return Err(format!(
                        "schema v6 sibling {} is missing prerequisite refs; refs were retained",
                        file_path.display()
                    ));
                }
                continue;
            };
            let refs: Vec<RecoveryArchivePrerequisiteRef> = serde_json::from_value(
                refs_value.clone(),
            )
            .map_err(|error| {
                format!(
                    "cannot decode prerequisite refs in sibling {}; refs were retained: {error}",
                    file_path.display()
                )
            })?;
            if is_v6 && refs.is_empty() {
                return Err(format!(
                    "schema v6 sibling {} has an empty prerequisite set; refs were retained",
                    file_path.display()
                ));
            }
            if let Some(reference) = refs
                .iter()
                .find(|reference| expected_names.contains(reference.ref_name.as_str()))
            {
                return Err(format!(
                    "archive sibling {} also references prerequisite {}; refs were retained",
                    path.display(),
                    reference.ref_name
                ));
            }
        }
    }
    Ok(())
}

fn verify_recovery_bundle(
    repository: &GitRepo,
    bundle: &Path,
    expected_roots: &[(String, String)],
    deadline: Instant,
) -> Result<(), String> {
    let mut verify = crate::git::git_command();
    verify
        .args(["bundle", "verify", "--quiet"])
        .arg(bundle)
        .current_dir(repository.root())
        .env("GIT_OPTIONAL_LOCKS", "0");
    run_recovery_git_output_limited(&mut verify, deadline, "Git bundle verify")?;

    let mut list_heads = crate::git::git_command();
    list_heads
        .args(["bundle", "list-heads"])
        .arg(bundle)
        .current_dir(repository.root())
        .env("GIT_OPTIONAL_LOCKS", "0");
    let output =
        run_recovery_git_output_limited(&mut list_heads, deadline, "Git bundle list-heads")?;
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| "Git bundle list-heads returned non-UTF-8 output".to_owned())?;
    let mut advertised_roots = std::collections::BTreeSet::new();
    for line in text.lines() {
        if line.is_empty() {
            return Err("Git bundle list-heads returned an empty entry".into());
        }
        let mut fields = line.split_whitespace();
        let object = fields.next().unwrap_or_default();
        let name = fields.next().unwrap_or_default();
        if !valid_git_object_id(object) || name.is_empty() || fields.next().is_some() {
            return Err("Git bundle list-heads returned a malformed entry".into());
        }
        advertised_roots.insert((name.to_owned(), object.to_owned()));
    }
    for (name, object) in expected_roots {
        if !advertised_roots.contains(&(name.clone(), object.clone())) {
            return Err(format!("{BUNDLE_FILE} does not hold {name} at {object}"));
        }
    }
    if expected_roots.is_empty() {
        return Err(format!("{BUNDLE_FILE} has no reviewed roots"));
    }
    Ok(())
}

struct CappedArchiveBuffer {
    bytes: Vec<u8>,
    max_bytes: usize,
}

impl Write for CappedArchiveBuffer {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if buffer.len() > self.max_bytes.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "serialized recovery manifest exceeds its size bound",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialize_recovery_manifest(
    manifest: &impl serde::Serialize,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let mut buffer = CappedArchiveBuffer {
        bytes: Vec::new(),
        max_bytes,
    };
    serde_json::to_writer_pretty(&mut buffer, manifest).map_err(|error| {
        format!("cannot serialize recovery manifest within its size bound: {error}")
    })?;
    Ok(buffer.bytes)
}

/// Copy at most the byte length captured by the reviewed plan, then read one
/// extra byte to detect growth without writing beyond that plan.
#[cfg(test)]
fn copy_expected_bytes_with_probe<R: Read, W: Write>(
    source: &mut R,
    target: &mut W,
    expected_bytes: u64,
) -> std::io::Result<String> {
    copy_expected_bytes_with_probe_until(source, target, expected_bytes, None)
}

fn copy_expected_bytes_with_probe_until<R: Read, W: Write>(
    source: &mut R,
    target: &mut W,
    expected_bytes: u64,
    deadline: Option<Instant>,
) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut remaining = expected_bytes;
    while remaining > 0 {
        check_archive_deadline_io(deadline)?;
        let limit = remaining.min(buffer.len() as u64) as usize;
        let read = source.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "source is shorter than its reviewed byte length",
            ));
        }
        target.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }

    check_archive_deadline_io(deadline)?;
    let mut probe = [0_u8; 1];
    if source.read(&mut probe)? != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "source grew beyond its reviewed byte length",
        ));
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn copy_regular_archive_file_until(
    source: &Path,
    archive_root: &Path,
    relative: &str,
    expected: &ArchivedFile,
    deadline: Option<Instant>,
) -> Result<(), String> {
    check_archive_deadline(deadline, "recovery archive file copy")?;
    let (mut source_file, source_metadata) =
        open_regular_file_no_follow(source).map_err(|error| {
            format!(
                "cannot open {} without following links: {error}",
                source.display()
            )
        })?;
    let mode = permission_mode(&source_metadata);
    if mode != expected.mode {
        return Err(format!("{} changed mode after review", expected.path));
    }
    let relative_path = Path::new(relative);
    let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
    let parent = ensure_archive_directory(archive_root, parent)?;
    let name = relative_path
        .file_name()
        .ok_or_else(|| format!("archive file path has no name: {relative:?}"))?;
    let target = parent.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut target_file = options
        .open(&target)
        .map_err(|error| format!("cannot create archive file {}: {error}", target.display()))?;
    let copied_sha256 = copy_expected_bytes_with_probe_until(
        &mut source_file,
        &mut target_file,
        expected.bytes,
        deadline,
    )
    .map_err(|error| format!("cannot copy {}: {error}", expected.path))?;
    let source_after = source_file
        .metadata()
        .map_err(|error| format!("cannot recheck {}: {error}", expected.path))?;
    if source_after.len() != expected.bytes
        || permission_mode(&source_after) != expected.mode
        || copied_sha256 != expected.sha256
    {
        return Err(format!("{} changed while it was archived", expected.path));
    }
    #[cfg(unix)]
    if let Some(mode) = expected.mode {
        use std::os::unix::fs::PermissionsExt;
        target_file
            .set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|error| format!("cannot preserve mode for {}: {error}", expected.path))?;
    }
    target_file
        .sync_all()
        .map_err(|error| format!("cannot sync {}: {error}", expected.path))?;
    if permission_mode(
        &target_file
            .metadata()
            .map_err(|error| format!("cannot verify mode for {}: {error}", expected.path))?,
    ) != expected.mode
    {
        return Err(format!("{} mode did not copy intact", expected.path));
    }
    Ok(())
}

fn archive_staging_nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = ARCHIVE_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{:x}-{sequence:x}", std::process::id())
}

fn sync_archive_directory(_path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        options.open(_path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "directory durability sync is unsupported on this platform",
        ))
    }
}

#[cfg(test)]
fn sync_archive_directories_bottom_up<S>(root: &Path, sync_directory: &S) -> Result<(), String>
where
    S: Fn(&Path) -> std::io::Result<()>,
{
    sync_archive_directories_bottom_up_until(root, sync_directory, None)
}

fn sync_archive_directories_bottom_up_until<S>(
    root: &Path,
    sync_directory: &S,
    deadline: Option<Instant>,
) -> Result<(), String>
where
    S: Fn(&Path) -> std::io::Result<()>,
{
    let deadline = effective_archive_deadline(deadline);
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    let mut directories = Vec::new();
    let mut visited = 0_usize;
    while let Some((directory, depth)) = pending.pop() {
        check_archive_deadline(deadline, "archive durability scan")?;
        if depth > MAX_RECOVERY_ARCHIVE_TREE_DEPTH {
            return Err("archive directory depth exceeds the durability bound".into());
        }
        let metadata = std::fs::symlink_metadata(&directory).map_err(|error| {
            format!(
                "cannot inspect archive directory {}: {error}",
                directory.display()
            )
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "archive component is not a real directory: {}",
                directory.display()
            ));
        }
        directories.push((depth, directory.clone()));
        let entries = std::fs::read_dir(&directory).map_err(|error| {
            format!(
                "cannot read archive directory {}: {error}",
                directory.display()
            )
        })?;
        for entry in entries {
            check_archive_deadline(deadline, "archive durability scan")?;
            let entry = entry.map_err(|error| format!("cannot read archive entry: {error}"))?;
            visited += 1;
            if visited > MAX_RECOVERY_ARCHIVE_TREE_ENTRIES {
                return Err("archive entry count exceeds the durability bound".into());
            }
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                format!("cannot inspect archive entry {}: {error}", path.display())
            })?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                pending.push((path, depth + 1));
            } else if !metadata.is_file() && !metadata.file_type().is_symlink() {
                return Err(format!(
                    "archive contains an unsupported filesystem node: {}",
                    path.display()
                ));
            }
        }
    }
    directories.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
    for (_, directory) in directories {
        check_archive_deadline(deadline, "archive durability sync")?;
        sync_directory(&directory).map_err(|error| {
            format!(
                "cannot sync archive directory {}: {error}",
                directory.display()
            )
        })?;
    }
    Ok(())
}

/// Write every part into a fresh, real staging directory, then read each part
/// back. `Err` carries the first failure; the caller keeps the worktree.
#[cfg(test)]
fn write_verified_archive(
    repository: &GitRepo,
    checkout: &GitRepo,
    plan: &CleanupResolvePlan,
    task: Option<&str>,
    dir: &Path,
) -> Result<(), String> {
    write_verified_archive_with_directory_sync(
        repository,
        checkout,
        plan,
        task,
        dir,
        &sync_archive_directory,
    )
}

#[cfg(test)]
fn write_verified_archive_with_directory_sync<S>(
    repository: &GitRepo,
    checkout: &GitRepo,
    plan: &CleanupResolvePlan,
    task: Option<&str>,
    dir: &Path,
    sync_directory: &S,
) -> Result<(), String>
where
    S: Fn(&Path) -> std::io::Result<()>,
{
    write_verified_archive_with_bundle_writer(
        repository,
        checkout,
        plan,
        task,
        dir,
        &RecoveryBundleWriter {
            max_bytes: MAX_RECOVERY_BUNDLE_BYTES,
            write_to: write_recovery_bundle_to_file,
        },
        sync_directory,
        None,
    )
}

struct RecoveryBundleWriter<B> {
    max_bytes: u64,
    write_to: B,
}

// The explicit paths, timing bound, sync hook, and bundle writer are the
// narrow fault-injection seam used to verify the durable archive protocol.
#[allow(clippy::too_many_arguments)]
fn write_verified_archive_with_bundle_writer<S, B>(
    repository: &GitRepo,
    checkout: &GitRepo,
    plan: &CleanupResolvePlan,
    task: Option<&str>,
    dir: &Path,
    bundle: &RecoveryBundleWriter<B>,
    sync_directory: &S,
    deadline: Option<Instant>,
) -> Result<(), String>
where
    S: Fn(&Path) -> std::io::Result<()>,
    B: Fn(&GitRepo, &[String], &[String], u64, Instant, &mut std::fs::File) -> Result<u64, String>,
{
    check_archive_deadline(deadline, "recovery archive write")?;
    require_real_directory(dir)?;
    let dir = dir
        .canonicalize()
        .map_err(|error| format!("cannot resolve archive staging directory: {error}"))?;
    require_real_directory(&dir)?;
    let (retirement_record_sha256, archive_created_at_ms, prerequisite_refs) =
        if is_retirement_archive_schema(plan.recovery_archive_schema_version) {
            let (record, bytes) = read_retirement_archive_record(&dir)?;
            let expected = retirement_archive_record_for_plan(plan, record.archive_created_at_ms)?;
            if record != expected {
                return Err("retirement staging record does not match the reviewed plan".into());
            }
            (
                Some(sha256_hex(&bytes)),
                record.archive_created_at_ms,
                record.prerequisite_refs,
            )
        } else {
            (None, now_ms(), None)
        };

    // Patches are taken again rather than trusted from the plan: the digest
    // check just passed, and a patch that no longer hashes the same means the
    // worktree moved in between.
    let mut patches = Vec::new();
    for (cached, file, expected) in [
        (true, STAGED_PATCH_FILE, &plan.staged_patch_sha256),
        (false, UNSTAGED_PATCH_FILE, &plan.unstaged_patch_sha256),
    ] {
        check_archive_deadline(deadline, "recovery patch capture")?;
        let bytes = checkout
            .recovery_patch(cached)
            .map_err(|error| format!("cannot take {file}: {error}"))?;
        let path = create_new_archive_file(&dir, file, &bytes)?;
        let written = describe_untracked_until(&path, file, deadline)
            .map_err(|error| format!("cannot safely read {file}: {error}"))?
            .ok_or_else(|| format!("{file} is not a regular file"))?;
        if written.bytes != bytes.len() as u64 || written.sha256 != *expected {
            return Err(format!("{file} no longer matches the reviewed plan"));
        }
        patches.push((written.bytes != 0).then_some(path));
    }

    check_archive_deadline(deadline, "recovery bundle preparation")?;
    let bundle_integrity = if !recovery_bundle_required(plan) {
        None
    } else {
        let bundle_deadline = recovery_bundle_deadline()?;
        let bundle_deadline =
            deadline.map_or(bundle_deadline, |operation| operation.min(bundle_deadline));
        let (path, mut bundle_file) = create_new_archive_file_handle(&dir, BUNDLE_FILE)?;
        let additional_roots = recovery_bundle_refs(plan);
        let streamed_bytes = match (bundle.write_to)(
            checkout,
            &plan.delivery_targets,
            &additional_roots,
            bundle.max_bytes,
            bundle_deadline,
            &mut bundle_file,
        ) {
            Ok(bytes) => bytes,
            Err(reason) => {
                if let Err(sync_error) = bundle_file.sync_all() {
                    return Err(format!(
                        "{reason}; cannot sync partial {BUNDLE_FILE}: {sync_error}"
                    ));
                }
                return Err(reason);
            }
        };
        bundle_file
            .sync_all()
            .map_err(|error| format!("cannot sync {BUNDLE_FILE}: {error}"))?;
        drop(bundle_file);
        let (bytes, sha256) = hash_regular_file_no_follow_until(&path, bundle.max_bytes, deadline)
            .map_err(|error| format!("cannot read back {BUNDLE_FILE}: {error}"))?;
        if bytes != streamed_bytes {
            return Err(format!("{BUNDLE_FILE} byte count changed after streaming"));
        }
        Some(RecoveryBundleIntegrity { bytes, sha256 })
    };
    let bundle = bundle_integrity.as_ref().map(|_| BUNDLE_FILE);

    let scratch = tempfile::tempdir().map_err(|error| format!("no scratch index: {error}"))?;
    repository
        .check_recovery_patches(
            &plan.head,
            patches[0].as_deref(),
            patches[1].as_deref(),
            &scratch.path().join("index"),
        )
        .map_err(|error| format!("the patches do not apply to {}: {error}", plan.head))?;

    let worktree = Path::new(&plan.worktree_path);
    let untracked_root = create_exclusive_archive_directory(&dir, UNTRACKED_DIRECTORY)?;
    for file in &plan.untracked_files {
        check_archive_deadline(deadline, "untracked recovery copy")?;
        if !is_contained_relative(&file.path) {
            return Err(format!(
                "untracked archive path is invalid: {:?}",
                file.path
            ));
        }
        let source = worktree.join(&file.path);
        let relative_path = Path::new(&file.path);
        let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
        let parent = ensure_archive_directory(&untracked_root, parent)?;
        let target =
            parent.join(relative_path.file_name().ok_or_else(|| {
                format!("untracked archive path has no filename: {:?}", file.path)
            })?);
        if file.symlink {
            let link = std::fs::read_link(&source)
                .map_err(|error| format!("cannot read link {}: {error}", file.path))?;
            std::os::unix::fs::symlink(link, &target)
                .map_err(|error| format!("cannot preserve link {}: {error}", file.path))?;
        } else {
            copy_regular_archive_file_until(&source, &untracked_root, &file.path, file, deadline)?;
        }
        let archived = describe_untracked_until(&target, &file.path, deadline)
            .map_err(|error| format!("cannot read back {}: {error}", file.path))?;
        if archived.as_ref() != Some(file) {
            return Err(format!(
                "{} changed while it was archived, or did not copy intact",
                file.path
            ));
        }
    }

    let ignored_root = create_exclusive_archive_directory(&dir, IGNORED_DIRECTORY)?;
    for file in &plan.ignored_files {
        check_archive_deadline(deadline, "ignored recovery copy")?;
        if !is_contained_relative(&file.path) {
            return Err(format!("ignored archive path is invalid: {:?}", file.path));
        }
        let source = worktree.join(&file.path);
        let relative_path = Path::new(&file.path);
        let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
        let parent = ensure_archive_directory(&ignored_root, parent)?;
        let target = parent.join(
            relative_path
                .file_name()
                .ok_or_else(|| format!("ignored archive path has no filename: {:?}", file.path))?,
        );
        if file.symlink {
            let link = std::fs::read_link(&source)
                .map_err(|error| format!("cannot read ignored link {}: {error}", file.path))?;
            std::os::unix::fs::symlink(link, &target)
                .map_err(|error| format!("cannot preserve ignored link {}: {error}", file.path))?;
        } else {
            copy_regular_archive_file_until(&source, &ignored_root, &file.path, file, deadline)?;
        }
        let archived = describe_untracked_until(&target, &file.path, deadline)
            .map_err(|error| format!("cannot read back ignored {}: {error}", file.path))?;
        if archived.as_ref() != Some(file) {
            return Err(format!(
                "ignored {} changed while it was archived, or did not copy intact",
                file.path
            ));
        }
    }

    check_archive_deadline(deadline, "recovery manifest write")?;
    let manifest_root_proofs_sha256 =
        if is_retirement_archive_schema(plan.recovery_archive_schema_version) {
            Some(recovery_root_proofs_sha256(&plan.recovery_root_proofs)?)
        } else {
            None
        };
    let manifest_delivery_targets_sha256 =
        if is_retirement_archive_schema(plan.recovery_archive_schema_version) {
            Some(delivery_targets_sha256(&plan.delivery_targets)?)
        } else {
            None
        };
    let manifest = RecoveryManifest {
        schema_version: plan.recovery_archive_schema_version,
        plan_digest: &plan.digest,
        created_at_ms: archive_created_at_ms,
        minimum_archive_retention_ms: if is_retirement_archive_schema(
            plan.recovery_archive_schema_version,
        ) {
            plan.minimum_archive_retention_ms
        } else {
            None
        },
        retirement_record_sha256: retirement_record_sha256.as_deref(),
        aethyme_version: env!("CARGO_PKG_VERSION"),
        repository: repository.root().to_string_lossy().into_owned(),
        session_id: plan.session_id,
        task,
        worktree_path: &plan.worktree_path,
        branch_ref: &plan.branch_ref,
        branch_tip: plan.branch_tip.as_deref(),
        checkout_branch_ref: is_retirement_archive_schema(plan.recovery_archive_schema_version)
            .then_some(plan.checkout_branch_ref.as_deref())
            .flatten(),
        checkout_branch_tip: is_retirement_archive_schema(plan.recovery_archive_schema_version)
            .then_some(plan.checkout_branch_tip.as_deref())
            .flatten(),
        recovery_root_proofs_sha256: manifest_root_proofs_sha256.as_deref(),
        recovery_root_proofs: is_retirement_archive_schema(plan.recovery_archive_schema_version)
            .then_some(plan.recovery_root_proofs.as_slice()),
        delivery_targets_sha256: manifest_delivery_targets_sha256.as_deref(),
        prerequisite_refs: (plan.recovery_archive_schema_version
            == PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION)
            .then_some(prerequisite_refs.as_deref())
            .flatten(),
        disposition: plan.disposition.as_str(),
        head: &plan.head,
        base: repository
            .head_commit()
            .ok()
            .and_then(|primary| crate::landing_base(repository, &plan.head, &primary).ok()),
        delivery_targets: &plan.delivery_targets,
        landing: &plan.landing,
        bundle,
        bundle_bytes: bundle_integrity.as_ref().map(|integrity| integrity.bytes),
        bundle_sha256: bundle_integrity
            .as_ref()
            .map(|integrity| integrity.sha256.as_str()),
        unlanded_commits: &plan.unlanded_commits,
        staged_patch: PatchRecord {
            file: STAGED_PATCH_FILE,
            bytes: plan.staged_patch_bytes,
            sha256: &plan.staged_patch_sha256,
        },
        unstaged_patch: PatchRecord {
            file: UNSTAGED_PATCH_FILE,
            bytes: plan.unstaged_patch_bytes,
            sha256: &plan.unstaged_patch_sha256,
        },
        untracked_directory: UNTRACKED_DIRECTORY,
        untracked_files: &plan.untracked_files,
        ignored_directory: IGNORED_DIRECTORY,
        ignored_paths: &plan.ignored_paths,
        ignored_files: &plan.ignored_files,
        ignored_policy_sha256: &plan.ignored_policy_sha256,
    };
    let manifest = serialize_recovery_manifest(&manifest, MAX_RECOVERY_MANIFEST_BYTES as usize)?;
    create_new_archive_file(&dir, MANIFEST_FILE, &manifest)?;
    let readme = restore_readme(plan, bundle.is_some());
    if readme.len() as u64 > MAX_RECOVERY_README_BYTES {
        return Err(format!(
            "restore README exceeds the {MAX_RECOVERY_README_BYTES}-byte archive size bound"
        ));
    }
    create_new_archive_file(&dir, README_FILE, readme.as_bytes())?;
    verify_recovery_archive_payload_for_plan(repository, &dir, plan)?;
    check_archive_deadline(deadline, "recovery archive durability sync")?;
    sync_archive_directories_bottom_up_until(&dir, sync_directory, deadline)?;
    Ok(())
}

/// Restore steps written into the archive, specific to what it holds.
fn restore_readme(plan: &CleanupResolvePlan, has_bundle: bool) -> String {
    let checkout = if has_bundle {
        format!(
            "   git fetch <this-archive>/{BUNDLE_FILE} HEAD\n   git switch --detach FETCH_HEAD   # {}\n",
            plan.head
        )
    } else {
        format!(
            "   git switch --detach {}   # already on a delivery target\n",
            plan.head
        )
    };
    let branch_roots_note = if is_retirement_archive_schema(plan.recovery_archive_schema_version) {
        let recorded_tip = plan.branch_tip.as_deref().unwrap_or("unknown");
        let checkout_branch = plan
            .checkout_branch_ref
            .as_deref()
            .unwrap_or("detached checkout");
        let checkout_tip = plan.checkout_branch_tip.as_deref().unwrap_or("unknown");
        format!(
            "`HEAD` is the captured checkout. The manifest records the session branch `{}` at `{recorded_tip}` and the branch actually checked out `{checkout_branch}` at `{checkout_tip}`. If those identities differ, retirement preserves the distinct branch refs; restore the intended root explicitly instead of substituting one for another.\n\n",
            plan.branch_ref
        )
    } else {
        String::new()
    };
    format!(
        "# Recovery archive for broker session {id}\n\n\
         {branch_roots_note}\
         Written by `aethyme broker finish cleanup resolve {id} --archive` before the worktree\n\
         `{path}` was removed. `{MANIFEST_FILE}` lists every part with its SHA-256.\n\
         Policy-approved ignored output was not archived; reinstall generated dependencies if needed.\n\
         Protected standalone ignored paths are listed in `manifest.json` and stored in `{IGNORED_DIRECTORY}/`.\n\n\
         To restore into a NEW checkout of the same repository (never over a checkout in use):\n\n\
         1. Check out the session head:\n{checkout}\
         2. Re-apply the staged changes, then the unstaged ones (skip an empty patch):\n\
            git apply --index <this-archive>/{STAGED_PATCH_FILE}\n\
            git apply <this-archive>/{UNSTAGED_PATCH_FILE}\n\
         3. Copy back the untracked files and their permission bits:\n\
            cp -pR <this-archive>/{UNTRACKED_DIRECTORY}/. .\n\
         4. Copy back protected ignored files and links, if present:\n\
            cp -pR <this-archive>/{IGNORED_DIRECTORY}/. .\n",
        id = plan.session_id,
        path = plan.worktree_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_bundled_root_skips_even_a_failing_delivery_probe() {
        let roots = [("refs/heads/session".into(), "session-oid".into())].into();
        assert_eq!(
            recovery_root_is_available("refs/heads/session", "session-oid", &roots, || {
                panic!("an exact bundled root must not need a delivery ancestry probe")
            }),
            Ok(true)
        );
    }

    #[test]
    fn bundled_root_fast_path_requires_both_ref_and_object_identity() {
        let roots = [("refs/heads/session".into(), "session-oid".into())].into();
        for (reference, object_id) in [
            ("refs/heads/other", "session-oid"),
            ("refs/heads/session", "different-oid"),
        ] {
            let calls = std::cell::Cell::new(0);
            assert_eq!(
                recovery_root_is_available(reference, object_id, &roots, || {
                    calls.set(calls.get() + 1);
                    Ok(false)
                }),
                Ok(false)
            );
            assert_eq!(calls.get(), 1);
        }
    }

    #[test]
    fn unbundled_root_preserves_delivery_success_and_probe_failure() {
        let roots = std::collections::BTreeSet::new();
        assert_eq!(
            recovery_root_is_available("ref", "oid", &roots, || Ok(true)),
            Ok(true)
        );
        assert_eq!(
            recovery_root_is_available("ref", "oid", &roots, || Err("Git unavailable".into())),
            Err("Git unavailable".into())
        );
    }

    #[test]
    fn plan_ancestry_memo_caches_verdicts_but_retries_errors() {
        let memo = PlanAncestryMemo::default();
        let errors = std::cell::Cell::new(0);
        for _ in 0..2 {
            let result = memo.check_with("ancestor", "target-error", || {
                errors.set(errors.get() + 1);
                Err("temporary Git failure".into())
            });
            assert_eq!(result, Err("temporary Git failure".into()));
        }
        assert_eq!(errors.get(), 2);

        let negatives = std::cell::Cell::new(0);
        for _ in 0..2 {
            assert_eq!(
                memo.check_with("ancestor", "target-negative", || {
                    negatives.set(negatives.get() + 1);
                    Ok(false)
                }),
                Ok(false)
            );
        }
        assert_eq!(negatives.get(), 1);

        let positives = std::cell::Cell::new(0);
        for _ in 0..2 {
            assert_eq!(
                memo.check_with("ancestor", "target-positive", || {
                    positives.set(positives.get() + 1);
                    Ok(true)
                }),
                Ok(true)
            );
        }
        assert_eq!(positives.get(), 1);
    }

    #[cfg(unix)]
    fn write_over_cap_bundle(
        _checkout: &GitRepo,
        _delivery_targets: &[String],
        _additional_roots: &[String],
        max_bytes: u64,
        deadline: Instant,
        file: &mut std::fs::File,
    ) -> Result<u64, String> {
        let mut command = Command::new("sh");
        command.args(["-c", "printf 123456789"]);
        stream_recovery_command_stdout(&mut command, file, max_bytes, deadline)
    }

    #[cfg(unix)]
    fn write_invalid_bundle(
        _checkout: &GitRepo,
        _delivery_targets: &[String],
        _additional_roots: &[String],
        _max_bytes: u64,
        _deadline: Instant,
        file: &mut std::fs::File,
    ) -> Result<u64, String> {
        const INVALID_BUNDLE: &[u8] = b"not a Git bundle\n";
        file.write_all(INVALID_BUNDLE)
            .map_err(|error| format!("cannot write invalid test bundle: {error}"))?;
        Ok(INVALID_BUNDLE.len() as u64)
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn isolated_git_output(dir: &Path, args: &[&str]) -> std::process::Output {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(dir)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_GRAFT_FILE");
        for name in [
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_OBJECT_DIRECTORY_RELATIVE",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
            "GIT_REPLACE_REF_BASE",
            "GIT_SHALLOW_FILE",
            "GIT_CEILING_DIRECTORIES",
            "GIT_DISCOVERY_ACROSS_FILESYSTEM",
            "GIT_PREFIX",
            "GIT_CONFIG",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
        ] {
            command.env_remove(name);
        }
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_") {
                command.env_remove(name.as_ref());
            }
        }
        command
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    }

    fn git_output(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn copy_archive_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
        std::fs::create_dir(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            let metadata = std::fs::symlink_metadata(&source_path)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                copy_archive_tree(&source_path, &destination_path)?;
            } else if metadata.is_file() && !metadata.file_type().is_symlink() {
                std::fs::copy(&source_path, &destination_path)?;
            } else {
                return Err(std::io::Error::other(format!(
                    "fixture archive contains a non-regular entry: {}",
                    source_path.display()
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn schema_v5_proofs_bundle_a_unique_recorded_root_even_when_head_is_delivered() {
        let (_tmp, repository, mut plan) = staged_checkout();
        let head = repository.head_commit().unwrap();
        let tree = format!("{head}^{{tree}}");
        let unique_branch_tip = git_output(
            repository.root(),
            &[
                "commit-tree",
                &tree,
                "-p",
                &head,
                "-m",
                "session-only commit",
            ],
        );
        plan.recovery_archive_schema_version = RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION;
        plan.head = head.clone();
        plan.delivery_targets = vec![head.clone()];
        plan.unlanded_commits.clear();
        plan.branch_ref = "refs/heads/recorded-session".into();
        git(
            repository.root(),
            &["update-ref", &plan.branch_ref, &unique_branch_tip],
        );
        plan.branch_tip = Some(unique_branch_tip.clone());
        plan.checkout_branch_ref = Some("refs/heads/main".into());
        plan.checkout_branch_tip = Some(head.clone());
        plan.recovery_root_proofs = recovery_root_proofs_for_plan_parts(
            &repository,
            &head,
            &plan.branch_ref,
            plan.branch_tip.as_deref(),
            (
                plan.checkout_branch_ref.as_deref(),
                plan.checkout_branch_tip.as_deref(),
            ),
            &plan.delivery_targets,
            &PlanAncestryMemo::for_delivery_targets(&plan.delivery_targets, None),
        )
        .unwrap();

        assert!(
            plan.recovery_root_proofs
                .iter()
                .find(|root| root.ref_name == "HEAD")
                .unwrap()
                .available_from_delivery
        );
        assert!(
            !plan
                .recovery_root_proofs
                .iter()
                .find(|root| root.ref_name == "refs/heads/recorded-session")
                .unwrap()
                .available_from_delivery
        );
        assert_eq!(
            recovery_bundle_roots(&plan),
            vec![("refs/heads/recorded-session".into(), unique_branch_tip)]
        );
    }

    #[test]
    fn exact_delivery_root_is_checked_before_unrelated_failed_history() {
        let (_tmp, repo, plan) = staged_checkout();
        let mut targets = vec!["missing-delivery-ref".to_owned(); 1_000];
        targets.push(plan.head.clone());
        assert!(root_available_from_delivery(&repo, &plan.head, &targets).unwrap());
    }

    #[test]
    fn exact_delivery_root_is_preferred_by_plan_ancestry_memo() {
        let (_tmp, repo, plan) = staged_checkout();
        let targets = vec!["missing-delivery-ref".to_owned(), plan.head.clone()];
        let memo = PlanAncestryMemo::for_delivery_targets(&targets, None);
        assert!(
            root_available_from_delivery_during_plan(&repo, &plan.head, &targets, &memo).unwrap()
        );
        let results = memo.results.borrow();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results.get(&(plan.head.clone(), plan.head.clone())),
            Some(&true)
        );
    }

    #[test]
    fn exact_delivery_match_still_requires_a_real_git_object() {
        let (_tmp, repo, _plan) = staged_checkout();
        let absent = "0".repeat(40);
        let targets = vec![absent.clone()];
        assert!(root_available_from_delivery(&repo, &absent, &targets).is_err());
        assert!(
            root_available_from_delivery_during_plan(
                &repo,
                &absent,
                &targets,
                &PlanAncestryMemo::default(),
            )
            .is_err()
        );
        assert!(validate_delivery_prerequisites_remain_current(&repo, &targets, &targets).is_err());
    }

    /// A checkout with one staged edit and one untracked file, and the plan
    /// that describes it exactly.
    fn staged_checkout() -> (tempfile::TempDir, GitRepo, CleanupResolvePlan) {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("a.txt"), "one\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);
        git(tmp.path(), &["commit", "-qm", "init"]);
        std::fs::write(tmp.path().join("a.txt"), "two\n").unwrap();
        git(tmp.path(), &["add", "a.txt"]);
        std::fs::write(tmp.path().join("new.txt"), "untracked\n").unwrap();
        let repo = GitRepo::discover(tmp.path()).unwrap();
        let head = repo.head_commit().unwrap();
        let staged = repo.recovery_patch(true).unwrap();
        let unstaged = repo.recovery_patch(false).unwrap();
        let untracked = describe_untracked(&tmp.path().join("new.txt"), "new.txt")
            .unwrap()
            .unwrap();
        let plan = CleanupResolvePlan {
            schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
            digest: sha256_hex(b"test reviewed plan"),
            session_id: 1,
            target_origin: SessionOrigin::Spawned,
            recovery_archive_schema_version: RECOVERY_ARCHIVE_SCHEMA_VERSION,
            minimum_archive_retention_ms: None,
            retirement_allow_adopted: None,
            retirement_witness_sha256: None,
            repository_path: repo.root().to_string_lossy().into_owned(),
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            branch_ref: "refs/heads/main".into(),
            branch_tip: None,
            checkout_branch_ref: None,
            checkout_branch_tip: None,
            recovery_root_proofs: Vec::new(),
            disposition: CleanupDisposition::Dirty,
            reason: String::new(),
            head: head.clone(),
            delivery_targets: vec![head],
            landing: RecoveryLandingAssessment::Verdict(crate::LandingVerdict::NotLanded {
                examined: 0,
                truncated: false,
            }),
            unlanded_commits: Vec::new(),
            staged_patch_bytes: staged.len() as u64,
            staged_patch_sha256: sha256_hex(&staged),
            unstaged_patch_bytes: unstaged.len() as u64,
            unstaged_patch_sha256: sha256_hex(&unstaged),
            untracked_bytes: untracked.bytes,
            untracked_files: vec![untracked],
            ignored_paths: Vec::new(),
            ignored_files: Vec::new(),
            ignored_bytes: 0,
            ignored_policy_sha256: sha256_hex(b"{}"),
            archive_root: PathBuf::new(),
            apply_command: String::new(),
            discard_command: String::new(),
        };
        (tmp, repo, plan)
    }

    fn closed_unique_commit_fixture(
        task: &str,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        crate::test_storage::TestStorage,
        Broker,
        crate::types::Session,
        PathBuf,
        String,
        String,
        String,
    ) {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);
        let base = GitRepo::discover(&repository)
            .unwrap()
            .head_commit()
            .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker.start_worktree(task, None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("unique.txt"), "session commit\n").unwrap();
        git(&worktree, &["add", "unique.txt"]);
        git(&worktree, &["commit", "-qm", "unique session content"]);
        std::fs::write(worktree.join("preserve.txt"), "uncommitted recovery data\n").unwrap();
        let head = GitRepo::discover(&worktree).unwrap().head_commit().unwrap();
        let branch_ref = format!("refs/heads/{}", session.branch);
        broker.close(session.id).unwrap();
        (
            owner, repository, storage, broker, session, worktree, base, head, branch_ref,
        )
    }

    #[test]
    fn recovery_archive_skips_exact_landing_search_and_records_explicit_state() {
        let (_owner, _repository, _storage, mut broker, session, worktree, base, head, _branch) =
            closed_unique_commit_fixture("recovery archive avoids landing scan");
        std::fs::remove_file(worktree.join("preserve.txt")).unwrap();
        let control_before = crate::representation::test_landing_search_calls();
        crate::work_landed(broker.repo_handle(), &head, &base).unwrap();
        assert!(crate::representation::test_landing_search_calls() > control_before);
        let calls_before = crate::representation::test_landing_search_calls();
        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        assert_eq!(
            crate::representation::test_landing_search_calls(),
            calls_before
        );
        assert!(!plan.unlanded_commits.is_empty());
        assert_eq!(plan.head, head);
        assert!(matches!(
            &plan.landing,
            RecoveryLandingAssessment::NotEvaluatedForRecovery { .. }
        ));
        assert_eq!(
            serde_json::to_value(&plan.landing).unwrap()["state"],
            "not_evaluated_for_recovery"
        );

        let outcome = broker
            .cleanup_resolve_apply(session.id, &plan.digest)
            .unwrap();
        assert_eq!(
            crate::representation::test_landing_search_calls(),
            calls_before
        );
        assert!(outcome.worktree_removed, "{outcome:?}");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(outcome.archive.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(manifest["landing"]["state"], "not_evaluated_for_recovery");
        assert!(!worktree.exists());
    }

    #[test]
    fn target_fast_forward_before_apply_preserves_content_confirmation() {
        let (_owner, repository, _storage, mut broker, session, worktree, _base, _head, _branch) =
            closed_unique_commit_fixture("recovery archive target advances before apply");
        std::fs::remove_file(worktree.join("preserve.txt")).unwrap();
        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        git(&repository, &["merge", "--ff-only", &session.branch]);
        assert!(broker.cleanup_resolve_plan(session.id).is_err());
        let revalidated = broker
            .cleanup_resolve_revalidation_plan(session.id)
            .unwrap();
        assert_eq!(revalidated.disposition, CleanupDisposition::Eligible);
        assert_eq!(revalidated.digest, plan.digest);
        let outcome = broker
            .cleanup_resolve_apply(session.id, &plan.digest)
            .unwrap();
        assert!(outcome.worktree_removed, "{outcome:?}");
        assert!(!worktree.exists());
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(outcome.archive.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(manifest["disposition"], "eligible");
    }

    #[test]
    fn target_fast_forward_after_archive_keeps_original_evidence_and_allows_removal() {
        let (_owner, repository, _storage, mut broker, session, worktree, _base, head, _branch) =
            closed_unique_commit_fixture("recovery archive target fast-forward");
        std::fs::remove_file(worktree.join("preserve.txt")).unwrap();
        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        assert!(plan.unlanded_commits.contains(&head));
        let original_targets = plan.delivery_targets.clone();
        let repository_after_archive = repository.clone();
        let branch = session.branch.clone();
        let outcome = broker
            .cleanup_resolve_apply_with_post_archive_hook(session.id, &plan.digest, move || {
                git(&repository_after_archive, &["merge", "--ff-only", &branch]);
            })
            .unwrap();

        assert!(outcome.worktree_removed, "{outcome:?}");
        assert!(!worktree.exists());
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(outcome.archive.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            manifest["delivery_targets"],
            serde_json::json!(original_targets)
        );
        assert_eq!(
            manifest["unlanded_commits"],
            serde_json::json!(plan.unlanded_commits)
        );
        assert_eq!(manifest["landing"]["state"], "not_evaluated_for_recovery");
    }

    #[test]
    fn rewound_delivery_target_after_archive_refuses_removal() {
        let (_owner, repository, _storage, mut broker, session, worktree, base, head, branch_ref) =
            closed_unique_commit_fixture("recovery archive target rewind");
        std::fs::write(repository.join("main-only.txt"), "later target\n").unwrap();
        git(&repository, &["add", "main-only.txt"]);
        git(&repository, &["commit", "-qm", "later delivery target"]);
        let later_target = GitRepo::discover(&repository)
            .unwrap()
            .head_commit()
            .unwrap();
        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        assert!(plan.delivery_targets.contains(&later_target));
        assert!(plan.unlanded_commits.contains(&head));

        let repository_after_archive = repository.clone();
        let reset_to = base.clone();
        let outcome = broker
            .cleanup_resolve_apply_with_post_archive_hook(session.id, &plan.digest, move || {
                git(&repository_after_archive, &["reset", "--hard", &reset_to]);
            })
            .unwrap();

        assert!(!outcome.worktree_removed, "{outcome:?}");
        assert!(outcome.cleanup_error.is_some(), "{outcome:?}");
        assert!(worktree.is_dir());
        assert_eq!(
            GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref(&branch_ref)
                .as_deref(),
            Some(head.as_str())
        );
        assert_eq!(
            std::fs::read(worktree.join("preserve.txt")).unwrap(),
            b"uncommitted recovery data\n"
        );
    }

    #[test]
    fn branch_tip_and_head_change_after_archive_keeps_worktree() {
        let (_owner, _repository, _storage, mut broker, session, worktree, _base, _head, _branch) =
            closed_unique_commit_fixture("recovery archive branch-tip change");
        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        let changed_worktree = worktree.clone();
        let outcome = broker
            .cleanup_resolve_apply_with_post_archive_hook(session.id, &plan.digest, move || {
                std::fs::write(changed_worktree.join("later.txt"), "new commit\n").unwrap();
                git(&changed_worktree, &["add", "later.txt"]);
                git(
                    &changed_worktree,
                    &["commit", "-qm", "change after archive"],
                );
            })
            .unwrap();

        assert!(!outcome.worktree_removed, "{outcome:?}");
        assert!(outcome.cleanup_error.is_some(), "{outcome:?}");
        assert!(worktree.is_dir());
        assert_eq!(
            std::fs::read(worktree.join("later.txt")).unwrap(),
            b"new commit\n"
        );
    }

    #[test]
    fn vanished_worktree_does_not_delete_a_moved_session_branch() {
        let (_owner, repository, _storage, mut broker, session, worktree, base, _head, branch_ref) =
            closed_unique_commit_fixture("recovery archive vanished checkout");
        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        let repository_after_archive = repository.clone();
        let branch_ref_after_archive = branch_ref.clone();
        let worktree_after_archive = worktree.clone();
        let moved_tip = base.clone();
        let outcome = broker
            .cleanup_resolve_apply_with_post_archive_hook(session.id, &plan.digest, move || {
                git(
                    &repository_after_archive,
                    &["update-ref", &branch_ref_after_archive, &moved_tip],
                );
                std::fs::remove_dir_all(&worktree_after_archive).unwrap();
            })
            .unwrap();

        assert!(!outcome.worktree_removed, "{outcome:?}");
        assert!(outcome.cleanup_error.is_some(), "{outcome:?}");
        assert!(outcome.archive.join(MANIFEST_FILE).is_file());
        assert!(!worktree.exists());
        assert_eq!(
            GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref(&branch_ref)
                .as_deref(),
            Some(base.as_str())
        );
    }

    #[test]
    fn branch_tip_mismatch_refuses_archive_without_dropping_branch_commit() {
        let (_owner, repository, _storage, broker, session, worktree, base, head, branch_ref) =
            closed_unique_commit_fixture("recovery archive mismatched branch tip");
        git(&worktree, &["checkout", "--detach", &base]);
        let error = broker.cleanup_resolve_plan(session.id).unwrap_err();
        let BrokerOpError::CleanupResolveRefused { id, reason } = error else {
            panic!("expected identity refusal, got {error:?}");
        };
        assert_eq!(id, session.id);
        assert!(
            reason.contains("registered session branch")
                && reason.contains("differs from the current worktree branch/HEAD")
                && reason.contains(&branch_ref)
                && reason.contains(&head)
                && reason.contains(&base)
                && reason.contains("(detached)"),
            "{reason}"
        );
        assert!(worktree.is_dir());
        assert_eq!(
            GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref(&branch_ref)
                .as_deref(),
            Some(head.as_str())
        );
    }

    #[test]
    fn incomplete_bounded_untracked_inventory_refuses_archive_plan_and_keeps_source() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        git(
            &repository,
            &["config", "user.name", "Archive inventory test"],
        );
        git(
            &repository,
            &["config", "user.email", "archive-inventory@example.invalid"],
        );
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "README.md"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("bounded archive inventory", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        broker.close(session.id).unwrap();
        let first = worktree.join("untracked-one.txt");
        let second = worktree.join("untracked-two.txt");
        std::fs::write(&first, "keep one\n").unwrap();
        std::fs::write(&second, "keep two\n").unwrap();

        let error = broker
            .cleanup_resolve_plan_with_limits(session.id, 1024 * 1024, 16, 1024 * 1024, 1)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("could not completely list untracked archive paths"),
            "an incomplete path inventory must refuse the archive plan: {error}"
        );
        assert!(
            worktree.is_dir(),
            "an incomplete inventory removed the checkout"
        );
        assert_eq!(std::fs::read(first).unwrap(), b"keep one\n");
        assert_eq!(std::fs::read(second).unwrap(), b"keep two\n");
        assert!(
            !broker.recovery_archive_root().unwrap().exists(),
            "planning with an incomplete inventory must not create an archive"
        );
    }

    #[test]
    fn a_retirement_archive_is_not_landed_until_every_root_is_delivered() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join("README.md"), "base\n").unwrap();
        git(&repository, &["add", "README.md"]);
        git(&repository, &["commit", "-qm", "initial"]);
        let delivered_head = GitRepo::discover(&repository)
            .unwrap()
            .head_commit()
            .unwrap();

        git(&repository, &["checkout", "-qb", "session-root"]);
        std::fs::write(repository.join("private.txt"), "unique branch history\n").unwrap();
        git(&repository, &["add", "private.txt"]);
        git(&repository, &["commit", "-qm", "unique branch commit"]);
        let undelivered_branch_root = GitRepo::discover(&repository)
            .unwrap()
            .head_commit()
            .unwrap();
        git(&repository, &["checkout", "main"]);

        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&repository).unwrap();
        let archive = |recovery_roots| OwnedRecoveryArchive {
            path: PathBuf::new(),
            session_id: 9,
            head: delivered_head.clone(),
            recovery_roots,
            created_at_ms: now_ms(),
            minimum_archive_retention_ms: Some(MIN_RECOVERY_ARCHIVE_RETENTION_MS),
            prerequisite_refs: Vec::new(),
            retirement_digest: None,
            prerequisite_targets: Vec::new(),
            holds_uncommitted: false,
        };

        let retirement_archive = archive(vec![
            RecoveryRootProof {
                ref_name: "HEAD".into(),
                object_id: delivered_head.clone(),
                available_from_delivery: true,
            },
            RecoveryRootProof {
                ref_name: "refs/heads/session-root".into(),
                object_id: undelivered_branch_root,
                available_from_delivery: false,
            },
        ]);
        assert!(!broker.recovery_archive_landed(&retirement_archive));

        let legacy_archive = archive(vec![RecoveryRootProof {
            ref_name: "HEAD".into(),
            object_id: delivered_head.clone(),
            available_from_delivery: true,
        }]);
        assert!(broker.recovery_archive_landed(&legacy_archive));
    }

    fn interrupt_retirement_bundle_write(
        _checkout: &GitRepo,
        _delivery_targets: &[String],
        _additional_roots: &[String],
        _max_bytes: u64,
        _deadline: Instant,
        file: &mut std::fs::File,
    ) -> Result<u64, String> {
        file.write_all(b"interrupted bundle staging")
            .map_err(|error| format!("cannot write interrupted test bundle: {error}"))?;
        Err("simulated interrupted retirement bundle write".into())
    }

    #[test]
    fn retirement_partial_restarts_after_delivery_only_change() {
        let (_owner, repository, _storage, mut broker, session, worktree, _base, _head, _branch) =
            closed_unique_commit_fixture("retirement partial delivery refresh");
        let original_plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        let original_content_identity = retirement_content_identity_sha256(&original_plan).unwrap();
        let error = broker
            .cleanup_resolve_apply_with_bundle_writer_mode(
                session.id,
                &original_plan.digest,
                || panic!("interrupted staging must fail before archive verification"),
                sync_archive_directory,
                |_, _| {
                    Some(crate::disk_headroom::DiskHeadroom {
                        bytes: u64::MAX,
                        inodes: None,
                    })
                },
                true,
                false,
                None,
                None,
                None,
                RecoveryBundleWriter {
                    max_bytes: MAX_RECOVERY_BUNDLE_BYTES,
                    write_to: interrupt_retirement_bundle_write,
                },
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("simulated interrupted retirement bundle write"),
            "{error}"
        );

        let partial = original_plan
            .archive_root
            .join(format!("{}.retirement-v6.partial", session.id));
        assert!(partial.is_dir());
        assert!(!partial.join(MANIFEST_FILE).exists());
        let (mut partial_record, _) = read_retirement_archive_record(&partial).unwrap();
        assert_eq!(
            partial_record.content_identity_sha256,
            original_content_identity
        );
        let old_partial_time = now_ms() - 20 * 24 * 60 * 60 * 1_000;
        partial_record.archive_created_at_ms = old_partial_time;
        std::fs::write(
            partial.join(RETIREMENT_ARCHIVE_RECORD_FILE),
            serde_json::to_vec(&partial_record).unwrap(),
        )
        .unwrap();

        let merge = isolated_git_output(&repository, &["merge", "--ff-only", &session.branch]);
        assert!(
            merge.status.success(),
            "could not advance delivery target: {}",
            String::from_utf8_lossy(&merge.stderr)
        );
        let refreshed_plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        assert_eq!(original_plan.digest, refreshed_plan.digest);
        assert_eq!(
            retirement_content_identity_sha256(&refreshed_plan).unwrap(),
            original_content_identity
        );
        assert!(
            refreshed_plan
                .recovery_root_proofs
                .iter()
                .all(|root| root.available_from_delivery)
        );

        let outcome = broker
            .cleanup_resolve_retirement_apply(session.id, &refreshed_plan.digest, false)
            .unwrap();
        assert!(outcome.worktree_removed, "{outcome:?}");
        assert!(!worktree.exists());
        assert!(!partial.exists());
        assert!(!outcome.archive.join(BUNDLE_FILE).exists());
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(outcome.archive.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            manifest["schema_version"],
            PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        );
        let prerequisite_refs = manifest["prerequisite_refs"].as_array().unwrap();
        assert!(!prerequisite_refs.is_empty());
        for pin in prerequisite_refs {
            let ref_name = pin["ref_name"].as_str().unwrap();
            let object_id = pin["object_id"].as_str().unwrap();
            assert_eq!(
                git_output(&repository, &["rev-parse", "--verify", ref_name]),
                object_id
            );
        }
        assert!(manifest["created_at_ms"].as_i64().unwrap() > old_partial_time);
        let final_record = read_retirement_archive_record(&outcome.archive).unwrap().0;
        assert_eq!(
            final_record.content_identity_sha256,
            original_content_identity
        );
        assert!(
            now_ms().saturating_sub(final_record.archive_created_at_ms)
                < i64::try_from(final_record.minimum_archive_retention_ms).unwrap(),
            "restarting an old partial must assign the rebuilt archive a fresh retention age"
        );
    }

    #[test]
    fn retirement_partial_preserves_checkout_when_source_content_changes() {
        let (_owner, _repository, _storage, mut broker, session, worktree, _base, _head, _branch) =
            closed_unique_commit_fixture("retirement partial source change");
        let reviewed_plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        let interrupted = broker
            .cleanup_resolve_apply_with_bundle_writer_mode(
                session.id,
                &reviewed_plan.digest,
                || panic!("interrupted staging must fail before archive verification"),
                sync_archive_directory,
                |_, _| {
                    Some(crate::disk_headroom::DiskHeadroom {
                        bytes: u64::MAX,
                        inodes: None,
                    })
                },
                true,
                false,
                None,
                None,
                None,
                RecoveryBundleWriter {
                    max_bytes: MAX_RECOVERY_BUNDLE_BYTES,
                    write_to: interrupt_retirement_bundle_write,
                },
            )
            .unwrap_err()
            .to_string();
        assert!(
            interrupted.contains("simulated interrupted retirement bundle write"),
            "{interrupted}"
        );

        let partial = reviewed_plan
            .archive_root
            .join(format!("{}.retirement-v6.partial", session.id));
        let receipt_before = std::fs::read(partial.join(RETIREMENT_ARCHIVE_RECORD_FILE)).unwrap();
        let bundle_before = std::fs::read(partial.join(BUNDLE_FILE)).unwrap();
        assert_eq!(bundle_before, b"interrupted bundle staging");
        std::fs::write(worktree.join("preserve.txt"), "changed after review\n").unwrap();

        let stale = broker
            .cleanup_resolve_retirement_apply(session.id, &reviewed_plan.digest, false)
            .unwrap_err();
        assert!(
            matches!(
                &stale,
                BrokerOpError::CleanupResolveConfirmationMismatch { .. }
            ),
            "the old confirmation must be refused before touching staging: {stale}"
        );
        let changed_plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        assert_ne!(reviewed_plan.digest, changed_plan.digest);
        let restart = broker
            .cleanup_resolve_retirement_apply(session.id, &changed_plan.digest, false)
            .unwrap_err();
        assert!(
            matches!(&restart, BrokerOpError::RecoveryArchiveUnverified { .. }),
            "a new confirmation must not replace staging for changed source content: {restart}"
        );

        assert!(worktree.is_dir());
        assert_eq!(
            std::fs::read(worktree.join("preserve.txt")).unwrap(),
            b"changed after review\n"
        );
        assert!(partial.is_dir());
        assert_eq!(
            std::fs::read(partial.join(RETIREMENT_ARCHIVE_RECORD_FILE)).unwrap(),
            receipt_before
        );
        assert_eq!(
            std::fs::read(partial.join(BUNDLE_FILE)).unwrap(),
            bundle_before
        );
    }

    #[test]
    fn retirement_confirmation_survives_forward_delivery_move_before_apply() {
        let (_owner, repository, _storage, mut broker, session, worktree, _base, _head, _branch) =
            closed_unique_commit_fixture("retirement forward delivery before apply");
        let reviewed_plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();

        let merge = isolated_git_output(
            &repository,
            &["merge", "--ff-only", session.branch.as_str()],
        );
        assert!(
            merge.status.success(),
            "could not advance delivery target: {}",
            String::from_utf8_lossy(&merge.stderr)
        );
        let current_plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        assert_eq!(reviewed_plan.digest, current_plan.digest);
        assert_ne!(
            reviewed_plan.delivery_targets,
            current_plan.delivery_targets
        );
        assert!(
            current_plan
                .recovery_root_proofs
                .iter()
                .all(|proof| proof.available_from_delivery)
        );

        let outcome = broker
            .cleanup_resolve_retirement_apply(session.id, &reviewed_plan.digest, false)
            .unwrap();
        assert!(outcome.worktree_removed, "{outcome:?}");
        assert!(!worktree.exists());
        assert!(!outcome.archive.join(BUNDLE_FILE).exists());
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(outcome.archive.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            manifest["schema_version"],
            PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        );
        assert_eq!(
            manifest["delivery_targets"],
            serde_json::json!(current_plan.delivery_targets)
        );
        for pin in manifest["prerequisite_refs"].as_array().unwrap() {
            let ref_name = pin["ref_name"].as_str().unwrap();
            let object_id = pin["object_id"].as_str().unwrap();
            assert_eq!(
                git_output(&repository, &["rev-parse", "--verify", ref_name]),
                object_id
            );
        }
    }

    #[test]
    fn preserved_branch_retirement_rechecks_content_after_archive_verification() {
        let (_owner, repository, _storage, mut broker, session, worktree, _base, head, branch_ref) =
            closed_unique_commit_fixture("preserved retirement rechecks archived content");
        git(&worktree, &["checkout", "--detach"]);
        let plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        assert!(plan.checkout_branch_ref.is_none());
        assert_eq!(plan.checkout_branch_tip.as_deref(), Some(head.as_str()));

        let late_change = worktree.join("preserve.txt");
        let outcome = broker
            .cleanup_resolve_apply_with_bundle_writer_mode(
                session.id,
                &plan.digest,
                move || {
                    std::fs::write(&late_change, "changed after archive\n").unwrap();
                },
                sync_archive_directory,
                |_, _| {
                    Some(crate::disk_headroom::DiskHeadroom {
                        bytes: u64::MAX,
                        inodes: None,
                    })
                },
                true,
                false,
                None,
                None,
                None,
                RecoveryBundleWriter {
                    max_bytes: MAX_RECOVERY_BUNDLE_BYTES,
                    write_to: write_recovery_bundle_to_file,
                },
            )
            .unwrap();

        assert!(!outcome.worktree_removed, "{outcome:?}");
        assert!(
            outcome
                .cleanup_error
                .as_deref()
                .is_some_and(|error| { error.contains("changed after archive verification") })
        );
        assert!(worktree.is_dir());
        assert!(outcome.archive.join(MANIFEST_FILE).is_file());
        assert_eq!(
            std::fs::read(worktree.join("preserve.txt")).unwrap(),
            b"changed after archive\n"
        );
        assert_eq!(
            git_output(&repository, &["rev-parse", "--verify", branch_ref.as_str()]),
            head
        );
    }

    #[test]
    fn unmodified_preserved_branch_retirement_removes_checkout_and_keeps_branch() {
        let (_owner, repository, _storage, mut broker, session, worktree, _base, head, branch_ref) =
            closed_unique_commit_fixture("preserved retirement keeps detached branch");
        git(&worktree, &["checkout", "--detach"]);
        let plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        assert!(plan.checkout_branch_ref.is_none());
        assert_eq!(plan.checkout_branch_tip.as_deref(), Some(head.as_str()));

        let outcome = broker
            .cleanup_resolve_retirement_apply(session.id, &plan.digest, false)
            .unwrap();

        assert!(outcome.worktree_removed, "{outcome:?}");
        assert!(outcome.cleanup_error.is_none(), "{outcome:?}");
        assert!(!worktree.exists());
        assert!(outcome.archive.join(MANIFEST_FILE).is_file());
        assert_eq!(
            git_output(&repository, &["rev-parse", "--verify", branch_ref.as_str()]),
            head
        );
    }

    #[test]
    fn retirement_archive_refuses_backward_delivery_move_and_keeps_prerequisite_pins() {
        let (_owner, repository, _storage, mut broker, session, worktree, base, head, branch_ref) =
            closed_unique_commit_fixture("retirement archive backward delivery move");
        let plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        assert!(plan.delivery_targets.contains(&base));

        let tree = format!("{base}^{{tree}}");
        let unrelated_root = isolated_git_output(
            &repository,
            &["commit-tree", &tree, "-m", "unrelated delivery root"],
        );
        assert!(
            unrelated_root.status.success(),
            "could not create unrelated delivery root: {}",
            String::from_utf8_lossy(&unrelated_root.stderr)
        );
        let unrelated_root = String::from_utf8(unrelated_root.stdout)
            .unwrap()
            .trim()
            .to_owned();

        let repository_after_archive = repository.clone();
        let moved_target = unrelated_root.clone();
        let outcome = broker
            .cleanup_resolve_apply_with_bundle_writer_mode(
                session.id,
                &plan.digest,
                move || {
                    let moved = isolated_git_output(
                        &repository_after_archive,
                        &["update-ref", "refs/heads/main", moved_target.as_str()],
                    );
                    assert!(
                        moved.status.success(),
                        "could not move the delivery ref: {}",
                        String::from_utf8_lossy(&moved.stderr)
                    );
                },
                sync_archive_directory,
                |_, _| {
                    Some(crate::disk_headroom::DiskHeadroom {
                        bytes: u64::MAX,
                        inodes: None,
                    })
                },
                true,
                false,
                None,
                None,
                None,
                RecoveryBundleWriter {
                    max_bytes: MAX_RECOVERY_BUNDLE_BYTES,
                    write_to: write_recovery_bundle_to_file,
                },
            )
            .unwrap();

        assert!(!outcome.worktree_removed, "{outcome:?}");
        assert!(
            outcome
                .cleanup_error
                .as_deref()
                .is_some_and(|error| error.contains("archive prerequisites are no longer retained")),
            "backward delivery movement must refuse checkout removal: {outcome:?}"
        );
        assert!(worktree.is_dir());
        assert_eq!(
            std::fs::read(worktree.join("preserve.txt")).unwrap(),
            b"uncommitted recovery data\n"
        );
        assert_eq!(
            GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref(&branch_ref)
                .as_deref(),
            Some(head.as_str())
        );
        assert!(outcome.archive.is_dir());

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(outcome.archive.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            manifest["schema_version"],
            PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION
        );
        let prerequisite_refs = manifest["prerequisite_refs"].as_array().unwrap();
        assert!(!prerequisite_refs.is_empty());
        for pin in prerequisite_refs {
            let ref_name = pin["ref_name"].as_str().unwrap();
            let object_id = pin["object_id"].as_str().unwrap();
            assert_eq!(
                git_output(&repository, &["rev-parse", "--verify", ref_name]),
                object_id
            );
        }

        let refreshed_plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        assert_eq!(plan.digest, refreshed_plan.digest);
        assert!(
            !refreshed_plan.delivery_targets.contains(&base),
            "the fixture must actually move the captured delivery prerequisite"
        );
        let retry_error = broker
            .cleanup_resolve_retirement_apply(session.id, &refreshed_plan.digest, false)
            .unwrap_err()
            .to_string();
        assert!(
            retry_error.contains("prerequisite") && retry_error.contains("verification"),
            "a completed archive with a lost prerequisite must refuse reuse: {retry_error}"
        );
        assert!(worktree.is_dir());
        assert!(outcome.archive.is_dir());
        for pin in prerequisite_refs {
            let ref_name = pin["ref_name"].as_str().unwrap();
            let object_id = pin["object_id"].as_str().unwrap();
            assert_eq!(
                git_output(&repository, &["rev-parse", "--verify", ref_name]),
                object_id
            );
        }
    }

    #[test]
    fn v6_bundled_root_still_requires_the_verified_bundle_to_hold_it() {
        let (_owner, repository, _storage, mut broker, session, _worktree, _base, _head, _branch) =
            closed_unique_commit_fixture("v6 bundled root membership remains required");
        let plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        let outcome = broker
            .cleanup_resolve_retirement_apply(session.id, &plan.digest, false)
            .unwrap();
        assert!(outcome.worktree_removed, "{outcome:?}");
        broker.owned_recovery_archive(&outcome.archive).unwrap();

        let replacement = _owner.path().join("bundle-without-session-root");
        let bundle = isolated_git_output(
            &repository,
            &["bundle", "create", replacement.to_str().unwrap(), "HEAD"],
        );
        assert!(bundle.status.success(), "{bundle:?}");
        let bundle_path = outcome.archive.join(BUNDLE_FILE);
        std::fs::copy(replacement, &bundle_path).unwrap();
        let bytes = std::fs::read(&bundle_path).unwrap();
        let manifest_path = outcome.archive.join(MANIFEST_FILE);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["schema_version"], serde_json::json!(6));
        manifest["bundle_bytes"] = serde_json::json!(bytes.len() as u64);
        manifest["bundle_sha256"] = serde_json::json!(sha256_hex(&bytes));
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let error = broker.owned_recovery_archive(&outcome.archive).unwrap_err();
        assert!(error.contains("does not hold"), "{error}");
    }

    #[test]
    fn retirement_pins_keep_bundle_prerequisites_after_checkout_removal_and_git_prune() {
        let (_owner, repository, _storage, mut broker, session, worktree, base, head, branch_ref) =
            closed_unique_commit_fixture("retirement pinned bundle survives prune");
        let plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        assert!(plan.delivery_targets.contains(&base));
        assert!(recovery_bundle_required(&plan));

        let outcome = broker
            .cleanup_resolve_retirement_apply(session.id, &plan.digest, false)
            .unwrap();
        assert!(outcome.worktree_removed, "{outcome:?}");
        assert!(!worktree.exists());
        assert!(
            !isolated_git_output(
                &repository,
                &["show-ref", "--verify", "--quiet", &branch_ref]
            )
            .status
            .success(),
            "ordinary spawned cleanup should remove its matching branch"
        );

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(outcome.archive.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert!(outcome.archive.join(BUNDLE_FILE).is_file());
        let prerequisite_refs = manifest["prerequisite_refs"].as_array().unwrap();
        assert!(
            prerequisite_refs
                .iter()
                .any(|pin| { pin["object_id"].as_str() == Some(base.as_str()) })
        );

        let tree = format!("{base}^{{tree}}");
        let unrelated_root = isolated_git_output(
            &repository,
            &["commit-tree", &tree, "-m", "unrelated delivery root"],
        );
        assert!(
            unrelated_root.status.success(),
            "could not create unrelated delivery root: {}",
            String::from_utf8_lossy(&unrelated_root.stderr)
        );
        let unrelated_root = String::from_utf8(unrelated_root.stdout)
            .unwrap()
            .trim()
            .to_owned();
        let moved = isolated_git_output(
            &repository,
            &["update-ref", "refs/heads/main", &unrelated_root],
        );
        assert!(
            moved.status.success(),
            "could not move the delivery ref: {}",
            String::from_utf8_lossy(&moved.stderr)
        );
        let reflogs =
            isolated_git_output(&repository, &["reflog", "expire", "--expire=now", "--all"]);
        assert!(
            reflogs.status.success(),
            "could not expire old reflogs: {}",
            String::from_utf8_lossy(&reflogs.stderr)
        );
        let pruned = isolated_git_output(&repository, &["gc", "--prune=now"]);
        assert!(
            pruned.status.success(),
            "could not prune unreachable Git objects: {}",
            String::from_utf8_lossy(&pruned.stderr)
        );
        assert!(
            isolated_git_output(
                &repository,
                &["cat-file", "-e", &format!("{base}^{{commit}}")]
            )
            .status
            .success(),
            "the exact captured prerequisite object must remain reachable through its pin"
        );
        assert!(
            !isolated_git_output(
                &repository,
                &["cat-file", "-e", &format!("{head}^{{commit}}")]
            )
            .status
            .success(),
            "the removed checkout's unlanded commit should be absent from the repository after prune"
        );

        let owned = broker.owned_recovery_archive(&outcome.archive).unwrap();
        let captured_targets: Vec<String> =
            serde_json::from_value(manifest["delivery_targets"].clone()).unwrap();
        assert_eq!(owned.prerequisite_targets, captured_targets);
        assert_eq!(
            owned.prerequisite_refs,
            serde_json::from_value::<Vec<RecoveryArchivePrerequisiteRef>>(
                manifest["prerequisite_refs"].clone()
            )
            .unwrap()
        );
        assert!(
            recovery_bundle_required(&plan),
            "the archived session root still requires the verified bundle"
        );
        let restored_ref = "refs/heads/aethyme-recovery-bundle-test";
        let bundle_path = outcome.archive.join(BUNDLE_FILE);
        let source_refspec = format!("{branch_ref}:{restored_ref}");
        let restored = isolated_git_output(
            &repository,
            &[
                "fetch",
                "--no-tags",
                bundle_path.to_str().unwrap(),
                source_refspec.as_str(),
            ],
        );
        assert!(
            restored.status.success(),
            "could not restore the archived root after pruning: {}",
            String::from_utf8_lossy(&restored.stderr)
        );
        assert_eq!(
            git_output(&repository, &["rev-parse", "--verify", restored_ref]),
            head
        );
    }

    #[test]
    fn retirement_missing_or_conflicting_prerequisite_pin_blocks_archive_ownership() {
        let (_owner, repository, _storage, mut broker, session, _worktree, base, head, _branch) =
            closed_unique_commit_fixture("retirement pin ownership tampering");
        let plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        let outcome = broker
            .cleanup_resolve_retirement_apply(session.id, &plan.digest, false)
            .unwrap();
        assert!(outcome.worktree_removed, "{outcome:?}");
        let owned = broker.owned_recovery_archive(&outcome.archive).unwrap();
        let pin = owned
            .prerequisite_refs
            .first()
            .expect("schema v6 archive has at least one prerequisite pin");

        let removed = isolated_git_output(
            &repository,
            &[
                "update-ref",
                "-d",
                pin.ref_name.as_str(),
                pin.object_id.as_str(),
            ],
        );
        assert!(
            removed.status.success(),
            "could not remove the fixture prerequisite ref: {}",
            String::from_utf8_lossy(&removed.stderr)
        );
        assert!(
            broker.owned_recovery_archive(&outcome.archive).is_err(),
            "an archive with a missing pin must not be treated as broker-owned"
        );
        let scan = broker.recovery_archives().unwrap();
        assert!(
            scan.owned
                .iter()
                .all(|archive| archive.path != outcome.archive)
                && scan
                    .unowned
                    .iter()
                    .any(|(path, _)| path == &outcome.archive),
            "the missing-pin archive must be excluded from GC's owned deletion candidates"
        );

        let conflicting_oid = if pin.object_id != head { head } else { base };
        let installed = isolated_git_output(
            &repository,
            &[
                "update-ref",
                pin.ref_name.as_str(),
                conflicting_oid.as_str(),
            ],
        );
        assert!(
            installed.status.success(),
            "could not install the foreign conflicting ref: {}",
            String::from_utf8_lossy(&installed.stderr)
        );
        let deadline = recovery_bundle_deadline().unwrap();
        assert!(
            ensure_recovery_prerequisite_refs_installed(
                &GitRepo::discover(&repository).unwrap(),
                &owned.prerequisite_refs,
                deadline,
            )
            .is_err(),
            "retry must refuse a conflicting pin rather than overwrite it"
        );
        assert_eq!(
            git_output(&repository, &["rev-parse", "--verify", &pin.ref_name]),
            conflicting_oid
        );
        assert!(broker.owned_recovery_archive(&outcome.archive).is_err());
        let scan = broker.recovery_archives().unwrap();
        assert!(
            scan.owned
                .iter()
                .all(|archive| archive.path != outcome.archive)
                && scan
                    .unowned
                    .iter()
                    .any(|(path, _)| path == &outcome.archive),
            "a conflicting-pin archive must remain outside owned GC deletion candidates"
        );
        assert!(outcome.archive.is_dir());
    }

    #[test]
    fn copied_v6_archive_is_unowned_and_blocks_shared_pin_release() {
        let (_owner, repository, _storage, mut broker, session, _worktree, _base, _head, _branch) =
            closed_unique_commit_fixture("retirement duplicate archive pin use");
        let plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        let outcome = broker
            .cleanup_resolve_retirement_apply(session.id, &plan.digest, false)
            .unwrap();
        assert!(outcome.worktree_removed, "{outcome:?}");
        let owned = broker.owned_recovery_archive(&outcome.archive).unwrap();
        let copied_archive = outcome.archive.with_file_name(format!(
            "{}-copy",
            outcome
                .archive
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap()
        ));
        copy_archive_tree(&outcome.archive, &copied_archive).unwrap();
        let copy_error = broker.owned_recovery_archive(&copied_archive).unwrap_err();
        assert!(
            copy_error.contains("canonical retirement identity"),
            "a renamed copy cannot become a second owned archive: {copy_error}"
        );

        std::fs::remove_dir_all(&outcome.archive).unwrap();
        let release_error = broker
            .release_recovery_archive_prerequisite_refs_after_removal(&owned)
            .unwrap_err();
        assert!(
            release_error.contains("also references prerequisite"),
            "refs used by a sibling archive must be retained: {release_error}"
        );
        assert!(copied_archive.is_dir());
        for pin in &owned.prerequisite_refs {
            assert_eq!(
                git_output(&repository, &["rev-parse", "--verify", &pin.ref_name]),
                pin.object_id
            );
        }
    }

    #[test]
    fn retirement_content_identity_ignores_disposable_directory_churn_but_binds_files() {
        let (_owner, _repository, _storage, broker, session, _worktree, _base, _head, _branch) =
            closed_unique_commit_fixture("retirement source identity classification");
        let plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        let original = retirement_content_identity_sha256(&plan).unwrap();

        let mut with_disposable_directory = plan.clone();
        with_disposable_directory.ignored_paths.push(IgnoredPath {
            path: "target/generated".into(),
            classification: IgnoredPathClassification::DisposableDirectory,
        });
        assert_eq!(
            retirement_content_identity_sha256(&with_disposable_directory).unwrap(),
            original,
            "policy-authorized disposable directory churn must not strand staging"
        );

        let file = ArchivedFile {
            path: "credentials.txt".into(),
            bytes: 6,
            sha256: sha256_hex(b"secret"),
            symlink: false,
            mode: Some(0o600),
        };
        let mut with_preserved_file = with_disposable_directory;
        with_preserved_file.ignored_files.push(file);
        let file_identity = retirement_content_identity_sha256(&with_preserved_file).unwrap();
        assert_ne!(file_identity, original);
        with_preserved_file.ignored_files[0].sha256 = sha256_hex(b"changed");
        assert_ne!(
            retirement_content_identity_sha256(&with_preserved_file).unwrap(),
            file_identity,
            "preserved ignored-file content changes must block partial restart"
        );
    }

    #[test]
    fn v5_bundle_keeps_pruned_unavailable_roots_owned_and_checks_bundle_membership() {
        let (_owner, repository, _storage, mut broker, session, worktree, _base, head, branch) =
            closed_unique_commit_fixture("retirement archive survives source pruning");
        std::fs::remove_file(worktree.join("preserve.txt")).unwrap();

        let plan = broker
            .cleanup_resolve_retirement_plan(session.id, false)
            .unwrap();
        assert!(
            plan.recovery_root_proofs
                .iter()
                .any(|root| root.object_id == head && !root.available_from_delivery)
        );
        let outcome = broker
            .cleanup_resolve_retirement_apply(session.id, &plan.digest, false)
            .unwrap();
        assert!(outcome.worktree_removed, "{outcome:?}");
        assert!(!worktree.exists());

        let expire =
            isolated_git_output(&repository, &["reflog", "expire", "--expire=now", "--all"]);
        assert!(
            expire.status.success(),
            "{}",
            String::from_utf8_lossy(&expire.stderr)
        );
        let gc = isolated_git_output(&repository, &["gc", "--prune=now"]);
        assert!(
            gc.status.success(),
            "{}",
            String::from_utf8_lossy(&gc.stderr)
        );
        let branch_ref = format!("refs/heads/{branch}");
        let branch = isolated_git_output(
            &repository,
            &["show-ref", "--verify", "--quiet", &branch_ref],
        );
        assert!(
            !branch.status.success(),
            "retirement should remove its matching branch ref"
        );
        let missing = isolated_git_output(&repository, &["cat-file", "-e", &head]);
        assert!(
            !missing.status.success(),
            "the source object should be unreachable and pruned before the ownership check"
        );

        let archived = broker.owned_recovery_archive(&outcome.archive).unwrap();
        assert!(
            archived
                .recovery_roots
                .iter()
                .any(|root| root.object_id == head && !root.available_from_delivery)
        );
        assert!(!broker.recovery_archive_landed(&archived));
        assert!(
            now_ms().saturating_sub(archived.created_at_ms)
                < i64::try_from(archived.minimum_archive_retention_ms.unwrap()).unwrap(),
            "the recently created archive must remain inside its immutable minimum age"
        );
        let inventory = broker.recovery_archives().unwrap();
        assert_eq!(inventory.owned.len(), 1, "{inventory:?}");
        assert!(inventory.unowned.is_empty(), "{inventory:?}");

        let replacement_bundle = _owner.path().join("bundle-without-retirement-root");
        let bundle = isolated_git_output(
            &repository,
            &[
                "bundle",
                "create",
                replacement_bundle.to_str().unwrap(),
                "HEAD",
            ],
        );
        assert!(
            bundle.status.success(),
            "could not create the incomplete replacement bundle: {}",
            String::from_utf8_lossy(&bundle.stderr)
        );
        let bundle_path = outcome.archive.join(BUNDLE_FILE);
        std::fs::copy(&replacement_bundle, &bundle_path).unwrap();
        let bundle_bytes = std::fs::read(&bundle_path).unwrap();
        let manifest_path = outcome.archive.join(MANIFEST_FILE);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["bundle_bytes"] = serde_json::json!(bundle_bytes.len() as u64);
        manifest["bundle_sha256"] = serde_json::json!(sha256_hex(&bundle_bytes));
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let error = broker.owned_recovery_archive(&outcome.archive).unwrap_err();
        assert!(error.contains("does not hold"), "{error}");
    }

    #[test]
    fn an_archive_matching_its_plan_verifies() {
        let (_tmp, repo, plan) = staged_checkout();
        let dir = tempfile::tempdir().unwrap();
        write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap();
        assert!(dir.path().join(MANIFEST_FILE).is_file());
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(manifest["schema_version"], RECOVERY_ARCHIVE_SCHEMA_VERSION);
        assert!(manifest.get("bundle_bytes").is_none());
        assert!(manifest.get("bundle_sha256").is_none());
        assert_eq!(
            std::fs::read(dir.path().join("untracked/new.txt")).unwrap(),
            b"untracked\n"
        );
    }

    #[test]
    fn legacy_v2_plan_digest_keeps_its_frozen_serialization() {
        let plan = CleanupResolvePlan {
            schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
            digest: String::new(),
            session_id: 9,
            target_origin: SessionOrigin::Spawned,
            recovery_archive_schema_version: RECOVERY_ARCHIVE_SCHEMA_VERSION,
            minimum_archive_retention_ms: None,
            retirement_allow_adopted: None,
            retirement_witness_sha256: None,
            repository_path: "/tmp/aethyme-v2-repository".into(),
            worktree_path: "/tmp/aethyme-v2".into(),
            branch_ref: "refs/heads/main".into(),
            branch_tip: None,
            checkout_branch_ref: None,
            checkout_branch_tip: None,
            recovery_root_proofs: Vec::new(),
            disposition: CleanupDisposition::Dirty,
            reason: String::new(),
            head: "1".repeat(40),
            delivery_targets: vec!["2".repeat(40)],
            landing: RecoveryLandingAssessment::Verdict(crate::LandingVerdict::NotLanded {
                examined: 1,
                truncated: false,
            }),
            unlanded_commits: vec!["3".repeat(40)],
            staged_patch_bytes: 0,
            staged_patch_sha256: "4".repeat(40),
            unstaged_patch_bytes: 0,
            unstaged_patch_sha256: "5".repeat(40),
            untracked_files: vec![ArchivedFile {
                path: "new.txt".into(),
                bytes: 3,
                sha256: "6".repeat(40),
                symlink: false,
                mode: None,
            }],
            untracked_bytes: 3,
            ignored_paths: vec![IgnoredPath {
                path: "cache/".into(),
                classification: IgnoredPathClassification::DisposableDirectory,
            }],
            ignored_files: vec![ArchivedFile {
                path: "private.txt".into(),
                bytes: 7,
                sha256: "7".repeat(40),
                symlink: false,
                mode: None,
            }],
            ignored_bytes: 7,
            ignored_policy_sha256: String::new(),
            archive_root: PathBuf::new(),
            apply_command: String::new(),
            discard_command: String::new(),
        };
        let regenerable = vec!["dist/".to_owned(), "target/".to_owned()];
        let policy = IgnoredPolicyProof {
            strict: true,
            regenerable: &regenerable,
        };

        assert_eq!(
            legacy_v2_cleanup_resolve_digest(&plan, &policy).unwrap(),
            "31dec7c8b3dd9700062bd6332b0e2dab83d73767ec354fb4dc0ad318658a50c8"
        );
        // Independent Python JSON/SHA-256 vector using the pre-change v3 field order.
        assert_eq!(
            legacy_v3_cleanup_resolve_digest(&plan, &policy).unwrap(),
            "89c341cd4c86f79e0c35e1652d3a4cd67b8c9471aca1402fe39a5b91ec2534a0"
        );
    }

    #[cfg(unix)]
    #[test]
    fn schema_v5_receipt_binds_the_archive_creation_time() {
        let (_tmp, repo, mut plan) = staged_checkout();
        plan.recovery_archive_schema_version = RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION;
        plan.minimum_archive_retention_ms = Some(MIN_RECOVERY_ARCHIVE_RETENTION_MS);
        plan.retirement_allow_adopted = Some(false);
        plan.branch_tip = Some(plan.head.clone());
        plan.checkout_branch_ref = Some("refs/heads/main".into());
        plan.checkout_branch_tip = Some(plan.head.clone());
        plan.recovery_root_proofs = recovery_root_proofs_for_plan_parts(
            &repo,
            &plan.head,
            &plan.branch_ref,
            plan.branch_tip.as_deref(),
            (
                plan.checkout_branch_ref.as_deref(),
                plan.checkout_branch_tip.as_deref(),
            ),
            &plan.delivery_targets,
            &PlanAncestryMemo::for_delivery_targets(&plan.delivery_targets, None),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        plan.archive_root = dir.path().parent().unwrap().to_path_buf();
        create_retirement_archive_record(dir.path(), &plan).unwrap();
        write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap();

        let manifest_path = dir.path().join(MANIFEST_FILE);
        let receipt_path = dir.path().join(RETIREMENT_ARCHIVE_RECORD_FILE);
        let original_manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        let original_record: RetirementArchiveRecord =
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
        assert_eq!(
            original_manifest["created_at_ms"],
            original_record.archive_created_at_ms
        );
        verify_recovery_archive_payload_for_plan(&repo, dir.path(), &plan).unwrap();

        let mut changed_manifest = original_manifest.clone();
        changed_manifest["created_at_ms"] =
            serde_json::json!(original_record.archive_created_at_ms + 1);
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&changed_manifest).unwrap(),
        )
        .unwrap();
        let error = verify_recovery_archive_payload_for_plan(&repo, dir.path(), &plan).unwrap_err();
        assert!(error.contains("retirement record does not match its archive manifest"));

        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&original_manifest).unwrap(),
        )
        .unwrap();
        let mut changed_record = original_record;
        changed_record.archive_created_at_ms += 1;
        std::fs::write(&receipt_path, serde_json::to_vec(&changed_record).unwrap()).unwrap();
        let error = verify_recovery_archive_payload_for_plan(&repo, dir.path(), &plan).unwrap_err();
        assert!(error.contains("retirement record does not match its archive manifest"));
    }

    #[cfg(unix)]
    #[test]
    fn schema_v4_bundle_manifest_hashes_the_streamed_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("base.txt"), "base\n").unwrap();
        git(tmp.path(), &["add", "base.txt"]);
        git(tmp.path(), &["commit", "-qm", "initial"]);
        let repo = GitRepo::discover(tmp.path()).unwrap();
        let delivered = repo.head_commit().unwrap();
        std::fs::write(tmp.path().join("pending.txt"), "pending\n").unwrap();
        git(tmp.path(), &["add", "pending.txt"]);
        git(tmp.path(), &["commit", "-qm", "pending"]);
        let head = repo.head_commit().unwrap();
        let empty_patch = sha256_hex(b"");
        let policy = crate::AutoCleanupPolicy::load(repo.root()).unwrap();
        let proof = IgnoredPolicyProof {
            strict: policy.strict,
            regenerable: &policy.regenerable,
        };
        let plan = CleanupResolvePlan {
            schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
            digest: sha256_hex(b"v4 bundle fixture plan"),
            session_id: 9,
            target_origin: SessionOrigin::Spawned,
            recovery_archive_schema_version: RECOVERY_ARCHIVE_SCHEMA_VERSION,
            minimum_archive_retention_ms: None,
            retirement_allow_adopted: None,
            retirement_witness_sha256: None,
            repository_path: repo.root().to_string_lossy().into_owned(),
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            branch_ref: "refs/heads/main".into(),
            branch_tip: Some(head.clone()),
            checkout_branch_ref: Some("refs/heads/main".into()),
            checkout_branch_tip: Some(head.clone()),
            recovery_root_proofs: Vec::new(),
            disposition: CleanupDisposition::Dirty,
            reason: "fixture".into(),
            head: head.clone(),
            delivery_targets: vec![delivered],
            landing: RecoveryLandingAssessment::Verdict(crate::LandingVerdict::NotLanded {
                examined: 1,
                truncated: false,
            }),
            unlanded_commits: vec![head],
            staged_patch_bytes: 0,
            staged_patch_sha256: empty_patch.clone(),
            unstaged_patch_bytes: 0,
            unstaged_patch_sha256: empty_patch,
            untracked_files: Vec::new(),
            untracked_bytes: 0,
            ignored_paths: Vec::new(),
            ignored_files: Vec::new(),
            ignored_bytes: 0,
            ignored_policy_sha256: sha256_hex(&serde_json::to_vec(&proof).unwrap()),
            archive_root: PathBuf::new(),
            apply_command: String::new(),
            discard_command: String::new(),
        };
        let dir = tempfile::tempdir().unwrap();
        write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap();

        let manifest_path = dir.path().join(MANIFEST_FILE);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(value["schema_version"], RECOVERY_ARCHIVE_SCHEMA_VERSION);
        assert_eq!(value["bundle"], BUNDLE_FILE);
        // Schema-4 output stays parseable by released strict readers that reject unknown fields.
        assert!(value.get("checkout_branch_ref").is_none());
        assert!(value.get("checkout_branch_tip").is_none());
        assert!(value.get("recovery_root_proofs").is_none());
        assert!(
            value["bundle_bytes"]
                .as_u64()
                .is_some_and(|bytes| { bytes > 0 && bytes <= MAX_RECOVERY_BUNDLE_BYTES })
        );
        assert!(value["bundle_sha256"].as_str().is_some_and(valid_sha256));
        for (field, reason) in [
            ("bundle_bytes", "missing its byte length"),
            ("bundle_sha256", "missing a valid SHA-256"),
        ] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            let missing: StoredManifest = serde_json::from_value(missing).unwrap();
            assert!(
                validate_manifest_identity(&missing)
                    .unwrap_err()
                    .contains(reason)
            );
        }
        let mut fields_without_bundle = value.clone();
        fields_without_bundle["bundle"] = serde_json::Value::Null;
        let fields_without_bundle: StoredManifest =
            serde_json::from_value(fields_without_bundle).unwrap();
        assert!(
            validate_manifest_identity(&fields_without_bundle)
                .unwrap_err()
                .contains("integrity fields without a bundle")
        );

        let mut oversized = value.clone();
        oversized["bundle_bytes"] = serde_json::json!(MAX_RECOVERY_BUNDLE_BYTES + 1);
        let oversized: StoredManifest = serde_json::from_value(oversized).unwrap();
        assert!(
            validate_manifest_identity(&oversized)
                .unwrap_err()
                .contains("outside its supported bound")
        );

        let manifest_v4: StoredManifest = serde_json::from_value(value.clone()).unwrap();
        verify_recovery_archive_payload_for_plan(&repo, dir.path(), &plan).unwrap();
        let mut v4_with_v5_only_fields = value.clone();
        v4_with_v5_only_fields["checkout_branch_ref"] = serde_json::json!("refs/heads/main");
        v4_with_v5_only_fields["recovery_root_proofs"] = serde_json::json!([]);
        let v4_with_v5_only_fields: StoredManifest =
            serde_json::from_value(v4_with_v5_only_fields).unwrap();
        assert!(
            validate_manifest_identity(&v4_with_v5_only_fields)
                .unwrap_err()
                .contains("only retirement archives may declare retirement metadata")
        );

        let legacy_digest = legacy_v2_cleanup_resolve_digest(&plan, &proof).unwrap();
        let mut legacy_value = value.clone();
        legacy_value["schema_version"] =
            serde_json::json!(PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION);
        legacy_value["plan_digest"] = serde_json::json!(legacy_digest);
        legacy_value.as_object_mut().unwrap().remove("branch_tip");
        legacy_value.as_object_mut().unwrap().remove("bundle_bytes");
        legacy_value
            .as_object_mut()
            .unwrap()
            .remove("bundle_sha256");
        std::fs::write(&manifest_path, serde_json::to_vec(&legacy_value).unwrap()).unwrap();
        verify_recovery_archive_payload_for_plan(&repo, dir.path(), &plan).unwrap();
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();

        let bundle_path = dir.path().join(BUNDLE_FILE);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&bundle_path)
            .unwrap()
            .write_all(b"tamper")
            .unwrap();
        assert!(
            verify_recovery_archive_tree(dir.path(), &manifest_v4)
                .unwrap_err()
                .contains("differs from its manifest integrity record")
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_untracked_file_identity_includes_unix_permission_bits() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("tool.sh");
        std::fs::write(&file, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o751)).unwrap();

        let described = describe_untracked(&file, "tool.sh").unwrap().unwrap();
        assert_eq!(described.mode, Some(0o751));
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_staging_symlink_is_rejected_before_any_payload_write() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = root.path().join("archive.partial");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();

        let error = create_exclusive_archive_directory(root.path(), "archive.partial").unwrap_err();
        assert!(error.contains("exclusively create"), "{error}");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_archive_root_is_rejected_before_staging() {
        let parent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = parent.path().join("archive-root");
        std::os::unix::fs::symlink(outside.path(), &root).unwrap();

        let error = prepare_archive_root(&root).unwrap_err();
        assert!(error.contains("real directory"), "{error}");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn archive_inventory_does_not_enumerate_a_replaced_symlink_root() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&repository).unwrap();
        let root = broker.recovery_archive_root().unwrap();
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("unrelated-archive")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &root).unwrap();

        let scan = broker.recovery_archives().unwrap();
        assert!(scan.owned.is_empty());
        assert_eq!(scan.unowned.len(), 1);
        assert_eq!(scan.unowned[0].0, root);
        assert!(scan.unowned[0].1.contains("unsafe archive root"));
    }

    #[cfg(unix)]
    #[test]
    fn archive_inventory_rejects_a_symlink_in_the_owned_root_chain() {
        let anchor = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let parent = anchor.path().join("recovery-archives");
        std::os::unix::fs::symlink(outside.path(), &parent).unwrap();
        let root = parent.join("repository-key");

        let error = inspect_archive_root(&root).unwrap_err();
        assert!(
            error
                .reason
                .contains("archive parent is not a real directory"),
            "{error:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_archive_file_symlink_is_not_followed_or_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"leave this alone").unwrap();
        std::os::unix::fs::symlink(&sentinel, root.path().join("manifest.json")).unwrap();

        let error =
            create_new_archive_file(root.path(), MANIFEST_FILE, b"replacement").unwrap_err();
        assert!(error.contains("cannot create archive file"), "{error}");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"leave this alone");
    }

    #[test]
    fn archive_directories_are_synced_bottom_up() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("untracked/nested")).unwrap();
        std::fs::create_dir(root.path().join("ignored")).unwrap();

        let synced = std::cell::RefCell::new(Vec::new());
        sync_archive_directories_bottom_up(root.path(), &|path| {
            synced
                .borrow_mut()
                .push(path.strip_prefix(root.path()).unwrap().to_path_buf());
            Ok(())
        })
        .unwrap();

        let synced = synced.into_inner();
        let position = |path: &str| {
            synced
                .iter()
                .position(|entry| entry == Path::new(path))
                .unwrap()
        };
        assert!(position("untracked/nested") < position("untracked"));
        assert!(position("untracked") < position(""));
        assert!(position("ignored") < position(""));
    }

    #[test]
    fn archive_ownership_rejects_unlisted_files_and_keeps_legacy_v1_readable() {
        let (_tmp, repo, plan) = staged_checkout();
        let dir = tempfile::tempdir().unwrap();
        write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap();
        let manifest_path = dir.path().join(MANIFEST_FILE);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        #[cfg(unix)]
        let valid_v3 = value.clone();
        let mut malformed = value.clone();
        malformed["ignored_policy_sha256"] = serde_json::json!("invalid");
        let malformed: StoredManifest = serde_json::from_value(malformed).unwrap();
        assert!(
            recovery_archive_expected_files(&malformed)
                .unwrap_err()
                .contains("invalid plan or ignored policy identity")
        );

        value["schema_version"] = serde_json::json!(LEGACY_RECOVERY_ARCHIVE_SCHEMA_VERSION);
        value.as_object_mut().unwrap().remove("branch_tip");
        for file in value["untracked_files"].as_array_mut().unwrap() {
            file.as_object_mut().unwrap().remove("mode");
        }
        value.as_object_mut().unwrap().remove("ignored_directory");
        value.as_object_mut().unwrap().remove("ignored_paths");
        value.as_object_mut().unwrap().remove("ignored_files");
        value
            .as_object_mut()
            .unwrap()
            .remove("ignored_policy_sha256");
        value.as_object_mut().unwrap().remove("plan_digest");
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
        let legacy: StoredManifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        verify_recovery_archive_tree(dir.path(), &legacy).unwrap();

        std::fs::write(dir.path().join("unlisted.bin"), b"work").unwrap();
        let error = verify_recovery_archive_tree(dir.path(), &legacy).unwrap_err();
        assert!(error.contains("unlisted file"), "{error}");

        value["schema_version"] = serde_json::json!(PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION);
        let missing_v2_field: StoredManifest = serde_json::from_value(value.clone()).unwrap();
        assert!(
            recovery_archive_expected_files(&missing_v2_field)
                .unwrap_err()
                .contains("missing its ignored directory identity")
        );

        #[cfg(unix)]
        {
            let mut missing_mode = valid_v3;
            let untracked = missing_mode["untracked_files"].as_array_mut().unwrap();
            untracked[0].as_object_mut().unwrap().remove("mode");
            let missing_mode: StoredManifest = serde_json::from_value(missing_mode).unwrap();
            assert!(
                recovery_archive_expected_files(&missing_mode)
                    .unwrap_err()
                    .contains("missing its Unix file mode")
            );
        }

        value["schema_version"] =
            serde_json::json!(PINNED_RETIREMENT_RECOVERY_ARCHIVE_SCHEMA_VERSION + 1);
        let future: StoredManifest = serde_json::from_value(value).unwrap();
        assert!(
            recovery_archive_expected_files(&future)
                .unwrap_err()
                .contains("not supported")
        );
    }

    #[test]
    fn schema_v2_archive_and_frozen_plan_digest_remain_readable() {
        let (_tmp, repo, mut plan) = staged_checkout();
        let policy = crate::AutoCleanupPolicy::load(repo.root()).unwrap();
        let proof = IgnoredPolicyProof {
            strict: policy.strict,
            regenerable: &policy.regenerable,
        };
        plan.ignored_policy_sha256 = sha256_hex(&serde_json::to_vec(&proof).unwrap());
        let legacy_digest = legacy_v2_cleanup_resolve_digest(&plan, &proof).unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap();

        let manifest_path = dir.path().join(MANIFEST_FILE);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        value["schema_version"] = serde_json::json!(PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION);
        value["plan_digest"] = serde_json::json!(legacy_digest);
        value.as_object_mut().unwrap().remove("branch_tip");
        value.as_object_mut().unwrap().remove("bundle_bytes");
        value.as_object_mut().unwrap().remove("bundle_sha256");
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();

        verify_recovery_archive_payload_for_plan(&repo, dir.path(), &plan).unwrap();
    }

    #[test]
    fn schema_v3_archive_with_frozen_v3_and_v4_digests_remains_readable() {
        let (_tmp, repo, mut plan) = staged_checkout();
        let policy = crate::AutoCleanupPolicy::load(repo.root()).unwrap();
        let proof = IgnoredPolicyProof {
            strict: policy.strict,
            regenerable: &policy.regenerable,
        };
        plan.ignored_policy_sha256 = sha256_hex(&serde_json::to_vec(&proof).unwrap());
        plan.ignored_paths.push(IgnoredPath {
            path: "old-cache/".into(),
            classification: IgnoredPathClassification::DisposableDirectory,
        });
        let digest_v3 = legacy_v3_cleanup_resolve_digest(&plan, &proof).unwrap();
        let digest_v4 = legacy_v4_cleanup_resolve_digest(&plan, &proof).unwrap();
        assert_ne!(digest_v3, digest_v4);
        let dir = tempfile::tempdir().unwrap();
        write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap();
        let path = dir.path().join(MANIFEST_FILE);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["schema_version"] =
            serde_json::json!(PREVIOUS_BUNDLE_RECOVERY_ARCHIVE_SCHEMA_VERSION);
        value.as_object_mut().unwrap().remove("branch_tip");
        for digest in [&digest_v4, &digest_v3] {
            value["plan_digest"] = serde_json::json!(digest);
            std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            verify_recovery_archive_payload_for_plan(&repo, dir.path(), &plan).unwrap();
            let mut added_directory = plan.clone();
            added_directory.ignored_paths.push(IgnoredPath {
                path: "new-cache/".into(),
                classification: IgnoredPathClassification::DisposableDirectory,
            });
            let result =
                verify_recovery_archive_payload_for_plan(&repo, dir.path(), &added_directory);
            if digest == &digest_v4 {
                result.unwrap();
            } else {
                assert!(
                    result.is_err(),
                    "the frozen v3 digest binds every ignored root"
                );
            }
        }

        let mut changed = plan.clone();
        changed.delivery_targets.push("different-target".into());
        assert!(verify_recovery_archive_payload_for_plan(&repo, dir.path(), &changed).is_err());
    }

    #[test]
    fn archive_ownership_rejects_patch_length_mismatch() {
        let (_tmp, repo, plan) = staged_checkout();
        let dir = tempfile::tempdir().unwrap();
        write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap();
        let manifest_path = dir.path().join(MANIFEST_FILE);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert!(plan.staged_patch_bytes > 0);
        value["staged_patch"]["bytes"] = serde_json::json!(0);
        let forged: StoredManifest = serde_json::from_value(value).unwrap();

        let error = verify_recovery_archive_tree(dir.path(), &forged).unwrap_err();
        assert!(
            error.contains("staged.patch byte length does not match"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn archive_ownership_rejects_file_mode_mismatch() {
        use std::os::unix::fs::PermissionsExt;

        let (_tmp, repo, plan) = staged_checkout();
        let dir = tempfile::tempdir().unwrap();
        write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap();
        let path = dir.path().join("untracked/new.txt");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let manifest: StoredManifest =
            serde_json::from_slice(&std::fs::read(dir.path().join(MANIFEST_FILE)).unwrap())
                .unwrap();

        let error = verify_recovery_archive_tree(dir.path(), &manifest).unwrap_err();
        assert!(error.contains("different file mode"), "{error}");
    }

    /// The patch written is re-hashed against the reviewed one, so a
    /// worktree that moved after review never yields an archive.
    #[test]
    fn a_patch_that_differs_from_the_plan_fails_verification() {
        let (_tmp, repo, mut plan) = staged_checkout();
        plan.staged_patch_sha256 = sha256_hex(b"something else");
        let dir = tempfile::tempdir().unwrap();
        let error = write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap_err();
        assert!(error.contains("staged.patch no longer matches"), "{error}");
        assert!(!dir.path().join(MANIFEST_FILE).exists());
    }

    /// Every copied file is read back and compared with what was reviewed.
    #[test]
    fn an_untracked_copy_that_differs_from_the_plan_fails_verification() {
        let (_tmp, repo, mut plan) = staged_checkout();
        plan.untracked_files[0].sha256 = sha256_hex(b"something else");
        let dir = tempfile::tempdir().unwrap();
        let error = write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap_err();
        assert!(error.contains("new.txt changed"), "{error}");
        assert!(!dir.path().join(MANIFEST_FILE).exists());
    }

    /// Patches are applied to the recorded head in a private index before the
    /// archive counts as complete.
    #[test]
    fn patches_that_do_not_apply_to_the_head_fail_verification() {
        let (tmp, repo, mut plan) = staged_checkout();
        // Record a head the staged patch was not taken against.
        std::fs::write(tmp.path().join("b.txt"), "b\n").unwrap();
        git(tmp.path(), &["add", "b.txt"]);
        git(tmp.path(), &["commit", "-qm", "b"]);
        git(tmp.path(), &["rm", "-q", "--cached", "a.txt"]);
        let staged = repo.recovery_patch(true).unwrap();
        plan.staged_patch_sha256 = sha256_hex(&staged);
        plan.staged_patch_bytes = staged.len() as u64;
        let dir = tempfile::tempdir().unwrap();
        let error = write_verified_archive(&repo, &repo, &plan, None, dir.path()).unwrap_err();
        assert!(error.contains("do not apply"), "{error}");
    }

    #[test]
    fn only_plain_relative_paths_are_contained() {
        assert!(is_contained_relative("src/lib.rs"));
        assert!(!is_contained_relative("../escape"));
        assert!(!is_contained_relative("/etc/passwd"));
        assert!(!is_contained_relative("a/../../b"));
        assert!(!is_contained_relative(""));
    }

    /// A link is archived as a link: its hash names the target, and nothing
    /// outside the worktree is read through it.
    #[test]
    fn a_symlink_is_described_by_its_target_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("secret"), "outside").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("secret"), tmp.path().join("link")).unwrap();
        let described = describe_untracked(&tmp.path().join("link"), "link")
            .unwrap()
            .unwrap();
        assert!(described.symlink);
        assert_eq!(described.bytes, 0);
        let mut expected = b"symlink\0".to_vec();
        expected.extend_from_slice(tmp.path().join("secret").as_os_str().as_encoded_bytes());
        assert_eq!(described.sha256, sha256_hex(&expected));
    }

    #[test]
    fn disposable_ignored_directory_added_after_archive_verification_does_not_stall_cleanup() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(
            repository.join(".gitignore"),
            "/.aethyme/\n/test-results/\n",
        )
        .unwrap();
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("archive disposable directory churn fixture", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("new.txt"), "recover me\n").unwrap();
        broker.close(session.id).unwrap();
        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        assert!(plan.ignored_files.is_empty());
        let digest = plan.digest.clone();
        let late_worktree = worktree.clone();
        let outcome = broker
            .cleanup_resolve_apply_with_post_archive_hook(session.id, &digest, move || {
                std::fs::create_dir(late_worktree.join("test-results")).unwrap();
                std::fs::write(
                    late_worktree.join("test-results/generated.txt"),
                    "generated",
                )
                .unwrap();
            })
            .unwrap();
        assert!(outcome.worktree_removed, "{outcome:?}");
        assert!(outcome.cleanup_error.is_none(), "{outcome:?}");
        assert!(!worktree.exists());
        assert_eq!(
            std::fs::read(outcome.archive.join("untracked/new.txt")).unwrap(),
            b"recover me\n"
        );
    }

    #[test]
    fn ignored_authorization_keeps_every_standalone_descriptor() {
        let ignored = vec![
            IgnoredPath {
                path: "cache/".into(),
                classification: IgnoredPathClassification::DisposableDirectory,
            },
            IgnoredPath {
                path: ".DS_Store".into(),
                classification: IgnoredPathClassification::DisposableOutput,
            },
            IgnoredPath {
                path: "private.txt".into(),
                classification: IgnoredPathClassification::PreservedFile,
            },
        ];
        assert_eq!(authorization_ignored_paths(&ignored), ignored[1..].to_vec());
    }

    #[test]
    fn ignored_content_added_after_archive_verification_keeps_the_worktree() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join(".gitignore"), "/.aethyme/\n/private.txt\n").unwrap();
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("archive cleanup recheck fixture", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("new.txt"), "recover me\n").unwrap();
        broker.close(session.id).unwrap();

        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        let digest = plan.digest.clone();
        let late_ignored = worktree.join("private.txt");
        let outcome = broker
            .cleanup_resolve_apply_with_post_archive_hook(session.id, &digest, move || {
                std::fs::write(&late_ignored, "preserve me\n").unwrap();
            })
            .unwrap();

        assert!(!outcome.worktree_removed);
        assert!(
            outcome
                .cleanup_error
                .as_deref()
                .is_some_and(|error| { error.contains("changed after archive verification") })
        );
        assert!(outcome.archive.join(MANIFEST_FILE).is_file());
        assert!(outcome.archive.join("untracked/new.txt").is_file());
        assert!(worktree.is_dir());
        assert_eq!(
            std::fs::read(worktree.join("private.txt")).unwrap(),
            b"preserve me\n"
        );
    }

    #[test]
    fn standalone_generated_output_added_after_archive_verification_keeps_the_worktree() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join(".gitignore"), "/.aethyme/\n/.DS_Store\n").unwrap();
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("archive cleanup recheck fixture", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("new.txt"), "recover me\n").unwrap();
        broker.close(session.id).unwrap();

        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        let digest = plan.digest.clone();
        let late_ignored = worktree.join(".DS_Store");
        let outcome = broker
            .cleanup_resolve_apply_with_post_archive_hook(session.id, &digest, move || {
                std::fs::write(&late_ignored, "preserve me\n").unwrap();
            })
            .unwrap();

        assert!(!outcome.worktree_removed);
        assert!(
            outcome
                .cleanup_error
                .as_deref()
                .is_some_and(|error| { error.contains("changed after archive verification") })
        );
        assert!(outcome.archive.join(MANIFEST_FILE).is_file());
        assert!(outcome.archive.join("untracked/new.txt").is_file());
        assert!(worktree.is_dir());
        assert_eq!(
            std::fs::read(worktree.join(".DS_Store")).unwrap(),
            b"preserve me\n"
        );
    }

    #[test]
    fn archive_changed_after_initial_readback_keeps_the_worktree() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join(".gitignore"), "/private.txt\n").unwrap();
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("archive recheck fixture", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("private.txt"), "reviewed data\n").unwrap();
        broker.close(session.id).unwrap();

        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        let archive_root = plan.archive_root.clone();
        let outcome = broker
            .cleanup_resolve_apply_with_post_archive_hook(session.id, &plan.digest, move || {
                let archive = std::fs::read_dir(&archive_root)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                std::fs::write(archive.join("ignored/private.txt"), "altered data\n").unwrap();
            })
            .unwrap();

        assert!(!outcome.worktree_removed);
        assert!(
            outcome
                .cleanup_error
                .as_deref()
                .is_some_and(|error| { error.contains("archive changed before worktree removal") })
        );
        assert!(worktree.is_dir());
        assert_eq!(
            std::fs::read(worktree.join("private.txt")).unwrap(),
            b"reviewed data\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn archive_mode_change_after_initial_readback_keeps_the_worktree() {
        use std::os::unix::fs::PermissionsExt;

        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join(".gitignore"), "/private.txt\n").unwrap();
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("archive mode recheck fixture", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        let protected = worktree.join("private.txt");
        std::fs::write(&protected, "reviewed data\n").unwrap();
        std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o640)).unwrap();
        broker.close(session.id).unwrap();

        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        let archive_root = plan.archive_root.clone();
        let outcome = broker
            .cleanup_resolve_apply_with_post_archive_hook(session.id, &plan.digest, move || {
                let archive = std::fs::read_dir(&archive_root)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                std::fs::set_permissions(
                    archive.join("ignored/private.txt"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap();
            })
            .unwrap();

        assert!(!outcome.worktree_removed);
        assert!(
            outcome
                .cleanup_error
                .as_deref()
                .is_some_and(|error| error.contains("archive changed before worktree removal"))
        );
        assert!(worktree.is_dir());
        assert_eq!(
            std::fs::metadata(protected).unwrap().permissions().mode() & 0o7777,
            0o640
        );
    }

    #[test]
    fn archive_capacity_refusal_queries_configured_destination_before_staging() {
        for (case, headroom) in [
            (
                "insufficient",
                Some(crate::disk_headroom::DiskHeadroom {
                    bytes: 0,
                    inodes: None,
                }),
            ),
            ("unknown", None),
        ] {
            let owner = tempfile::tempdir().unwrap();
            let repository = owner.path().join("repository");
            std::fs::create_dir(&repository).unwrap();
            git(&repository, &["init", "-q", "-b", "main"]);
            std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
            git(&repository, &["add", "-A"]);
            git(&repository, &["commit", "-qm", "initial"]);

            let storage = crate::test_storage::TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let session = broker
                .start_worktree("archive capacity refusal fixture", None)
                .unwrap();
            let worktree = PathBuf::from(&session.worktree_path);
            let committed = worktree.join("committed.txt");
            std::fs::write(&committed, "pending commit\n").unwrap();
            git(&worktree, &["add", "committed.txt"]);
            git(&worktree, &["commit", "-qm", "pending"]);
            let source = worktree.join("preserve.txt");
            std::fs::write(&source, "retain before archive staging\n").unwrap();
            broker.close(session.id).unwrap();

            let plan = broker.cleanup_resolve_plan(session.id).unwrap();
            assert!(
                !plan.unlanded_commits.is_empty(),
                "fixture must require a commit bundle"
            );
            assert!(plan.archive_root.starts_with(storage.host_state_dir()));
            assert!(!plan.archive_root.starts_with(&repository));
            let expected_repository = broker.main_root().to_path_buf();
            let expected_archive_root = plan.archive_root.clone();
            let error = broker
                .cleanup_resolve_apply_with_hooks(
                    session.id,
                    &plan.digest,
                    || panic!("capacity refusal must happen before archive verification"),
                    sync_archive_directory,
                    move |repository_root, destination| {
                        assert_eq!(repository_root, expected_repository);
                        assert_eq!(destination, expected_archive_root);
                        headroom
                    },
                )
                .unwrap_err()
                .to_string();

            assert!(
                error.contains("archive capacity preflight failed"),
                "{case}: {error}"
            );
            assert!(worktree.is_dir(), "{case}");
            assert_eq!(
                std::fs::read(&source).unwrap(),
                b"retain before archive staging\n",
                "{case}"
            );
            assert_eq!(
                std::fs::read_dir(&plan.archive_root).unwrap().count(),
                0,
                "{case}: no partial archive or payload should be written"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn bundle_limit_refusal_keeps_source_and_bounded_partial_archive() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("bundle cap refusal fixture", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        let committed = worktree.join("pending.txt");
        std::fs::write(&committed, "pending commit data\n").unwrap();
        git(&worktree, &["add", "pending.txt"]);
        git(&worktree, &["commit", "-qm", "pending"]);
        broker.close(session.id).unwrap();

        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        assert!(!plan.unlanded_commits.is_empty());
        let error = broker
            .cleanup_resolve_apply_with_bundle_writer(
                session.id,
                &plan.digest,
                || panic!("bundle refusal must happen before archive verification"),
                sync_archive_directory,
                |_, _| {
                    Some(crate::disk_headroom::DiskHeadroom {
                        bytes: u64::MAX,
                        inodes: None,
                    })
                },
                RecoveryBundleWriter {
                    max_bytes: 8,
                    write_to: write_over_cap_bundle,
                },
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("hard 8-byte cap"), "{error}");
        assert!(worktree.is_dir());
        assert_eq!(std::fs::read(&committed).unwrap(), b"pending commit data\n");
        let entries = std::fs::read_dir(&plan.archive_root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].extension().and_then(|ext| ext.to_str()),
            Some("partial")
        );
        let partial_bundle = entries[0].join(BUNDLE_FILE);
        assert_eq!(std::fs::metadata(&partial_bundle).unwrap().len(), 8);
        assert_eq!(std::fs::read(partial_bundle).unwrap(), b"12345678");
        assert!(!entries[0].join(MANIFEST_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn invalid_bundle_fails_payload_verification_after_manifest_and_keeps_checkout() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("invalid bundle verification fixture", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        let committed = worktree.join("pending.txt");
        std::fs::write(&committed, "pending commit data\n").unwrap();
        git(&worktree, &["add", "pending.txt"]);
        git(&worktree, &["commit", "-qm", "pending"]);
        broker.close(session.id).unwrap();

        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        assert!(!plan.unlanded_commits.is_empty());
        let error = broker
            .cleanup_resolve_apply_with_bundle_writer(
                session.id,
                &plan.digest,
                || panic!("an invalid bundle must fail before archive publication"),
                sync_archive_directory,
                |_, _| {
                    Some(crate::disk_headroom::DiskHeadroom {
                        bytes: u64::MAX,
                        inodes: None,
                    })
                },
                RecoveryBundleWriter {
                    max_bytes: MAX_RECOVERY_BUNDLE_BYTES,
                    write_to: write_invalid_bundle,
                },
            )
            .unwrap_err();
        let reason = match error {
            BrokerOpError::RecoveryArchiveUnverified { reason, .. } => reason,
            other => panic!("unexpected invalid-bundle refusal: {other}"),
        };
        assert!(reason.contains("Git bundle verify failed"), "{reason}");

        let entries = std::fs::read_dir(&plan.archive_root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1, "no final archive should be published");
        let partial = &entries[0];
        assert_eq!(
            partial.extension().and_then(|extension| extension.to_str()),
            Some("partial")
        );
        assert!(partial.join(MANIFEST_FILE).is_file());
        assert_eq!(
            std::fs::read(partial.join(BUNDLE_FILE)).unwrap(),
            b"not a Git bundle\n"
        );
        assert!(worktree.is_dir(), "the checkout must remain in place");
        assert_eq!(std::fs::read(&committed).unwrap(), b"pending commit data\n");
        assert_eq!(
            GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref(&plan.branch_ref),
            plan.branch_tip
        );
    }

    #[cfg(unix)]
    #[test]
    fn archive_root_sync_failure_keeps_the_original_worktree() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join(".gitignore"), "/private.txt\n").unwrap();
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("archive sync failure fixture", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        let protected = worktree.join("private.txt");
        std::fs::write(&protected, "keep until archive root is durable\n").unwrap();
        broker.close(session.id).unwrap();

        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        let expected_archive_root = plan.archive_root.clone();
        let error = broker
            .cleanup_resolve_apply_with_hooks(
                session.id,
                &plan.digest,
                || {},
                move |path| {
                    if path == expected_archive_root.as_path() {
                        Err(std::io::Error::other("injected archive root sync failure"))
                    } else {
                        sync_archive_directory(path)
                    }
                },
                |_, _| {
                    Some(crate::disk_headroom::DiskHeadroom {
                        bytes: u64::MAX,
                        inodes: None,
                    })
                },
            )
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("cannot sync archive root before worktree removal"),
            "{error}"
        );
        assert!(worktree.is_dir());
        assert_eq!(
            std::fs::read(&protected).unwrap(),
            b"keep until archive root is durable\n"
        );
        let archives = std::fs::read_dir(&plan.archive_root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(archives.len(), 1);
        let archived = broker.owned_recovery_archive(&archives[0]).unwrap();
        assert!(archived.holds_uncommitted);
    }

    #[test]
    fn archive_plan_refuses_tracked_submodules_and_preserves_the_checkout() {
        for child_state in ["clean", "modified", "untracked", "pointer"] {
            let owner = tempfile::tempdir().unwrap();
            let repository = owner.path().join("repository");
            let source = owner.path().join("submodule-source");
            std::fs::create_dir(&repository).unwrap();
            std::fs::create_dir(&source).unwrap();
            git(&source, &["init", "-q", "-b", "main"]);
            std::fs::write(source.join("tracked.txt"), "submodule source\n").unwrap();
            git(&source, &["add", "tracked.txt"]);
            git(&source, &["commit", "-qm", "submodule initial"]);

            git(&repository, &["init", "-q", "-b", "main"]);
            std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
            git(&repository, &["add", "README.md"]);
            git(&repository, &["commit", "-qm", "superproject initial"]);
            let source_path = source.to_str().unwrap();
            git(
                &repository,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "--quiet",
                    source_path,
                    "sub",
                ],
            );
            git(
                &repository,
                &["config", "-f", ".gitmodules", "submodule.sub.ignore", "all"],
            );
            git(&repository, &["add", "-A"]);
            git(&repository, &["commit", "-qm", "add submodule"]);
            git(&repository, &["config", "diff.ignoreSubmodules", "all"]);
            git(&repository, &["config", "submodule.sub.ignore", "all"]);

            let storage = crate::test_storage::TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let session = broker
                .start_worktree("dirty submodule archive refusal", None)
                .unwrap();
            let worktree = PathBuf::from(&session.worktree_path);
            git(
                &worktree,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "update",
                    "--init",
                    "--quiet",
                    "--",
                    "sub",
                ],
            );
            std::fs::write(worktree.join("outside.txt"), "archive-needed work\n").unwrap();
            let child = worktree.join("sub");
            match child_state {
                "clean" => {}
                "modified" => {
                    std::fs::write(child.join("tracked.txt"), "modified child content\n").unwrap();
                }
                "untracked" => {
                    std::fs::write(child.join("untracked.txt"), "untracked child content\n")
                        .unwrap();
                }
                "pointer" => {
                    std::fs::write(child.join("tracked.txt"), "second child commit\n").unwrap();
                    git(&child, &["add", "tracked.txt"]);
                    git(&child, &["commit", "-qm", "second child commit"]);
                }
                _ => unreachable!(),
            }
            broker.close(session.id).unwrap();

            let error = broker
                .cleanup_resolve_plan(session.id)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("does not include child Git repositories"),
                "{error}"
            );
            assert!(worktree.is_dir());
        }
    }

    #[cfg(unix)]
    #[test]
    fn protected_ignored_files_and_links_are_archived_and_block_gc_expiry() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        let newline_name = "private-line\nbreak";
        std::fs::write(
            repository.join(".gitignore"),
            "/private.txt\n/private-link\n/private*break\n",
        )
        .unwrap();
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("archive protected ignored paths", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("private.txt"), "preserve me\n").unwrap();
        std::fs::write(worktree.join(newline_name), "preserve newline name\n").unwrap();
        std::os::unix::fs::symlink("../outside-secret", worktree.join("private-link")).unwrap();
        broker.close(session.id).unwrap();

        let plan = broker.cleanup_resolve_plan(session.id).unwrap();
        assert_eq!(plan.ignored_files.len(), 3);
        assert_eq!(
            plan.ignored_bytes,
            (b"preserve me\n".len() + b"preserve newline name\n".len()) as u64
        );
        assert_eq!(plan.ignored_paths.len(), 3);
        assert!(
            plan.ignored_paths
                .iter()
                .any(|path| path.path == newline_name)
        );
        assert!(
            plan.ignored_paths
                .iter()
                .all(|path| path.classification == IgnoredPathClassification::PreservedFile)
        );
        let outcome = broker
            .cleanup_resolve_apply(session.id, &plan.digest)
            .unwrap();
        assert!(outcome.worktree_removed);
        assert_eq!(
            std::fs::read(outcome.archive.join("ignored/private.txt")).unwrap(),
            b"preserve me\n"
        );
        assert_eq!(
            std::fs::read(outcome.archive.join("ignored").join(newline_name)).unwrap(),
            b"preserve newline name\n"
        );
        assert!(
            std::fs::symlink_metadata(outcome.archive.join("ignored/private-link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let archived = broker.owned_recovery_archive(&outcome.archive).unwrap();
        assert!(archived.holds_uncommitted);
        assert!(!broker.recovery_archive_landed(&archived));
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_archive_refuses_non_utf8_ignored_standalone_files() {
        use std::os::unix::ffi::OsStringExt;

        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join(".gitignore"), "/private-*\n").unwrap();
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("archive non-UTF-8 ignored file refusal", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        let mut filename = b"private-".to_vec();
        filename.push(0xff);
        let path = worktree.join(std::ffi::OsString::from_vec(filename));
        std::fs::write(&path, "preserve or refuse\n").unwrap();
        broker.close(session.id).unwrap();

        let error = broker
            .cleanup_resolve_plan(session.id)
            .unwrap_err()
            .to_string();
        assert!(error.contains("non-UTF-8"), "{error}");
        assert!(worktree.is_dir());
        assert!(path.is_file());
    }

    #[test]
    fn ignored_directories_stay_collapsed_and_strict_policy_refuses_leftovers() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path();
        git(repository, &["init", "-q", "-b", "main"]);
        std::fs::write(
            repository.join(".gitignore"),
            "/opaque-cache/\n/private.txt\n",
        )
        .unwrap();
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(repository, &["add", "-A"]);
        git(repository, &["commit", "-qm", "initial"]);
        std::fs::create_dir_all(repository.join("opaque-cache/nested")).unwrap();
        std::fs::write(repository.join("opaque-cache/nested/data"), "discardable").unwrap();
        #[cfg(unix)]
        assert!(
            std::process::Command::new("mkfifo")
                .arg(repository.join("opaque-cache/nested/private.pipe"))
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(repository.join("private.txt"), "preserve").unwrap();

        let checkout = GitRepo::discover(repository).unwrap();
        let default_policy = crate::AutoCleanupPolicy::default();
        let (roots, files) =
            ignored_inventory(&checkout, repository, &default_policy, 1, None).unwrap();
        assert!(roots.iter().any(|root| {
            root.path == "opaque-cache/"
                && root.classification == IgnoredPathClassification::DisposableDirectory
        }));
        assert!(
            !roots
                .iter()
                .any(|root| root.path.starts_with("opaque-cache/nested"))
        );
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "private.txt");

        let strict = crate::AutoCleanupPolicy {
            strict: true,
            ..Default::default()
        };
        assert!(ignored_inventory(&checkout, repository, &strict, 1, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ignored_special_files_fail_closed() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path();
        git(repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join(".gitignore"), "/private.pipe\n").unwrap();
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(repository, &["add", "-A"]);
        git(repository, &["commit", "-qm", "initial"]);
        let status = std::process::Command::new("mkfifo")
            .arg(repository.join("private.pipe"))
            .status()
            .unwrap();
        assert!(status.success());

        let checkout = GitRepo::discover(repository).unwrap();
        let error = ignored_inventory(
            &checkout,
            repository,
            &crate::AutoCleanupPolicy::default(),
            1,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("special file"), "{error}");
    }

    #[test]
    fn archive_copy_accepts_exact_size_and_never_writes_growth_probe() {
        use std::io::Cursor;

        let mut exact_source = Cursor::new(b"archive".to_vec());
        let mut exact_target = Vec::new();
        let digest =
            copy_expected_bytes_with_probe(&mut exact_source, &mut exact_target, 7).unwrap();
        assert_eq!(exact_target, b"archive");
        assert_eq!(digest, sha256_hex(b"archive"));

        let mut growing_source = Cursor::new(b"archive grew".to_vec());
        let mut growing_target = Vec::new();
        let error = copy_expected_bytes_with_probe(&mut growing_source, &mut growing_target, 7)
            .unwrap_err();
        assert!(error.to_string().contains("grew beyond"), "{error}");
        assert_eq!(growing_target, b"archive");
    }

    #[test]
    fn archive_capacity_keeps_the_storage_reserve_at_the_known_size_boundary() {
        let known = MAX_RECOVERY_MANIFEST_BYTES + MAX_RECOVERY_README_BYTES + 7;
        let required = known + crate::DEFAULT_GATE_HEADROOM_BYTES;
        assert!(require_recovery_archive_headroom(Some(required), known).is_ok());
        assert!(require_recovery_archive_headroom(Some(required - 1), known).is_err());
        assert!(require_recovery_archive_headroom(None, known).is_err());
        assert!(require_recovery_archive_headroom(Some(u64::MAX), u64::MAX).is_err());

        let mut total = u64::MAX;
        assert!(add_archive_size(&mut total, 1, "fixture").is_err());
        assert_eq!(total, u64::MAX);
        let overflowing_files = [
            ArchivedFile {
                path: "large".into(),
                bytes: u64::MAX,
                sha256: String::new(),
                symlink: false,
                mode: None,
            },
            ArchivedFile {
                path: "extra".into(),
                bytes: 1,
                sha256: String::new(),
                symlink: false,
                mode: None,
            },
        ];
        assert!(checked_archived_file_bytes(&overflowing_files).is_none());
    }

    #[test]
    fn recovery_manifest_serializer_refuses_past_its_fixture_limit() {
        let value = "archive manifest";
        let expected = serde_json::to_vec_pretty(&value).unwrap();
        assert_eq!(
            serialize_recovery_manifest(&value, expected.len()).unwrap(),
            expected
        );
        assert!(serialize_recovery_manifest(&value, expected.len() - 1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn archive_capacity_counts_symlink_target_bytes_and_metadata_allowances() {
        let (owner, _checkout, mut plan) = staged_checkout();
        let target = "relative-target";
        let link_path = owner.path().join("archive-link");
        std::os::unix::fs::symlink(target, &link_path).unwrap();
        let link = describe_untracked(&link_path, "archive-link")
            .unwrap()
            .unwrap();
        plan.untracked_files.push(link);

        let expected = plan.staged_patch_bytes
            + plan.unstaged_patch_bytes
            + plan.untracked_bytes
            + target.len() as u64
            + MAX_RECOVERY_MANIFEST_BYTES
            + MAX_RECOVERY_README_BYTES;
        assert_eq!(
            known_recovery_archive_payload_bytes(&plan).unwrap(),
            expected
        );
    }
}
