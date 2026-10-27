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

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::error::BrokerError;
use crate::git::GitRepo;
use crate::types::SessionOrigin;
use crate::{Broker, BrokerOpError, CleanupDisposition};

pub const CLEANUP_RESOLVE_SCHEMA_VERSION: u32 = 2;
const LEGACY_RECOVERY_ARCHIVE_SCHEMA_VERSION: u32 = 1;
const PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION: u32 = 2;
const RECOVERY_ARCHIVE_SCHEMA_VERSION: u32 = 3;
const MAX_RECOVERY_BUNDLE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_RECOVERY_BUNDLE_STDERR_BYTES: usize = 64 * 1024;
const MAX_RECOVERY_BUNDLE_METADATA_OUTPUT_BYTES: usize = 64 * 1024;

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
#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupResolvePlan {
    pub schema_version: u32,
    /// Binds the head, delivery targets, patches, untracked files, ignored
    /// root classifications, protected ignored files and cleanup policy.
    pub digest: String,
    pub session_id: i64,
    pub worktree_path: String,
    pub branch_ref: String,
    pub disposition: CleanupDisposition,
    /// Why cleanup alone refuses this worktree.
    pub reason: String,
    pub head: String,
    /// The commits cleanup judges against; the bundle excludes all of them.
    pub delivery_targets: Vec<String>,
    /// The shared landed-work verdict for `head` against the primary checkout.
    pub landing: crate::LandingVerdict,
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
    aethyme_version: &'static str,
    repository: String,
    session_id: i64,
    task: Option<&'a str>,
    worktree_path: &'a str,
    branch_ref: &'a str,
    disposition: &'a str,
    head: &'a str,
    /// Where `head` diverged from the primary checkout, when Git can say.
    base: Option<String>,
    delivery_targets: &'a [String],
    landing: &'a crate::LandingVerdict,
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
        schema_version: plan.schema_version,
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

/// The portable mode field is intentionally absent where Unix mode bits are
/// unavailable. Never guess a mode on another platform.
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

fn hash_regular_file_no_follow(path: &Path, max_bytes: u64) -> std::io::Result<(u64, String)> {
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
fn describe_untracked(full: &Path, relative: &str) -> std::io::Result<Option<ArchivedFile>> {
    let metadata = std::fs::symlink_metadata(full)?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(full)?;
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
) -> Result<(Vec<IgnoredPath>, Vec<ArchivedFile>), BrokerOpError> {
    let refuse = |reason: String| BrokerOpError::CleanupResolveRefused {
        id: session_id,
        reason,
    };
    let mut roots = Vec::new();
    let mut files = Vec::new();
    for relative in checkout.ignored_paths_readonly()? {
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
                let Some(file) =
                    describe_untracked(&full, &relative).map_err(|error| io_error(&full, error))?
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
    reject_unarchivable_special_nodes(worktree, &opaque_directories).map_err(refuse)?;
    Ok((roots, files))
}

/// Git does not report every unsupported filesystem node (for example, an
/// ignored FIFO). Walk only outside policy-approved opaque ignored directories
/// and refuse if the bounded, no-follow scan cannot establish that the tree
/// contains no special nodes.
fn reject_unarchivable_special_nodes(
    worktree: &Path,
    opaque_directories: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let root_metadata = std::fs::symlink_metadata(worktree)
        .map_err(|error| format!("cannot inspect worktree root: {error}"))?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err("worktree root is not a real directory".into());
    }

    let mut pending = vec![(PathBuf::new(), String::new(), 0_usize)];
    let mut visited = 0_usize;
    while let Some((relative_dir, relative_dir_string, depth)) = pending.pop() {
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
        if session.origin != SessionOrigin::Spawned {
            return Err(refuse(
                "only broker-spawned worktrees are archived and removed; an adopted checkout \
                 belongs to whoever adopted it"
                    .into(),
            ));
        }
        let mut records = crate::measurement::SizeRecords::default();
        let Some(item) =
            self.cleanup_item_scanned(&session, crate::SizeScan::Recorded, &mut records)?
        else {
            return Err(refuse(
                "nothing is retained: its worktree and branch are already gone".into(),
            ));
        };
        match item.disposition {
            CleanupDisposition::Eligible => {
                return Err(refuse(format!(
                    "cleanup can already remove it without an archive ({}); run \
                     `aethyme broker finish cleanup {session_id}`",
                    item.reason
                )));
            }
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
        self.refuse_live_checkout(session_id, &worktree)?;
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
        let delivery_targets = self.cleanup_delivery_targets()?;
        let unlanded_commits = checkout.commits_not_on_any(&head, &delivery_targets)?;
        let primary = self.repo_handle().head_commit()?;
        let landing = crate::work_landed(self.repo_handle(), &head, &primary)?;
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

        let mut untracked_files = Vec::new();
        for relative in checkout.untracked_paths()? {
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
            match describe_untracked(&full, &relative).map_err(|error| io_error(&full, error))? {
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
        let (ignored_paths, ignored_files) =
            ignored_inventory(&checkout, &worktree, &ignored_policy, session_id)?;
        let ignored_bytes = checked_archived_file_bytes(&ignored_files)
            .ok_or_else(|| refuse("the total ignored archive size overflows u64".into()))?;
        let ignored_policy_bytes =
            serde_json::to_vec(&ignored_policy_proof).map_err(|error| refuse(error.to_string()))?;
        let ignored_policy_sha256 = sha256_hex(&ignored_policy_bytes);

        let digest_input = DigestInputV3 {
            schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
            recovery_archive_schema_version: RECOVERY_ARCHIVE_SCHEMA_VERSION,
            max_recovery_bundle_bytes: MAX_RECOVERY_BUNDLE_BYTES,
            session_id,
            worktree_path: &session.worktree_path,
            disposition: item.disposition.as_str(),
            head: &head,
            delivery_targets: &delivery_targets,
            unlanded_commits: &unlanded_commits,
            staged_patch_sha256: &staged_patch_sha256,
            unstaged_patch_sha256: &unstaged_patch_sha256,
            untracked_files: &untracked_files,
            ignored_paths: &ignored_paths,
            ignored_files: &ignored_files,
            ignored_policy: &ignored_policy_proof,
        };
        let mut hasher = Sha256::new();
        hasher.update(b"aethyme-cleanup-resolve-v3\0");
        hasher
            .update(serde_json::to_vec(&digest_input).map_err(|error| refuse(error.to_string()))?);
        let digest = format!("{:x}", hasher.finalize());

        Ok(CleanupResolvePlan {
            schema_version: CLEANUP_RESOLVE_SCHEMA_VERSION,
            apply_command: format!(
                "aethyme broker finish cleanup resolve {session_id} --archive --confirm {digest}"
            ),
            discard_command: item.force_cleanup_command.clone(),
            digest,
            session_id,
            worktree_path: session.worktree_path.clone(),
            branch_ref: item.branch_ref,
            disposition: item.disposition,
            reason: item.reason,
            head,
            delivery_targets,
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
        B: Fn(&GitRepo, &[String], u64, Instant, &mut std::fs::File) -> Result<u64, String>,
    {
        if confirm.len() != 64 || !confirm.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BrokerOpError::CleanupResolveConfirmationNotSha256);
        }
        let plan = self.cleanup_resolve_plan(session_id)?;
        if plan.digest != confirm {
            return Err(BrokerOpError::CleanupResolveConfirmationMismatch {
                id: session_id,
                actual: confirm.to_owned(),
            });
        }
        let session = self.store_ref().session(session_id)?;
        let archive_root = prepare_archive_root(&plan.archive_root).map_err(|reason| {
            BrokerOpError::RecoveryArchiveUnverified {
                id: session_id,
                path: plan.archive_root.to_string_lossy().into_owned(),
                reason,
            }
        })?;
        let known_payload_bytes =
            known_recovery_archive_payload_bytes(&plan).map_err(|reason| {
                BrokerOpError::CleanupResolveRefused {
                    id: session_id,
                    reason: format!("archive capacity preflight failed: {reason}"),
                }
            })?;
        let destination_headroom =
            headroom_for(self.main_root(), &archive_root).map(|headroom| headroom.bytes);
        require_recovery_archive_headroom(destination_headroom, known_payload_bytes).map_err(
            |reason| BrokerOpError::CleanupResolveRefused {
                id: session_id,
                reason: format!("archive capacity preflight failed: {reason}"),
            },
        )?;
        let name = format!(
            "{session_id}-{}-{}",
            &plan.head[..12],
            archive_staging_nonce()
        );
        let partial_name = format!("{name}.partial");
        let partial_path = archive_root.join(&partial_name);
        let partial =
            create_exclusive_archive_directory(&archive_root, &partial_name).map_err(|reason| {
                BrokerOpError::RecoveryArchiveUnverified {
                    id: session_id,
                    path: partial_path.to_string_lossy().into_owned(),
                    reason,
                }
            })?;
        let archive = archive_root.join(&name);
        let checkout = GitRepo::discover(Path::new(&plan.worktree_path))?;
        if let Err(reason) = write_verified_archive_with_bundle_writer(
            self.repo_handle(),
            &checkout,
            &plan,
            session.task.as_deref(),
            &partial,
            &bundle,
            &sync_directory,
        ) {
            return Err(BrokerOpError::RecoveryArchiveUnverified {
                id: session_id,
                path: partial.to_string_lossy().into_owned(),
                reason,
            });
        }
        match std::fs::symlink_metadata(&archive) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(BrokerOpError::RecoveryArchiveUnverified {
                    id: session_id,
                    path: archive.to_string_lossy().into_owned(),
                    reason: "archive destination already exists".into(),
                });
            }
            Err(error) => return Err(io_error(&archive, error)),
        }
        std::fs::rename(&partial, &archive).map_err(|error| io_error(&archive, error))?;
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
        after_archive_verified();

        // The archive is complete and verified. Cleanup recomputes the full
        // plan under the lifecycle claim immediately before removing the tree.
        let cleanup_error = self
            .cleanup_after_recovery_archive(session_id, &plan.digest, &archive)
            .err()
            .map(|e| e.to_string());
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
    aethyme_version: String,
    repository: String,
    session_id: i64,
    task: Option<String>,
    worktree_path: String,
    branch_ref: String,
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
    /// Absent only in legacy schema v1. `None` in schema v2/v3 is incomplete and
    /// therefore cannot establish that an archive holds no ignored data.
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
pub(crate) struct OwnedRecoveryArchive {
    pub path: PathBuf,
    pub session_id: i64,
    pub head: String,
    pub created_at_ms: i64,
    /// Staged, unstaged, untracked or protected ignored changes -- work no
    /// commit carries, so no delivery target can ever hold it.
    pub holds_uncommitted: bool,
}

/// This repository's archive directory and what is in it.
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
    if !plan.unlanded_commits.is_empty() {
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
        RECOVERY_ARCHIVE_SCHEMA_VERSION => match manifest.bundle.as_deref() {
            Some(_) => {
                let Some(bytes) = manifest.bundle_bytes else {
                    return Err("schema v3 bundle is missing its byte length".into());
                };
                if bytes == 0 || bytes > MAX_RECOVERY_BUNDLE_BYTES {
                    return Err("schema v3 bundle length is outside its supported bound".into());
                }
                if !manifest.bundle_sha256.as_deref().is_some_and(valid_sha256) {
                    return Err("schema v3 bundle is missing a valid SHA-256".into());
                }
            }
            None => {
                if manifest.bundle_bytes.is_some() || manifest.bundle_sha256.is_some() {
                    return Err(
                        "schema v3 manifest has bundle integrity fields without a bundle".into(),
                    );
                }
            }
        },
        version => return Err(format!("manifest schema {version} is not supported")),
    }

    if manifest.session_id <= 0
        || manifest.created_at_ms < 0
        || manifest.aethyme_version.is_empty()
        || manifest.worktree_path.is_empty()
        || !Path::new(&manifest.worktree_path).is_absolute()
        || manifest.branch_ref.is_empty()
        || !matches!(
            manifest.disposition.as_str(),
            "dirty" | "pending_commits" | "unproven_provenance"
        )
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
        || !manifest
            .delivery_targets
            .iter()
            .all(|target| valid_git_object_id(target))
        || !manifest
            .unlanded_commits
            .iter()
            .all(|commit| valid_git_object_id(commit))
        || manifest.bundle.is_some() == manifest.unlanded_commits.is_empty()
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
        PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION | RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            #[cfg(unix)]
            if files
                .iter()
                .any(|file| !file.symlink && file.mode.is_none())
            {
                return Err(format!(
                    "schema v2/v3 {label} entry is missing its Unix file mode"
                ));
            }
            #[cfg(not(unix))]
            if files.iter().any(|file| file.mode.is_some()) {
                return Err(format!(
                    "schema v2/v3 {label} file modes cannot be verified on this platform"
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
        PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION | RECOVERY_ARCHIVE_SCHEMA_VERSION => {
            if manifest.ignored_directory.as_deref() != Some(IGNORED_DIRECTORY) {
                return Err(
                    "schema v2/v3 manifest is missing its ignored directory identity".into(),
                );
            }
            validate_archived_file_modes(
                "untracked",
                &manifest.untracked_files,
                manifest.schema_version,
            )?;
            let files = manifest
                .ignored_files
                .as_deref()
                .ok_or_else(|| "schema v2/v3 manifest is missing ignored_files".to_string())?;
            validate_archived_files("ignored", files)?;
            validate_archived_file_modes("ignored", files, manifest.schema_version)?;
            let ignored_paths = manifest
                .ignored_paths
                .as_deref()
                .ok_or_else(|| "schema v2/v3 manifest is missing ignored_paths".to_string())?;
            if !manifest
                .ignored_policy_sha256
                .as_deref()
                .is_some_and(valid_sha256)
                || !manifest.plan_digest.as_deref().is_some_and(valid_sha256)
            {
                return Err(
                    "schema v2/v3 manifest has an invalid plan or ignored policy identity".into(),
                );
            }
            let mut previous: Option<&str> = None;
            let mut preserved = Vec::new();
            for path in ignored_paths {
                let trimmed = path.path.trim_end_matches('/');
                if !is_contained_relative(trimmed)
                    || previous.is_some_and(|previous| previous >= path.path.as_str())
                {
                    return Err("schema v2/v3 ignored roots are invalid or unsorted".into());
                }
                if path.classification == IgnoredPathClassification::PreservedFile {
                    if path.path.ends_with('/') {
                        return Err("schema v2/v3 preserved ignored path is a directory".into());
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
fn recovery_archive_file_set(root: &Path) -> Result<std::collections::BTreeSet<PathBuf>, String> {
    let mut files = std::collections::BTreeSet::new();
    let mut pending = vec![(PathBuf::new(), 0_usize)];
    let mut visited = 0_usize;
    while let Some((relative_dir, depth)) = pending.pop() {
        if depth > MAX_RECOVERY_ARCHIVE_TREE_DEPTH {
            return Err("archive directory depth exceeds the ownership check bound".into());
        }
        let directory = root.join(&relative_dir);
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        for entry in entries {
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
    validate_manifest_identity(manifest)?;
    let expected = recovery_archive_expected_files(manifest)?;
    let actual = recovery_archive_file_set(root)?;
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
                    PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION | RECOVERY_ARCHIVE_SCHEMA_VERSION
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
    if manifest.schema_version == RECOVERY_ARCHIVE_SCHEMA_VERSION
        && let Some(bundle) = manifest.bundle.as_deref()
    {
        let path = root.join(bundle);
        let (bytes, sha256) = hash_regular_file_no_follow(&path, MAX_RECOVERY_BUNDLE_BYTES)
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
    verify_recovery_archive_payload_for_plan(repository, root, plan)
}

/// Verify archive payload and manifest against a plan without imposing a
/// final archive path. The writer uses this only for its owned `.partial`
/// directory; the final removal path uses the stricter wrapper above.
fn verify_recovery_archive_payload_for_plan(
    repository: &GitRepo,
    root: &Path,
    plan: &CleanupResolvePlan,
) -> Result<(), String> {
    let bytes = read_recovery_manifest(&root.join(MANIFEST_FILE))?;
    let manifest: StoredManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("unreadable {MANIFEST_FILE}: {error}"))?;
    validate_manifest_identity(&manifest)?;
    verify_recovery_archive_tree(root, &manifest)?;
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
        RECOVERY_ARCHIVE_SCHEMA_VERSION => Some(plan.digest.clone()),
        _ => None,
    };
    let expected_landing = serde_json::to_value(&plan.landing)
        .map_err(|error| format!("cannot encode reviewed landing verdict: {error}"))?;
    if expected_plan_digest
        .as_deref()
        .is_none_or(|expected| manifest.plan_digest.as_deref() != Some(expected))
        || manifest.session_id != plan.session_id
        || !same_path(Path::new(&manifest.repository), repository.root())
        || manifest.worktree_path != plan.worktree_path
        || manifest.branch_ref != plan.branch_ref
        || manifest.disposition != plan.disposition.as_str()
        || manifest.head != plan.head
        || manifest.delivery_targets != plan.delivery_targets
        || manifest.unlanded_commits != plan.unlanded_commits
        || manifest.landing != expected_landing
        || manifest.staged_patch.bytes != plan.staged_patch_bytes
        || manifest.staged_patch.sha256 != plan.staged_patch_sha256
        || manifest.unstaged_patch.bytes != plan.unstaged_patch_bytes
        || manifest.unstaged_patch.sha256 != plan.unstaged_patch_sha256
        || manifest.untracked_files != plan.untracked_files
        || manifest.ignored_paths.as_deref() != Some(plan.ignored_paths.as_slice())
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
        let actual = describe_untracked(&path, name)
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
            let path = root.join(directory).join(&expected.path);
            let actual = describe_untracked(&path, &expected.path)
                .map_err(|error| format!("cannot verify {}: {error}", path.display()))?
                .ok_or_else(|| format!("{} is not a regular file or symlink", path.display()))?;
            if &actual != expected {
                return Err(format!("{} differs from the reviewed plan", path.display()));
            }
        }
    }
    if manifest.bundle.is_some() {
        let bundle = root.join(BUNDLE_FILE);
        let deadline = recovery_bundle_deadline()?;
        verify_recovery_bundle(repository, &bundle, &plan.head, deadline)?;
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
                | RECOVERY_ARCHIVE_SCHEMA_VERSION
        ) {
            return Err(format!(
                "manifest schema {} is not supported",
                manifest.schema_version
            ));
        }
        validate_manifest_identity(&manifest)?;
        verify_recovery_archive_tree(dir, &manifest)?;
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
        Ok(OwnedRecoveryArchive {
            path: dir.to_path_buf(),
            session_id: manifest.session_id,
            head: manifest.head,
            created_at_ms: manifest.created_at_ms,
            holds_uncommitted: manifest.staged_patch.bytes > 0
                || manifest.unstaged_patch.bytes > 0
                || !manifest.untracked_files.is_empty()
                || manifest
                    .ignored_files
                    .as_deref()
                    .is_some_and(|files| !files.is_empty()),
        })
    }

    /// Whether nothing in `archive` exists only in it: its head reached the
    /// primary checkout or the upstream branch, and it holds no uncommitted
    /// changes. A head whose objects are gone cannot be shown to have landed,
    /// so it has not.
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
        targets.iter().any(|target| {
            matches!(
                crate::work_landed(repo, &archive.head, target),
                Ok(crate::LandingVerdict::Landed { .. })
            )
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
    max_bundle_bytes: u64,
    deadline: Instant,
    file: &mut std::fs::File,
) -> Result<u64, String> {
    let mut command = crate::git::git_command();
    command
        .args(["bundle", "create", "-", "HEAD", "--not"])
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

fn verify_recovery_bundle(
    repository: &GitRepo,
    bundle: &Path,
    expected_head: &str,
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
    let mut contains_head = false;
    let mut saw_entry = false;
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
        saw_entry = true;
        contains_head |= object == expected_head;
    }
    if !saw_entry || !contains_head {
        return Err(format!("{BUNDLE_FILE} does not hold {expected_head}"));
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
fn copy_expected_bytes_with_probe<R: Read, W: Write>(
    source: &mut R,
    target: &mut W,
    expected_bytes: u64,
) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut remaining = expected_bytes;
    while remaining > 0 {
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

    let mut probe = [0_u8; 1];
    if source.read(&mut probe)? != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "source grew beyond its reviewed byte length",
        ));
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn copy_regular_archive_file(
    source: &Path,
    archive_root: &Path,
    relative: &str,
    expected: &ArchivedFile,
) -> Result<(), String> {
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
    let copied_sha256 =
        copy_expected_bytes_with_probe(&mut source_file, &mut target_file, expected.bytes)
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

fn sync_archive_directories_bottom_up<S>(root: &Path, sync_directory: &S) -> Result<(), String>
where
    S: Fn(&Path) -> std::io::Result<()>,
{
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    let mut directories = Vec::new();
    let mut visited = 0_usize;
    while let Some((directory, depth)) = pending.pop() {
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
    )
}

struct RecoveryBundleWriter<B> {
    max_bytes: u64,
    write_to: B,
}

fn write_verified_archive_with_bundle_writer<S, B>(
    repository: &GitRepo,
    checkout: &GitRepo,
    plan: &CleanupResolvePlan,
    task: Option<&str>,
    dir: &Path,
    bundle: &RecoveryBundleWriter<B>,
    sync_directory: &S,
) -> Result<(), String>
where
    S: Fn(&Path) -> std::io::Result<()>,
    B: Fn(&GitRepo, &[String], u64, Instant, &mut std::fs::File) -> Result<u64, String>,
{
    require_real_directory(dir)?;
    let dir = dir
        .canonicalize()
        .map_err(|error| format!("cannot resolve archive staging directory: {error}"))?;
    require_real_directory(&dir)?;

    // Patches are taken again rather than trusted from the plan: the digest
    // check just passed, and a patch that no longer hashes the same means the
    // worktree moved in between.
    let mut patches = Vec::new();
    for (cached, file, expected) in [
        (true, STAGED_PATCH_FILE, &plan.staged_patch_sha256),
        (false, UNSTAGED_PATCH_FILE, &plan.unstaged_patch_sha256),
    ] {
        let bytes = checkout
            .recovery_patch(cached)
            .map_err(|error| format!("cannot take {file}: {error}"))?;
        let path = create_new_archive_file(&dir, file, &bytes)?;
        let written = describe_untracked(&path, file)
            .map_err(|error| format!("cannot safely read {file}: {error}"))?
            .ok_or_else(|| format!("{file} is not a regular file"))?;
        if written.bytes != bytes.len() as u64 || written.sha256 != *expected {
            return Err(format!("{file} no longer matches the reviewed plan"));
        }
        patches.push((written.bytes != 0).then_some(path));
    }

    let bundle_integrity = if plan.unlanded_commits.is_empty() {
        None
    } else {
        let deadline = recovery_bundle_deadline()?;
        let (path, mut bundle_file) = create_new_archive_file_handle(&dir, BUNDLE_FILE)?;
        let streamed_bytes = match (bundle.write_to)(
            checkout,
            &plan.delivery_targets,
            bundle.max_bytes,
            deadline,
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
        let (bytes, sha256) = hash_regular_file_no_follow(&path, bundle.max_bytes)
            .map_err(|error| format!("cannot read back {BUNDLE_FILE}: {error}"))?;
        if bytes != streamed_bytes {
            return Err(format!("{BUNDLE_FILE} byte count changed after streaming"));
        }
        verify_recovery_bundle(repository, &path, &plan.head, deadline)?;
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
            copy_regular_archive_file(&source, &untracked_root, &file.path, file)?;
        }
        let archived = describe_untracked(&target, &file.path)
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
            copy_regular_archive_file(&source, &ignored_root, &file.path, file)?;
        }
        let archived = describe_untracked(&target, &file.path)
            .map_err(|error| format!("cannot read back ignored {}: {error}", file.path))?;
        if archived.as_ref() != Some(file) {
            return Err(format!(
                "ignored {} changed while it was archived, or did not copy intact",
                file.path
            ));
        }
    }

    let manifest = RecoveryManifest {
        schema_version: RECOVERY_ARCHIVE_SCHEMA_VERSION,
        plan_digest: &plan.digest,
        created_at_ms: now_ms(),
        aethyme_version: env!("CARGO_PKG_VERSION"),
        repository: repository.root().to_string_lossy().into_owned(),
        session_id: plan.session_id,
        task,
        worktree_path: &plan.worktree_path,
        branch_ref: &plan.branch_ref,
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
    sync_archive_directories_bottom_up(&dir, sync_directory)?;
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
    format!(
        "# Recovery archive for broker session {id}\n\n\
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

    fn write_over_cap_bundle(
        _checkout: &GitRepo,
        _delivery_targets: &[String],
        max_bytes: u64,
        deadline: Instant,
        file: &mut std::fs::File,
    ) -> Result<u64, String> {
        let mut command = Command::new("sh");
        command.args(["-c", "printf 123456789"]);
        stream_recovery_command_stdout(&mut command, file, max_bytes, deadline)
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
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            branch_ref: "refs/heads/main".into(),
            disposition: CleanupDisposition::Dirty,
            reason: String::new(),
            head: head.clone(),
            delivery_targets: vec![head],
            landing: crate::LandingVerdict::NotLanded {
                examined: 0,
                truncated: false,
            },
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
            worktree_path: "/tmp/aethyme-v2".into(),
            branch_ref: "refs/heads/main".into(),
            disposition: CleanupDisposition::Dirty,
            reason: String::new(),
            head: "1".repeat(40),
            delivery_targets: vec!["2".repeat(40)],
            landing: crate::LandingVerdict::NotLanded {
                examined: 1,
                truncated: false,
            },
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
    }

    #[cfg(unix)]
    #[test]
    fn schema_v3_bundle_manifest_hashes_the_streamed_bundle() {
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
            digest: sha256_hex(b"v3 bundle fixture plan"),
            session_id: 9,
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            branch_ref: "refs/heads/main".into(),
            disposition: CleanupDisposition::Dirty,
            reason: "fixture".into(),
            head: head.clone(),
            delivery_targets: vec![delivered],
            landing: crate::LandingVerdict::NotLanded {
                examined: 1,
                truncated: false,
            },
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

        let manifest_v3: StoredManifest = serde_json::from_value(value.clone()).unwrap();
        verify_recovery_archive_payload_for_plan(&repo, dir.path(), &plan).unwrap();

        let legacy_digest = legacy_v2_cleanup_resolve_digest(&plan, &proof).unwrap();
        let mut legacy_value = value.clone();
        legacy_value["schema_version"] =
            serde_json::json!(PREVIOUS_RECOVERY_ARCHIVE_SCHEMA_VERSION);
        legacy_value["plan_digest"] = serde_json::json!(legacy_digest);
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
            verify_recovery_archive_tree(dir.path(), &manifest_v3)
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

        value["schema_version"] = serde_json::json!(CLEANUP_RESOLVE_SCHEMA_VERSION);
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

        value["schema_version"] = serde_json::json!(RECOVERY_ARCHIVE_SCHEMA_VERSION + 1);
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
        value.as_object_mut().unwrap().remove("bundle_bytes");
        value.as_object_mut().unwrap().remove("bundle_sha256");
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();

        verify_recovery_archive_payload_for_plan(&repo, dir.path(), &plan).unwrap();
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
        let (roots, files) = ignored_inventory(&checkout, repository, &default_policy, 1).unwrap();
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
        assert!(ignored_inventory(&checkout, repository, &strict, 1).is_err());
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
