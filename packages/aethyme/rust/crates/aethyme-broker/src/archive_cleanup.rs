//! Durable, bounded intent records for interrupted recovery-archive cleanup.
//!
//! A prepared intent is evidence to resume a cleanup only after callers have
//! revalidated its archive, worktree generation, Git identities, and residual
//! snapshot. This module does not authorize deletion by itself.

use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::atomic_file::write_synced_if_absent;

pub(crate) const ARCHIVE_CLEANUP_INTENT_SCHEMA_VERSION: u32 = 1;
pub(crate) const ARCHIVE_CLEANUP_POLICY_VERSION: u32 = 1;
const MAX_INTENT_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RESIDUAL_ENTRIES: usize = 50_000;
const MAX_RESIDUAL_PATH_BYTES: usize = 2 * 1024 * 1024;
const MAX_RELATIVE_PATH_BYTES: usize = 4096;
const MAX_DISPOSABLE_ROOTS: usize = 50_000;
const MAX_IDENTITY_STRING_BYTES: usize = 256 * 1024;

/// Immutable bindings captured before any archived worktree removal starts.
///
/// The record is deliberately separate from the recovery archive directory,
/// so writing it cannot change the archive manifest or its verified file set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArchiveCleanupIntent {
    pub(crate) schema_version: u32,
    pub(crate) policy_version: u32,
    pub(crate) session_id: i64,
    pub(crate) repository_key: String,
    pub(crate) repository_root: String,
    pub(crate) worktree_path: String,
    pub(crate) branch_ref: String,
    pub(crate) branch_tip: String,
    pub(crate) head: String,
    pub(crate) plan_digest: String,
    pub(crate) archive_path: String,
    pub(crate) archive_manifest_sha256: String,
    pub(crate) ignored_policy_sha256: String,
    pub(crate) root_generation_sha256: String,
    pub(crate) worktree_device: u64,
    pub(crate) worktree_inode: u64,
    pub(crate) git_pointer_device: u64,
    pub(crate) git_pointer_inode: u64,
    pub(crate) git_admin_path: String,
    pub(crate) git_admin_device: u64,
    pub(crate) git_admin_inode: u64,
    pub(crate) git_common_path: String,
    pub(crate) git_common_device: u64,
    pub(crate) git_common_inode: u64,
    pub(crate) mount_identity_sha256: String,
    /// Only policy-classified ignored directories may be omitted from the
    /// residual snapshot. Standalone ignored files must remain explicit.
    pub(crate) disposable_ignored_directory_roots: Vec<String>,
    /// Complete pre-removal snapshot of the entries that must be retained.
    pub(crate) residual_entries: Vec<ArchiveCleanupResidualEntry>,
}

/// One non-disposable entry captured from the worktree before removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArchiveCleanupResidualEntry {
    /// Normalized, slash-separated path relative to the worktree root.
    pub(crate) path: String,
    pub(crate) kind: ArchiveCleanupResidualKind,
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) mode: u32,
    pub(crate) bytes: u64,
    /// SHA-256 of regular-file bytes or symlink target bytes. Directories
    /// have no content digest; their children are separate entries.
    pub(crate) sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArchiveCleanupResidualKind {
    RegularFile,
    Symlink,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IntentWriteError {
    Invalid(String),
    Existing,
    Io(String),
}

impl ArchiveCleanupIntent {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != ARCHIVE_CLEANUP_INTENT_SCHEMA_VERSION
            || self.policy_version != ARCHIVE_CLEANUP_POLICY_VERSION
        {
            return Err("archive cleanup intent schema or policy version is unsupported".into());
        }
        if self.session_id <= 0
            || !safe_key(&self.repository_key)
            || !absolute_path(&self.repository_root)
            || !absolute_path(&self.worktree_path)
            || !absolute_path(&self.archive_path)
            || !valid_git_oid(&self.head)
            || !valid_git_oid(&self.branch_tip)
            || !valid_sha256(&self.plan_digest)
            || !valid_sha256(&self.archive_manifest_sha256)
            || !valid_sha256(&self.ignored_policy_sha256)
            || !crate::root_generation::valid_digest(&self.root_generation_sha256)
            || !valid_sha256(&self.mount_identity_sha256)
            || self.branch_ref.is_empty()
            || self.branch_ref.contains('\0')
            || !absolute_path(&self.git_admin_path)
            || !absolute_path(&self.git_common_path)
            || self.worktree_device == 0
            || self.worktree_inode == 0
            || self.git_pointer_device == 0
            || self.git_pointer_inode == 0
            || self.git_admin_device == 0
            || self.git_admin_inode == 0
            || self.git_common_device == 0
            || self.git_common_inode == 0
        {
            return Err("archive cleanup intent identity is incomplete or malformed".into());
        }
        if self.residual_entries.len() > MAX_RESIDUAL_ENTRIES {
            return Err("archive cleanup residual manifest exceeds its entry bound".into());
        }
        if self.disposable_ignored_directory_roots.len() > MAX_DISPOSABLE_ROOTS {
            return Err("archive cleanup disposable-root list exceeds its entry bound".into());
        }
        let identity_string_bytes = [
            self.repository_key.as_str(),
            self.repository_root.as_str(),
            self.worktree_path.as_str(),
            self.branch_ref.as_str(),
            self.branch_tip.as_str(),
            self.head.as_str(),
            self.plan_digest.as_str(),
            self.archive_path.as_str(),
            self.archive_manifest_sha256.as_str(),
            self.ignored_policy_sha256.as_str(),
            self.root_generation_sha256.as_str(),
            self.git_admin_path.as_str(),
            self.git_common_path.as_str(),
            self.mount_identity_sha256.as_str(),
        ]
        .into_iter()
        .try_fold(0usize, |total, value| total.checked_add(value.len()))
        .ok_or_else(|| "archive cleanup identity byte count overflows".to_owned())?;
        if identity_string_bytes > MAX_IDENTITY_STRING_BYTES {
            return Err("archive cleanup identity fields exceed their byte bound".into());
        }

        let mut total_path_bytes = 0usize;
        let mut previous: Option<&str> = None;
        for entry in &self.residual_entries {
            if !contained_relative_path(&entry.path)
                || entry.path.len() > MAX_RELATIVE_PATH_BYTES
                || entry.device == 0
                || entry.inode == 0
                || entry.mode & !0o7777 != 0
            {
                return Err(format!(
                    "archive cleanup residual path {:?} has an invalid identity",
                    entry.path
                ));
            }
            total_path_bytes = total_path_bytes
                .checked_add(entry.path.len())
                .ok_or_else(|| "archive cleanup path-byte count overflows".to_owned())?;
            if total_path_bytes > MAX_RESIDUAL_PATH_BYTES {
                return Err("archive cleanup residual manifest exceeds its path-byte bound".into());
            }
            if previous.is_some_and(|prior| prior >= entry.path.as_str()) {
                return Err("archive cleanup residual paths are not unique and sorted".into());
            }
            let valid_content = match entry.kind {
                ArchiveCleanupResidualKind::RegularFile | ArchiveCleanupResidualKind::Symlink => {
                    entry.sha256.as_deref().is_some_and(valid_sha256)
                }
                ArchiveCleanupResidualKind::Directory => entry.bytes == 0 && entry.sha256.is_none(),
            };
            if !valid_content {
                return Err(format!(
                    "archive cleanup residual path {:?} has an invalid content identity",
                    entry.path
                ));
            }
            previous = Some(&entry.path);
        }

        let mut previous_root: Option<&str> = None;
        for root in &self.disposable_ignored_directory_roots {
            if !contained_relative_path(root)
                || root.len() > MAX_RELATIVE_PATH_BYTES
                || previous_root.is_some_and(|prior| prior >= root.as_str())
            {
                return Err("archive cleanup disposable roots are malformed or unsorted".into());
            }
            total_path_bytes = total_path_bytes
                .checked_add(root.len())
                .ok_or_else(|| "archive cleanup path-byte count overflows".to_owned())?;
            if total_path_bytes > MAX_RESIDUAL_PATH_BYTES {
                return Err("archive cleanup residual manifest exceeds its path-byte bound".into());
            }
            if self.residual_entries.iter().any(|entry| {
                entry.path == *root
                    || entry
                        .path
                        .strip_prefix(root)
                        .is_some_and(|suffix| suffix.starts_with('/'))
                    || (entry.kind != ArchiveCleanupResidualKind::Directory
                        && root
                            .strip_prefix(&entry.path)
                            .is_some_and(|suffix| suffix.starts_with('/')))
            }) {
                return Err(format!(
                    "archive cleanup residual manifest overlaps disposable directory {root:?}"
                ));
            }
            if previous_root.is_some_and(|prior| {
                root.strip_prefix(prior)
                    .is_some_and(|suffix| suffix.starts_with('/'))
            }) {
                return Err("archive cleanup disposable roots overlap".into());
            }
            previous_root = Some(root);
        }
        Ok(())
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("cannot encode archive cleanup intent: {error}"))?;
        if bytes.len() as u64 > MAX_INTENT_FILE_BYTES {
            return Err("archive cleanup intent exceeds its encoded byte bound".into());
        }
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_INTENT_FILE_BYTES {
            return Err("archive cleanup intent exceeds its read byte bound".into());
        }
        let intent: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("cannot decode archive cleanup intent: {error}"))?;
        intent.validate()?;
        Ok(intent)
    }
}

pub(crate) fn intent_path(directory: &Path, session_id: i64) -> Result<PathBuf, String> {
    if session_id <= 0 {
        return Err("archive cleanup intent session id must be positive".into());
    }
    Ok(directory.join(format!("{session_id}.json")))
}

/// Publish once; an existing file, directory, or symlink is never replaced.
pub(crate) fn write_new(
    path: &Path,
    intent: &ArchiveCleanupIntent,
) -> Result<(), IntentWriteError> {
    let bytes = intent.encode().map_err(IntentWriteError::Invalid)?;
    verify_private_parent(path).map_err(IntentWriteError::Io)?;
    match fs::symlink_metadata(path) {
        Ok(_) => return Err(IntentWriteError::Existing),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(IntentWriteError::Io(error.to_string())),
    }
    match write_synced_if_absent(path, &bytes) {
        Ok(true) => Ok(()),
        Ok(false) => Err(IntentWriteError::Existing),
        Err(error) => Err(IntentWriteError::Io(error.to_string())),
    }
}

/// Read a bounded, owner-owned regular intent file without following links.
pub(crate) fn read_bounded(path: &Path, deadline: Instant) -> Result<ArchiveCleanupIntent, String> {
    check_deadline(deadline)?;
    verify_private_parent(path)?;
    let path_metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect archive cleanup intent: {error}"))?;
    if !path_metadata.is_file() || path_metadata.file_type().is_symlink() {
        return Err("archive cleanup intent is not a regular file".into());
    }
    verify_owned_private(&path_metadata)?;
    if path_metadata.len() > MAX_INTENT_FILE_BYTES {
        return Err("archive cleanup intent exceeds its read byte bound".into());
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    {
        return Err("archive cleanup intent reads require no-follow file support".into());
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot safely open archive cleanup intent: {error}"))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect opened archive cleanup intent: {error}"))?;
    if !opened_metadata.is_file()
        || !same_file_identity(&path_metadata, &opened_metadata)
        || opened_metadata.len() != path_metadata.len()
    {
        return Err("archive cleanup intent changed while it was opened".into());
    }
    verify_owned_private(&opened_metadata)?;

    let capacity = usize::try_from(opened_metadata.len())
        .map_err(|_| "archive cleanup intent size does not fit memory bounds".to_owned())?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_deadline(deadline)?;
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read archive cleanup intent: {error}"))?;
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) > MAX_INTENT_FILE_BYTES as usize {
            return Err("archive cleanup intent grew beyond its read byte bound".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    check_deadline(deadline)?;
    if bytes.len() != capacity
        || file
            .metadata()
            .map_err(|error| format!("cannot recheck archive cleanup intent: {error}"))?
            .len()
            != opened_metadata.len()
    {
        return Err("archive cleanup intent changed while it was read".into());
    }
    ArchiveCleanupIntent::decode(&bytes)
}

fn check_deadline(deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        Err("archive cleanup intent inspection exceeded its time budget".into())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn verify_owned_private(metadata: &fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: geteuid has no pointer arguments or preconditions.
    let current_uid = unsafe { libc::geteuid() };
    if metadata.uid() != current_uid || metadata.mode() & 0o077 != 0 {
        return Err("archive cleanup intent is not privately owned by this user".into());
    }
    Ok(())
}

fn verify_private_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "archive cleanup intent has no parent directory".to_owned())?;
    let metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("cannot inspect archive cleanup intent directory: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("archive cleanup intent parent is not a regular directory".into());
    }
    verify_owned_private(&metadata)
}

#[cfg(not(unix))]
fn verify_owned_private(_metadata: &fs::Metadata) -> Result<(), String> {
    Err("archive cleanup intent ownership cannot be verified on this platform".into())
}

#[cfg(unix)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    false
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn safe_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn absolute_path(value: &str) -> bool {
    !value.is_empty() && !value.contains('\0') && Path::new(value).is_absolute()
}

fn contained_relative_path(value: &str) -> bool {
    if value.is_empty() || value.starts_with('/') || value.contains('\\') || value.contains('\0') {
        return false;
    }
    value
        .split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
}

#[cfg(test)]
mod tests {
    use super::{
        ARCHIVE_CLEANUP_INTENT_SCHEMA_VERSION, ARCHIVE_CLEANUP_POLICY_VERSION,
        ArchiveCleanupIntent, ArchiveCleanupResidualEntry, ArchiveCleanupResidualKind, intent_path,
        read_bounded, write_new,
    };
    use std::time::{Duration, Instant};

    fn sample_intent() -> ArchiveCleanupIntent {
        ArchiveCleanupIntent {
            schema_version: ARCHIVE_CLEANUP_INTENT_SCHEMA_VERSION,
            policy_version: ARCHIVE_CLEANUP_POLICY_VERSION,
            session_id: 42,
            repository_key: "repo-aabbccddeeff0011".into(),
            repository_root: "/repos/main".into(),
            worktree_path: "/worktrees/42".into(),
            branch_ref: "refs/heads/agent/task".into(),
            branch_tip: "a".repeat(40),
            head: "a".repeat(40),
            plan_digest: "b".repeat(64),
            archive_path: "/state/recovery-archives/repo/42-archive".into(),
            archive_manifest_sha256: "c".repeat(64),
            ignored_policy_sha256: "d".repeat(64),
            root_generation_sha256: "e".repeat(64),
            worktree_device: 1,
            worktree_inode: 2,
            git_pointer_device: 1,
            git_pointer_inode: 3,
            git_admin_path: "/repo/.git/worktrees/42".into(),
            git_admin_device: 1,
            git_admin_inode: 4,
            git_common_path: "/repo/.git".into(),
            git_common_device: 1,
            git_common_inode: 5,
            mount_identity_sha256: "f".repeat(64),
            disposable_ignored_directory_roots: vec!["target".into()],
            residual_entries: vec![ArchiveCleanupResidualEntry {
                path: "src/main.rs".into(),
                kind: ArchiveCleanupResidualKind::RegularFile,
                device: 1,
                inode: 7,
                mode: 0o644,
                bytes: 5,
                sha256: Some("0".repeat(64)),
            }],
        }
    }

    #[test]
    fn intent_round_trip_preserves_archive_generation_and_residual_bindings() {
        let directory = tempfile::tempdir().unwrap();
        let path = intent_path(directory.path(), 42).unwrap();
        let intent = sample_intent();
        write_new(&path, &intent).unwrap();
        let decoded = read_bounded(&path, Instant::now() + Duration::from_secs(2)).unwrap();
        assert_eq!(decoded, intent);
    }

    #[test]
    fn existing_intent_is_never_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = intent_path(directory.path(), 42).unwrap();
        let intent = sample_intent();
        write_new(&path, &intent).unwrap();
        let existing = std::fs::read(&path).unwrap();
        assert_eq!(
            write_new(&path, &intent),
            Err(super::IntentWriteError::Existing)
        );
        assert_eq!(std::fs::read(&path).unwrap(), existing);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_intent_is_rejected_without_following_or_replacing_it() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("operator-choice.json");
        std::fs::write(&target, b"operator data").unwrap();
        let path = intent_path(directory.path(), 42).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(read_bounded(&path, Instant::now() + Duration::from_secs(2)).is_err());
        assert_eq!(
            write_new(&path, &sample_intent()),
            Err(super::IntentWriteError::Existing)
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"operator data");
    }

    #[test]
    fn unsupported_policy_versions_and_unknown_fields_fail_closed() {
        let value = serde_json::to_value(sample_intent()).unwrap();
        let mut unsupported = value.clone();
        unsupported["policy_version"] = serde_json::json!(999);
        assert!(ArchiveCleanupIntent::decode(&serde_json::to_vec(&unsupported).unwrap()).is_err());
        let mut unknown = value;
        unknown["future_authority"] = serde_json::json!(true);
        assert!(ArchiveCleanupIntent::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());
    }

    #[test]
    fn residual_paths_cannot_escape_or_overlap_disposable_directories() {
        let mut intent = sample_intent();
        intent.residual_entries[0].path = "../outside".into();
        assert!(intent.validate().is_err());

        let mut intent = sample_intent();
        intent.residual_entries[0].path = "target/new-file".into();
        assert!(intent.validate().is_err());
    }

    #[test]
    fn expired_and_oversized_intents_are_rejected_before_decode() {
        let directory = tempfile::tempdir().unwrap();
        let path = intent_path(directory.path(), 42).unwrap();
        std::fs::write(&path, vec![b'x'; 32 * 1024 * 1024 + 1]).unwrap();
        crate::host_state::protect_host_state_path(&path, false).unwrap();
        assert!(
            read_bounded(&path, Instant::now() + Duration::from_secs(2))
                .unwrap_err()
                .contains("read byte bound")
        );

        let small = directory.path().join("expired.json");
        write_new(&small, &sample_intent()).unwrap();
        assert!(read_bounded(&small, Instant::now() - Duration::from_millis(1)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn group_or_world_readable_intent_is_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = intent_path(directory.path(), 42).unwrap();
        write_new(&path, &sample_intent()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            read_bounded(&path, Instant::now() + Duration::from_secs(2))
                .unwrap_err()
                .contains("privately owned")
        );
    }
}
