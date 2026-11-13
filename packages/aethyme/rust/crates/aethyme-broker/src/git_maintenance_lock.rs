//! Reviewed recovery for one Git objects/maintenance.lock proven to predate
//! the current operating-system boot. This never invokes Git maintenance.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

const PLAN_SCHEMA_VERSION: u32 = 1;
const SAFETY_MARGIN_SECONDS: i64 = 60;
const LOCK_FILE_NAME: &str = "maintenance.lock";
const RECOVERY_DIRECTORY: &str = "git-maintenance-recovery";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlanStatus {
    Eligible,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Refusal {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GitMaintenanceLockPlan {
    pub schema_version: u32,
    pub action: &'static str,
    pub status: PlanStatus,
    pub eligible: bool,
    pub repository_root: Option<String>,
    pub git_common_dir: Option<String>,
    pub objects_dir: Option<String>,
    pub target: Option<String>,
    pub boot_proof: Option<BootProof>,
    pub file: Option<FileIdentity>,
    pub confirmation_sha256: Option<String>,
    pub refusals: Vec<Refusal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApplyStatus {
    Refused,
    Removed,
    RemovedReceiptFailure,
    RemovedButLockReappeared,
}

impl ApplyStatus {
    pub(crate) fn is_complete(&self) -> bool {
        matches!(self, Self::Removed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GitMaintenanceLockApplyReport {
    pub schema_version: u32,
    pub action: &'static str,
    pub status: ApplyStatus,
    pub confirmation_sha256: String,
    pub target: Option<String>,
    pub intent: Option<String>,
    pub receipt: Option<String>,
    pub target_unlinked: bool,
    pub objects_directory_synced: bool,
    pub receipt_written: bool,
    pub refusals: Vec<Refusal>,
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct Timestamp {
    seconds: i64,
    nanoseconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BootProof {
    source: String,
    boot_time: Timestamp,
    safe_before: Timestamp,
    safety_margin_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FileIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    links: u64,
    size: u64,
    modified: Timestamp,
    changed: Timestamp,
    birth: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DirectoryIdentity {
    canonical_path_hex: String,
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PlanBinding {
    schema_version: u32,
    repository: DirectoryIdentity,
    git_common_dir: DirectoryIdentity,
    objects_dir: DirectoryIdentity,
    lock_file_name: &'static str,
    file: FileIdentity,
    boot_proof: BootProof,
}

struct RepositoryContext {
    checkout: crate::GitRepo,
    repository_root: PathBuf,
    common_dir: PathBuf,
    objects_dir: PathBuf,
    repository_identity: DirectoryIdentity,
    common_identity: DirectoryIdentity,
    objects_identity: DirectoryIdentity,
}

fn refusal(code: &str, message: impl Into<String>) -> Refusal {
    Refusal {
        code: code.to_string(),
        message: message.into(),
    }
}

fn empty_plan() -> GitMaintenanceLockPlan {
    GitMaintenanceLockPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        action: "plan",
        status: PlanStatus::Refused,
        eligible: false,
        repository_root: None,
        git_common_dir: None,
        objects_dir: None,
        target: None,
        boot_proof: None,
        file: None,
        confirmation_sha256: None,
        refusals: Vec::new(),
    }
}

pub(crate) fn plan(cwd: &Path) -> GitMaintenanceLockPlan {
    collect_plan(cwd, read_boot_proof()).0
}

#[cfg(test)]
fn plan_with_boot(cwd: &Path, boot: Option<BootProof>) -> GitMaintenanceLockPlan {
    let result = boot.ok_or_else(|| "test supplied an unknown boot time".to_string());
    collect_plan(cwd, result).0
}

fn collect_plan(
    cwd: &Path,
    boot_result: Result<BootProof, String>,
) -> (GitMaintenanceLockPlan, Option<PlanBinding>) {
    let mut plan = empty_plan();

    if let Some(message) = crate::git::session_commit_git_environment_refusal() {
        plan.refusals
            .push(refusal("inherited_git_routing", message));
        return (plan, None);
    }

    let boot = match boot_result {
        Ok(boot) => {
            plan.boot_proof = Some(boot.clone());
            Some(boot)
        }
        Err(message) => {
            plan.refusals.push(refusal("boot_time_unknown", message));
            None
        }
    };

    let context = match capture_repository_context(cwd) {
        Ok(context) => context,
        Err(reason) => {
            plan.refusals.push(reason);
            return (plan, None);
        }
    };

    plan.repository_root = Some(context.repository_root.display().to_string());
    plan.git_common_dir = Some(context.common_dir.display().to_string());
    plan.objects_dir = Some(context.objects_dir.display().to_string());
    let target = context.objects_dir.join(LOCK_FILE_NAME);
    plan.target = Some(target.display().to_string());

    match unresolved_repository_wide_mutation(cwd) {
        Ok(true) => plan.refusals.push(refusal(
            "unresolved_repository_mutation",
            "a repository-wide coordinated Git mutation is still running or has an unknown outcome",
        )),
        Ok(false) => {}
        Err(message) => plan
            .refusals
            .push(refusal("repository_state_unavailable", message)),
    }

    let directory = match open_objects_directory(&context.objects_dir, &context.objects_identity) {
        Ok(directory) => directory,
        Err(reason) => {
            plan.refusals.push(reason);
            return (plan, None);
        }
    };
    let file = match inspect_lock_file(&directory, &target) {
        Ok(file) => {
            plan.file = Some(file.clone());
            file
        }
        Err(reason) => {
            plan.refusals.push(reason);
            return (plan, None);
        }
    };

    if file.size != 0 {
        plan.refusals.push(refusal(
            "lock_not_empty",
            "the maintenance lock contains bytes; it was retained",
        ));
    }
    let effective_uid = effective_uid();
    if file.uid != effective_uid {
        plan.refusals.push(refusal(
            "lock_owner_mismatch",
            "the maintenance lock is not owned by the current effective user",
        ));
    }
    if file.links != 1 {
        plan.refusals.push(refusal(
            "lock_link_count",
            "the maintenance lock does not have exactly one hard link",
        ));
    }
    if let Some(boot) = &boot {
        for (name, timestamp) in [("mtime", &file.modified), ("ctime", &file.changed)] {
            if timestamp >= &boot.safe_before {
                plan.refusals.push(refusal(
                    "lock_not_proven_preboot",
                    format!(
                        "the lock {name} is not strictly before the current boot minus the safety margin"
                    ),
                ));
            }
        }
        if let Some(birth) = &file.birth
            && birth >= &boot.safe_before
        {
            plan.refusals.push(refusal(
                "lock_not_proven_preboot",
                "the lock birth time is not strictly before the current boot minus the safety margin",
            ));
        }
    }

    if !plan.refusals.is_empty() {
        return (plan, None);
    }

    let binding = PlanBinding {
        schema_version: PLAN_SCHEMA_VERSION,
        repository: context.repository_identity,
        git_common_dir: context.common_identity,
        objects_dir: context.objects_identity,
        lock_file_name: LOCK_FILE_NAME,
        file,
        boot_proof: boot.expect("no boot-time refusal implies a boot proof"),
    };
    match binding_digest(&binding) {
        Ok(digest) => {
            plan.status = PlanStatus::Eligible;
            plan.eligible = true;
            plan.confirmation_sha256 = Some(digest);
            (plan, Some(binding))
        }
        Err(message) => {
            plan.refusals.push(refusal("plan_digest_failed", message));
            (plan, None)
        }
    }
}

fn capture_repository_context(cwd: &Path) -> Result<RepositoryContext, Refusal> {
    let checkout = crate::GitRepo::discover(cwd).map_err(|error| {
        refusal(
            "repository_unavailable",
            format!("cannot discover the current Git checkout: {error}"),
        )
    })?;
    let raw_root = checkout.main_root().map_err(|error| {
        refusal(
            "repository_root_unavailable",
            format!("cannot resolve the main checkout: {error}"),
        )
    })?;
    let (repository_root, repository_identity) =
        canonical_directory_identity(&raw_root).map_err(|message| {
            refusal(
                "repository_root_unavailable",
                format!("cannot establish the main checkout identity: {message}"),
            )
        })?;

    let main_checkout = crate::GitRepo::discover(&repository_root).map_err(|error| {
        refusal(
            "repository_unavailable",
            format!("cannot rediscover the canonical main checkout: {error}"),
        )
    })?;
    let raw_common = main_checkout.git_common_dir().map_err(|error| {
        refusal(
            "git_common_dir_unavailable",
            format!("cannot resolve the Git common directory: {error}"),
        )
    })?;
    let (common_dir, common_identity) =
        canonical_directory_identity(&raw_common).map_err(|message| {
            refusal(
                "git_common_dir_unavailable",
                format!("cannot establish the Git common-directory identity: {message}"),
            )
        })?;
    if common_dir.parent() != Some(repository_root.as_path()) {
        return Err(refusal(
            "repository_common_dir_mismatch",
            "the canonical Git common directory is not the main checkout's .git directory",
        ));
    }

    let raw_objects = common_dir.join("objects");
    let metadata = fs::symlink_metadata(&raw_objects).map_err(|error| {
        refusal(
            "objects_directory_unavailable",
            format!("cannot inspect the exact common-dir objects directory: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(refusal(
            "objects_directory_not_real",
            "the common-dir objects path is not a real directory",
        ));
    }
    let (objects_dir, objects_identity) =
        canonical_directory_identity(&raw_objects).map_err(|message| {
            refusal(
                "objects_directory_unavailable",
                format!("cannot establish the objects-directory identity: {message}"),
            )
        })?;
    if objects_dir != raw_objects {
        return Err(refusal(
            "objects_directory_redirected",
            "the common-dir objects path resolves through a different directory",
        ));
    }

    Ok(RepositoryContext {
        checkout,
        repository_root,
        common_dir,
        objects_dir,
        repository_identity,
        common_identity,
        objects_identity,
    })
}

fn canonical_directory_identity(path: &Path) -> Result<(PathBuf, DirectoryIdentity), String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("cannot canonicalize {}: {error}", path.display()))?;
    let directory = open_canonical_directory(&canonical)?;
    let _metadata = directory
        .metadata()
        .map_err(|error| format!("cannot fstat {}: {error}", canonical.display()))?;
    let _path_metadata = fs::metadata(&canonical)
        .map_err(|error| format!("cannot stat {}: {error}", canonical.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        if !_metadata.is_dir()
            || _metadata.dev() != _path_metadata.dev()
            || _metadata.ino() != _path_metadata.ino()
        {
            return Err(format!(
                "{} changed while resolving its directory identity",
                canonical.display()
            ));
        }
        Ok((
            canonical.clone(),
            DirectoryIdentity {
                canonical_path_hex: hex(canonical.as_os_str().as_bytes()),
                device: _metadata.dev(),
                inode: _metadata.ino(),
            },
        ))
    }
    #[cfg(not(unix))]
    {
        Err("native stale-lock recovery is unsupported on this platform".into())
    }
}

fn open_canonical_directory(_path: &Path) -> Result<File, String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let descriptor =
            crate::resources::managed_storage_inventory::open_absolute_directory_nofollow(_path)
                .map_err(|error| {
                    format!(
                        "cannot open directory {} without following links: {error}",
                        _path.display()
                    )
                })?;
        Ok(File::from(descriptor))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("native no-follow directory access is unsupported on this platform".into())
    }
}

fn open_objects_directory(path: &Path, expected: &DirectoryIdentity) -> Result<File, Refusal> {
    let directory = open_canonical_directory(path)
        .map_err(|message| refusal("objects_directory_unavailable", message))?;
    let metadata = directory.metadata().map_err(|error| {
        refusal(
            "objects_directory_unavailable",
            format!("cannot inspect the opened objects directory: {error}"),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !metadata.is_dir()
            || metadata.dev() != expected.device
            || metadata.ino() != expected.inode
        {
            return Err(refusal(
                "objects_directory_changed",
                "the no-follow objects-directory identity no longer matches the reviewed plan",
            ));
        }
    }
    Ok(directory)
}

fn unresolved_repository_wide_mutation(cwd: &Path) -> Result<bool, String> {
    let mut snapshot = crate::Broker::open_snapshot(cwd)
        .map_err(|error| format!("cannot read the broker operation snapshot: {error}"))?;
    let pending = snapshot
        .store()
        .pending_coordinated_operations()
        .map_err(|error| format!("cannot read unresolved coordinated operations: {error}"))?;
    Ok(pending.iter().any(|operation| {
        operation.provider == crate::OperationProvider::Git
            && operation.effect != crate::OperationEffect::Read
            && matches!(
                operation.status,
                crate::OperationStatus::Running | crate::OperationStatus::OutcomeUnknown
            )
    }))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn inspect_lock_file(directory: &File, path: &Path) -> Result<FileIdentity, Refusal> {
    use std::os::fd::AsRawFd;

    let name = CString::new(LOCK_FILE_NAME).expect("static file name has no NUL");
    let before = crate::resources::managed_storage_inventory::stat_at(
        directory.as_raw_fd(),
        &name,
        libc::AT_SYMLINK_NOFOLLOW,
    )
    .map_err(|error| {
        refusal(
            if error.kind() == io::ErrorKind::NotFound {
                "lock_missing"
            } else {
                "lock_metadata_unavailable"
            },
            format!("cannot inspect the exact maintenance lock: {error}"),
        )
    })?;
    if crate::resources::managed_storage_inventory::file_type(&before) != libc::S_IFREG {
        return Err(refusal(
            "lock_not_regular",
            "the exact maintenance lock is not a regular, non-symlink file",
        ));
    }
    let opened = crate::resources::managed_storage_inventory::openat_regular(
        directory.as_raw_fd(),
        &name,
        path,
    )
    .map_err(|error| refusal("lock_open_refused", error.to_string()))?;
    let opened_metadata = opened.metadata().map_err(|error| {
        refusal(
            "lock_metadata_unavailable",
            format!("cannot inspect the opened maintenance lock: {error}"),
        )
    })?;
    let after = crate::resources::managed_storage_inventory::stat_at(
        directory.as_raw_fd(),
        &name,
        libc::AT_SYMLINK_NOFOLLOW,
    )
    .map_err(|error| {
        refusal(
            "lock_changed_during_inspection",
            format!("the maintenance lock changed while being inspected: {error}"),
        )
    })?;
    let identity = file_identity(&opened_metadata);
    if !opened_metadata.is_file()
        || !stat_matches_identity(&before, &identity)
        || !stat_matches_identity(&after, &identity)
    {
        return Err(refusal(
            "lock_changed_during_inspection",
            "the maintenance lock entry changed while being inspected",
        ));
    }
    Ok(identity)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn inspect_lock_file(_directory: &File, _path: &Path) -> Result<FileIdentity, Refusal> {
    Err(refusal(
        "unsupported_platform",
        "native stale-lock recovery is supported only on macOS and Linux",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn unlink_reviewed_lock(
    directory: &File,
    directory_path: &Path,
    expected_directory: &DirectoryIdentity,
    target: &Path,
    expected_file: &FileIdentity,
) -> Result<(), Refusal> {
    use std::os::fd::AsRawFd;

    if target.parent() != Some(directory_path)
        || target.file_name() != Some(std::ffi::OsStr::new(LOCK_FILE_NAME))
    {
        return Err(refusal(
            "target_path_changed",
            "the target is not the exact common-dir objects/maintenance.lock entry",
        ));
    }
    let (_, current_directory) = canonical_directory_identity(directory_path)
        .map_err(|message| refusal("objects_directory_changed", message))?;
    if &current_directory != expected_directory {
        return Err(refusal(
            "objects_directory_changed",
            "the canonical objects directory changed before unlink",
        ));
    }

    let name = CString::new(LOCK_FILE_NAME).expect("static file name has no NUL");
    let before = crate::resources::managed_storage_inventory::stat_at(
        directory.as_raw_fd(),
        &name,
        libc::AT_SYMLINK_NOFOLLOW,
    )
    .map_err(|error| refusal("lock_changed_before_unlink", error.to_string()))?;
    if crate::resources::managed_storage_inventory::file_type(&before) != libc::S_IFREG {
        return Err(refusal(
            "lock_not_regular",
            "the exact lock entry became a symlink or special file; it was retained",
        ));
    }
    let opened = crate::resources::managed_storage_inventory::openat_regular(
        directory.as_raw_fd(),
        &name,
        target,
    )
    .map_err(|error| refusal("lock_changed_before_unlink", error.to_string()))?;
    let opened_identity = file_identity(&opened.metadata().map_err(|error| {
        refusal(
            "lock_metadata_unavailable",
            format!("cannot recheck the opened lock before unlink: {error}"),
        )
    })?);
    let named = crate::resources::managed_storage_inventory::stat_at(
        directory.as_raw_fd(),
        &name,
        libc::AT_SYMLINK_NOFOLLOW,
    )
    .map_err(|error| refusal("lock_changed_before_unlink", error.to_string()))?;
    if &opened_identity != expected_file
        || !stat_matches_identity(&before, expected_file)
        || !stat_matches_identity(&named, expected_file)
    {
        return Err(refusal(
            "lock_replaced_before_unlink",
            "the exact maintenance-lock identity changed immediately before unlink; it was retained",
        ));
    }
    let (_, current_directory) = canonical_directory_identity(directory_path)
        .map_err(|message| refusal("objects_directory_changed", message))?;
    if &current_directory != expected_directory {
        return Err(refusal(
            "objects_directory_changed",
            "the objects directory changed immediately before unlink; the lock was retained",
        ));
    }
    // SAFETY: directory is a live descriptor for the reviewed objects directory;
    // name is NUL terminated and one fixed basename. unlinkat does not follow a
    // symlink at the leaf and cannot be redirected by a replaced parent path.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(refusal(
            "unlink_failed",
            format!(
                "the maintenance lock was not unlinked: {}",
                io::Error::last_os_error()
            ),
        ));
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unlink_reviewed_lock(
    _directory: &File,
    _directory_path: &Path,
    _expected_directory: &DirectoryIdentity,
    _target: &Path,
    _expected_file: &FileIdentity,
) -> Result<(), Refusal> {
    Err(refusal(
        "unsupported_platform",
        "native stale-lock recovery is supported only on macOS and Linux",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn lock_entry_absent(directory: &File) -> Result<bool, Refusal> {
    use std::os::fd::AsRawFd;
    let name = CString::new(LOCK_FILE_NAME).expect("static file name has no NUL");
    match crate::resources::managed_storage_inventory::stat_at(
        directory.as_raw_fd(),
        &name,
        libc::AT_SYMLINK_NOFOLLOW,
    ) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(refusal(
            "post_unlink_verification_failed",
            format!("cannot verify the exact lock entry after unlink: {error}"),
        )),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn lock_entry_absent(_directory: &File) -> Result<bool, Refusal> {
    Err(refusal(
        "unsupported_platform",
        "native stale-lock recovery is supported only on macOS and Linux",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        uid: metadata.uid(),
        links: metadata.nlink(),
        size: metadata.size(),
        modified: Timestamp {
            seconds: metadata.mtime(),
            nanoseconds: metadata.mtime_nsec().max(0) as u32,
        },
        changed: Timestamp {
            seconds: metadata.ctime(),
            nanoseconds: metadata.ctime_nsec().max(0) as u32,
        },
        birth: metadata.created().ok().and_then(system_time_timestamp),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stat_matches_identity(stat: &libc::stat, identity: &FileIdentity) -> bool {
    let base = i128::from(stat.st_dev) == i128::from(identity.device)
        && i128::from(stat.st_ino) == i128::from(identity.inode)
        && i128::from(stat.st_mode) == i128::from(identity.mode)
        && i128::from(stat.st_uid) == i128::from(identity.uid)
        && i128::from(stat.st_nlink) == i128::from(identity.links)
        && i128::from(stat.st_size) == i128::from(identity.size);
    #[cfg(target_os = "linux")]
    let timestamps_match = {
        i128::from(stat.st_mtime) == i128::from(identity.modified.seconds)
            && i128::from(stat.st_mtime_nsec) == i128::from(identity.modified.nanoseconds)
            && i128::from(stat.st_ctime) == i128::from(identity.changed.seconds)
            && i128::from(stat.st_ctime_nsec) == i128::from(identity.changed.nanoseconds)
    };
    #[cfg(target_os = "macos")]
    let timestamps_match = {
        let birth_matches = identity.birth.as_ref().is_some_and(|birth| {
            i128::from(stat.st_birthtime) == i128::from(birth.seconds)
                && i128::from(stat.st_birthtime_nsec) == i128::from(birth.nanoseconds)
        });
        i128::from(stat.st_mtime) == i128::from(identity.modified.seconds)
            && i128::from(stat.st_mtime_nsec) == i128::from(identity.modified.nanoseconds)
            && i128::from(stat.st_ctime) == i128::from(identity.changed.seconds)
            && i128::from(stat.st_ctime_nsec) == i128::from(identity.changed.nanoseconds)
            && birth_matches
    };
    base && timestamps_match
}

fn system_time_timestamp(value: std::time::SystemTime) -> Option<Timestamp> {
    let duration = value.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(Timestamp {
        seconds: i64::try_from(duration.as_secs()).ok()?,
        nanoseconds: duration.subsec_nanos(),
    })
}

fn effective_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: geteuid reads the process effective UID and dereferences no pointers.
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn binding_digest(binding: &PlanBinding) -> Result<String, String> {
    let bytes = serde_json::to_vec(binding)
        .map_err(|error| format!("cannot serialize the reviewed lock identity: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex(bytes: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn read_boot_proof() -> Result<BootProof, String> {
    let (source, boot_time) = read_os_boot_time()?;
    let safe_before = Timestamp {
        seconds: boot_time
            .seconds
            .checked_sub(SAFETY_MARGIN_SECONDS)
            .ok_or_else(|| "boot time is outside the supported timestamp range".to_string())?,
        nanoseconds: boot_time.nanoseconds,
    };
    Ok(BootProof {
        source,
        boot_time,
        safe_before,
        safety_margin_seconds: SAFETY_MARGIN_SECONDS,
    })
}

#[cfg(target_os = "macos")]
fn read_os_boot_time() -> Result<(String, Timestamp), String> {
    let name = b"kern.boottime\0";
    let mut value = std::mem::MaybeUninit::<libc::timeval>::uninit();
    let mut length = std::mem::size_of::<libc::timeval>();
    // SAFETY: the name is NUL-terminated; value and length point to writable
    // storage of the exact timeval size expected by kern.boottime; no new value
    // is supplied to sysctl.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            value.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || length != std::mem::size_of::<libc::timeval>() {
        return Err(format!(
            "cannot read kern.boottime: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: sysctl succeeded and returned the full timeval buffer.
    let value = unsafe { value.assume_init() };
    let seconds: i64 = value.tv_sec;
    let micros = u32::try_from(value.tv_usec)
        .ok()
        .filter(|micros| *micros < 1_000_000)
        .ok_or_else(|| "kern.boottime microseconds are invalid".to_string())?;
    Ok((
        "macos_kern_boottime".into(),
        Timestamp {
            seconds,
            nanoseconds: micros * 1_000,
        },
    ))
}

#[cfg(target_os = "linux")]
fn read_os_boot_time() -> Result<(String, Timestamp), String> {
    let contents = fs::read_to_string("/proc/stat")
        .map_err(|error| format!("cannot read /proc/stat: {error}"))?;
    let values: Vec<_> = contents
        .lines()
        .filter_map(|line| line.strip_prefix("btime "))
        .collect();
    let [value] = values.as_slice() else {
        return Err("Linux /proc/stat must contain exactly one btime value".into());
    };
    let seconds = value
        .parse::<i64>()
        .map_err(|_| "Linux /proc/stat btime is invalid".to_string())?;
    Ok((
        "linux_proc_stat_btime".into(),
        Timestamp {
            seconds,
            nanoseconds: 0,
        },
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn read_os_boot_time() -> Result<(String, Timestamp), String> {
    Err("native boot-time proof is unsupported on this operating system".into())
}

pub(crate) fn apply(cwd: &Path, confirmation: &str) -> GitMaintenanceLockApplyReport {
    apply_with_boot_result(cwd, confirmation, read_boot_proof())
}

#[cfg(test)]
fn apply_with_boot(
    cwd: &Path,
    confirmation: &str,
    boot: Option<BootProof>,
) -> GitMaintenanceLockApplyReport {
    let result = boot.ok_or_else(|| "test supplied an unknown boot time".to_string());
    apply_with_boot_result(cwd, confirmation, result)
}

fn apply_with_boot_result(
    cwd: &Path,
    confirmation: &str,
    boot_result: Result<BootProof, String>,
) -> GitMaintenanceLockApplyReport {
    let mut report = GitMaintenanceLockApplyReport {
        schema_version: PLAN_SCHEMA_VERSION,
        action: "apply",
        status: ApplyStatus::Refused,
        confirmation_sha256: confirmation.to_string(),
        target: None,
        intent: None,
        receipt: None,
        target_unlinked: false,
        objects_directory_synced: false,
        receipt_written: false,
        refusals: Vec::new(),
        next_action: None,
    };
    if !valid_digest(confirmation) {
        report.refusals.push(refusal(
            "invalid_confirmation",
            "--confirm must be a full 64-character SHA-256 digest",
        ));
        return report;
    }

    let (initial, _) = collect_plan(cwd, boot_result.clone());
    report.target.clone_from(&initial.target);
    if !initial.eligible {
        report.refusals = initial.refusals;
        return report;
    }
    if initial.confirmation_sha256.as_deref() != Some(confirmation) {
        report.refusals.push(refusal(
            "confirmation_mismatch",
            "the confirmation does not match the current exact lock identity; review a fresh plan",
        ));
        return report;
    }

    let context = match capture_repository_context(cwd) {
        Ok(context) => context,
        Err(reason) => {
            report.refusals.push(reason);
            return report;
        }
    };
    let main_root = &context.repository_root;
    let (_repository_key, mut write_guard) =
        match crate::operations::try_acquire_local_repository_write_lock_as(
            main_root,
            &context.checkout,
            "git_maintenance_lock_apply",
            None,
        ) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                report.refusals.push(refusal(
                    "repository_write_lock_busy",
                    "a coordinated repository writer holds one of the required write-lock lanes",
                ));
                return report;
            }
            Err(error) => {
                report.refusals.push(refusal(
                    "repository_write_lock_unavailable",
                    format!("cannot acquire both repository write-lock lanes: {error}"),
                ));
                return report;
            }
        };

    // Name each step in the holder record, so a write refused behind this
    // exclusive hold says what it waits for.
    write_guard.record_phase("re-planning under the write locks");
    let (locked_plan, binding) = collect_plan(cwd, boot_result.clone());
    if !locked_plan.eligible {
        report.refusals = locked_plan.refusals;
        report.target.clone_from(&locked_plan.target);
        return report;
    }
    if locked_plan.confirmation_sha256.as_deref() != Some(confirmation) {
        report.refusals.push(refusal(
            "lock_changed_after_plan",
            "the repository, parent directories, boot proof, or lock-file identity changed under the write locks; retain the lock and review a fresh plan",
        ));
        report.target.clone_from(&locked_plan.target);
        return report;
    }
    let Some(binding) = binding else {
        report.refusals.push(refusal(
            "lock_binding_unavailable",
            "the reviewed lock identity could not be re-established",
        ));
        return report;
    };

    write_guard.record_phase("writing the recovery intent");
    let recovery_directory = match ensure_recovery_directory(main_root) {
        Ok(path) => path,
        Err(message) => {
            report
                .refusals
                .push(refusal("recovery_directory_unavailable", message));
            return report;
        }
    };
    let intent_path = recovery_directory.join(format!("intent-{confirmation}.json"));
    report.intent = Some(intent_path.display().to_string());
    let intent = RecoveryRecord {
        schema_version: PLAN_SCHEMA_VERSION,
        phase: "intent",
        confirmation_sha256: confirmation,
        repository_root: main_root.display().to_string(),
        target: locked_plan.target.as_deref().unwrap_or_default(),
        binding: &binding,
        target_unlinked: false,
        objects_directory_synced: false,
    };
    let intent_bytes = match serde_json::to_vec_pretty(&intent) {
        Ok(mut bytes) => {
            bytes.push(b'\n');
            bytes
        }
        Err(error) => {
            report.refusals.push(refusal(
                "intent_serialization_failed",
                format!("cannot serialize recovery intent: {error}"),
            ));
            return report;
        }
    };
    match crate::atomic_file::write_synced_if_absent(&intent_path, &intent_bytes) {
        Ok(true) => {}
        Ok(false) => {
            report.refusals.push(refusal(
                "intent_already_exists",
                "a recovery intent already exists for this exact plan; inspect it and the target before creating a fresh plan",
            ));
            return report;
        }
        Err(error) => {
            report.refusals.push(refusal(
                "intent_write_failed",
                format!("the durable recovery intent was not established: {error}"),
            ));
            return report;
        }
    }

    // Re-read every bound fact after the durable intent and immediately before
    // the one unlink. The intent is preserved if this check refuses.
    let (final_plan, _) = collect_plan(cwd, boot_result);
    if !final_plan.eligible || final_plan.confirmation_sha256.as_deref() != Some(confirmation) {
        report.refusals = final_plan.refusals;
        if report.refusals.is_empty() {
            report.refusals.push(refusal(
                "lock_changed_before_unlink",
                "the reviewed identity changed after the durable intent; the lock was retained",
            ));
        }
        report.target.clone_from(&final_plan.target);
        report.next_action = Some(
            "inspect the preserved intent and current maintenance.lock, then review a fresh plan; do not retry this digest"
                .into(),
        );
        return report;
    }

    write_guard.record_phase("removing the reviewed lock");
    let unlink_context = match capture_repository_context(cwd) {
        Ok(context) => context,
        Err(reason) => {
            report.refusals.push(reason);
            return report;
        }
    };
    if unlink_context.repository_identity != binding.repository
        || unlink_context.common_identity != binding.git_common_dir
        || unlink_context.objects_identity != binding.objects_dir
    {
        report.refusals.push(refusal(
            "parent_changed_before_unlink",
            "a canonical repository, common-directory, or objects-directory identity changed; the lock was retained",
        ));
        report.next_action = Some("inspect the preserved intent and review a fresh plan".into());
        return report;
    }
    let target = unlink_context.objects_dir.join(LOCK_FILE_NAME);
    report.target = Some(target.display().to_string());
    let objects_directory =
        match open_objects_directory(&unlink_context.objects_dir, &binding.objects_dir) {
            Ok(directory) => directory,
            Err(reason) => {
                report.refusals.push(reason);
                report.next_action =
                    Some("inspect the preserved intent and review a fresh plan".into());
                return report;
            }
        };
    if let Err(reason) = unlink_reviewed_lock(
        &objects_directory,
        &unlink_context.objects_dir,
        &binding.objects_dir,
        &target,
        &binding.file,
    ) {
        report.refusals.push(reason);
        report.next_action =
            Some("inspect the target and preserved intent before planning again".into());
        return report;
    }
    report.target_unlinked = true;

    report.objects_directory_synced = objects_directory.sync_all().is_ok();
    let receipt_path = recovery_directory.join(format!("receipt-{confirmation}.json"));
    report.receipt = Some(receipt_path.display().to_string());
    let receipt_target = target.display().to_string();
    let receipt = RecoveryRecord {
        schema_version: PLAN_SCHEMA_VERSION,
        phase: "receipt",
        confirmation_sha256: confirmation,
        repository_root: main_root.display().to_string(),
        target: &receipt_target,
        binding: &binding,
        target_unlinked: true,
        objects_directory_synced: report.objects_directory_synced,
    };
    let receipt_bytes = match serde_json::to_vec_pretty(&receipt) {
        Ok(mut bytes) => {
            bytes.push(b'\n');
            bytes
        }
        Err(error) => {
            report.status = ApplyStatus::RemovedReceiptFailure;
            report.refusals.push(refusal(
                "receipt_serialization_failed",
                format!("the lock was unlinked but its receipt could not be serialized: {error}"),
            ));
            report.next_action = Some(
                "do not retry the unlink; inspect the intent and target, then preserve an operator receipt"
                    .into(),
            );
            return report;
        }
    };
    match crate::atomic_file::write_synced_if_absent(&receipt_path, &receipt_bytes) {
        Ok(true) => report.receipt_written = true,
        Ok(false) => report.refusals.push(refusal(
            "receipt_already_exists",
            "the lock was unlinked but a receipt already exists for this digest",
        )),
        Err(error) => report.refusals.push(refusal(
            "receipt_write_failed",
            format!("the lock was unlinked but the durable receipt failed: {error}"),
        )),
    }
    let target_absent = match lock_entry_absent(&objects_directory) {
        Ok(absent) => absent,
        Err(reason) => {
            report.status = ApplyStatus::RemovedReceiptFailure;
            report.refusals.push(reason);
            report.next_action = Some(
                "the target was unlinked; do not retry this digest. Inspect the durable intent, receipt, and exact lock path"
                    .into(),
            );
            return report;
        }
    };
    if !report.receipt_written || !report.objects_directory_synced {
        report.status = ApplyStatus::RemovedReceiptFailure;
        report.next_action = Some(
            "the target may already be gone; do not retry this digest. Inspect the intent, receipt path, and exact lock path"
                .into(),
        );
    } else if !target_absent {
        report.status = ApplyStatus::RemovedButLockReappeared;
        report.refusals.push(refusal(
            "lock_reappeared_after_unlink",
            "a directory entry appeared at the exact path after unlink; it was not removed",
        ));
        report.next_action = Some("review a fresh plan for the newly present lock".into());
    } else {
        report.status = ApplyStatus::Removed;
    }
    report
}

#[derive(Serialize)]
struct RecoveryRecord<'a> {
    schema_version: u32,
    phase: &'static str,
    confirmation_sha256: &'a str,
    repository_root: String,
    target: &'a str,
    binding: &'a PlanBinding,
    target_unlinked: bool,
    objects_directory_synced: bool,
}

fn ensure_recovery_directory(repository_root: &Path) -> Result<PathBuf, String> {
    let metadata_directory = repository_root.join(".aethyme");
    let metadata = fs::symlink_metadata(&metadata_directory)
        .map_err(|error| format!("cannot inspect {}: {error}", metadata_directory.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(".aethyme is not a real directory".into());
    }
    let canonical_metadata = fs::canonicalize(&metadata_directory)
        .map_err(|error| format!("cannot canonicalize .aethyme: {error}"))?;
    if canonical_metadata != metadata_directory {
        return Err(".aethyme resolves through a different directory".into());
    }

    let recovery = metadata_directory.join(RECOVERY_DIRECTORY);
    match fs::create_dir(&recovery) {
        Ok(()) => {
            File::open(&metadata_directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| {
                    format!("cannot sync .aethyme after creating recovery directory: {error}")
                })?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("cannot create recovery directory: {error}")),
    }
    let metadata = fs::symlink_metadata(&recovery)
        .map_err(|error| format!("cannot inspect recovery directory: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("the recovery path is not a real directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != effective_uid() {
            return Err("the recovery directory is not owned by the current effective user".into());
        }
        if metadata.mode() & 0o022 != 0 {
            return Err("the recovery directory is group- or world-writable".into());
        }
    }
    let canonical = fs::canonicalize(&recovery)
        .map_err(|error| format!("cannot canonicalize recovery directory: {error}"))?;
    if canonical != recovery {
        return Err("the recovery directory resolves through a different path".into());
    }
    Ok(recovery)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::process::Command;

    struct Fixture {
        _owner: tempfile::TempDir,
        root: PathBuf,
        lock: PathBuf,
    }

    fn fixture(lock_bytes: &[u8]) -> Fixture {
        let owner = tempfile::tempdir().expect("temporary fixture root");
        let root = owner.path().join("repository");
        fs::create_dir_all(&root).expect("fixture checkout directory");
        run_git(&root, &["init", "--quiet"]);
        run_git(
            &root,
            &[
                "config",
                "--local",
                "remote.origin.url",
                "https://github.com/example/maintenance-lock-fixture.git",
            ],
        );
        drop(crate::Broker::open(&root).expect("fixture broker"));
        let lock = root.join(".git/objects/maintenance.lock");
        fs::write(&lock, lock_bytes).expect("fixture maintenance lock");
        Fixture {
            _owner: owner,
            root,
            lock,
        }
    }

    fn run_git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        command.current_dir(root).args(args);
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(key);
            }
        }
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        let output = command.output().expect("spawn fixture Git");
        assert!(
            output.status.success(),
            "fixture Git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fake_boot(seconds_from_now: i64) -> BootProof {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch");
        let boot_time = Timestamp {
            seconds: now.as_secs() as i64 + seconds_from_now,
            nanoseconds: now.subsec_nanos(),
        };
        BootProof {
            source: "private_test_boot_override".into(),
            safe_before: Timestamp {
                seconds: boot_time.seconds - SAFETY_MARGIN_SECONDS,
                nanoseconds: boot_time.nanoseconds,
            },
            boot_time,
            safety_margin_seconds: SAFETY_MARGIN_SECONDS,
        }
    }

    fn refusal_code(plan: &GitMaintenanceLockPlan, code: &str) -> bool {
        plan.refusals.iter().any(|refusal| refusal.code == code)
    }

    #[test]
    fn preboot_empty_lock_is_removed_with_durable_intent_and_receipt() {
        let fixture = fixture(b"");
        let boot = fake_boot(600);
        let plan = plan_with_boot(&fixture.root, Some(boot.clone()));
        assert!(plan.eligible, "{:?}", plan.refusals);
        let digest = plan.confirmation_sha256.clone().unwrap();

        let report = apply_with_boot(&fixture.root, &digest, Some(boot));
        assert_eq!(report.status, ApplyStatus::Removed);
        assert!(report.target_unlinked);
        assert!(report.objects_directory_synced);
        assert!(report.receipt_written);
        assert!(!fixture.lock.exists());
        assert!(Path::new(report.intent.as_deref().unwrap()).is_file());
        assert!(Path::new(report.receipt.as_deref().unwrap()).is_file());
    }

    #[test]
    fn fresh_nonempty_and_symlink_locks_are_retained() {
        let fresh = fixture(b"");
        let plan = plan_with_boot(&fresh.root, Some(fake_boot(-120)));
        assert!(!plan.eligible);
        assert!(refusal_code(&plan, "lock_not_proven_preboot"));
        assert!(fresh.lock.exists());

        let nonempty = fixture(b"lock contents");
        let plan = plan_with_boot(&nonempty.root, Some(fake_boot(600)));
        assert!(!plan.eligible);
        assert!(refusal_code(&plan, "lock_not_empty"));
        assert!(nonempty.lock.exists());

        let symlink = fixture(b"");
        let referent = symlink.root.join("referent");
        fs::write(&referent, b"keep").unwrap();
        fs::remove_file(&symlink.lock).unwrap();
        std::os::unix::fs::symlink(&referent, &symlink.lock).unwrap();
        let plan = plan_with_boot(&symlink.root, Some(fake_boot(600)));
        assert!(!plan.eligible);
        assert!(refusal_code(&plan, "lock_not_regular"));
        assert!(
            fs::symlink_metadata(&symlink.lock)
                .expect("symlink remains")
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(referent).unwrap(), b"keep");
    }

    #[test]
    fn changed_lock_and_changed_objects_parent_refuse_the_reviewed_digest() {
        let touched = fixture(b"");
        let boot = fake_boot(600);
        let plan = plan_with_boot(&touched.root, Some(boot.clone()));
        let digest = plan.confirmation_sha256.unwrap();
        let same_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&touched.lock)
            .unwrap();
        same_file.set_len(1).unwrap();
        same_file.set_len(0).unwrap();
        drop(same_file);
        let report = apply_with_boot(&touched.root, &digest, Some(boot.clone()));
        assert_eq!(report.status, ApplyStatus::Refused);
        assert!(
            report
                .refusals
                .iter()
                .any(|refusal| refusal.code == "confirmation_mismatch")
        );
        assert_eq!(fs::read(&touched.lock).unwrap(), b"");

        let replaced = fixture(b"");
        let plan = plan_with_boot(&replaced.root, Some(boot.clone()));
        let digest = plan.confirmation_sha256.unwrap();
        let old = File::open(&replaced.lock).unwrap();
        fs::remove_file(&replaced.lock).unwrap();
        fs::write(&replaced.lock, b"").unwrap();
        let report = apply_with_boot(&replaced.root, &digest, Some(boot.clone()));
        assert_eq!(report.status, ApplyStatus::Refused);
        assert!(
            report
                .refusals
                .iter()
                .any(|refusal| refusal.code == "confirmation_mismatch")
        );
        assert_eq!(fs::read(&replaced.lock).unwrap(), b"");
        drop(old);

        let parent = fixture(b"");
        let plan = plan_with_boot(&parent.root, Some(boot.clone()));
        let digest = plan.confirmation_sha256.unwrap();
        let objects = parent.root.join(".git/objects");
        fs::rename(&objects, parent.root.join(".git/objects-old")).unwrap();
        fs::create_dir(&objects).unwrap();
        fs::write(objects.join(LOCK_FILE_NAME), b"").unwrap();
        let report = apply_with_boot(&parent.root, &digest, Some(boot));
        assert_eq!(report.status, ApplyStatus::Refused);
        assert!(
            report
                .refusals
                .iter()
                .any(|refusal| refusal.code == "confirmation_mismatch")
        );
        assert_eq!(fs::read(objects.join(LOCK_FILE_NAME)).unwrap(), b"");
    }

    #[test]
    fn unknown_boot_time_refuses_without_a_confirmation() {
        let fixture = fixture(b"");
        let plan = plan_with_boot(&fixture.root, None);
        assert!(!plan.eligible);
        assert!(plan.confirmation_sha256.is_none());
        assert!(refusal_code(&plan, "boot_time_unknown"));
        assert!(fixture.lock.exists());
    }

    #[test]
    fn existing_intent_refuses_without_touching_the_lock() {
        let fixture = fixture(b"");
        let boot = fake_boot(600);
        let plan = plan_with_boot(&fixture.root, Some(boot.clone()));
        assert!(plan.eligible, "{:?}", plan.refusals);
        let digest = plan.confirmation_sha256.unwrap();
        let recovery_directory = fixture.root.join(".aethyme").join(RECOVERY_DIRECTORY);
        fs::create_dir(&recovery_directory).unwrap();
        fs::write(
            recovery_directory.join(format!("intent-{digest}.json")),
            b"existing reviewed intent",
        )
        .unwrap();

        let report = apply_with_boot(&fixture.root, &digest, Some(boot));
        assert_eq!(report.status, ApplyStatus::Refused);
        assert!(
            report
                .refusals
                .iter()
                .any(|refusal| refusal.code == "intent_already_exists")
        );
        assert_eq!(fs::read(&fixture.lock).unwrap(), b"");
    }

    #[test]
    fn only_a_fully_removed_lock_is_a_successful_apply_status() {
        assert!(ApplyStatus::Removed.is_complete());
        assert!(!ApplyStatus::Refused.is_complete());
        assert!(!ApplyStatus::RemovedReceiptFailure.is_complete());
        assert!(!ApplyStatus::RemovedButLockReappeared.is_complete());
    }

    #[test]
    fn active_repository_write_lock_and_unresolved_non_origin_git_write_refuse() {
        let busy = fixture(b"");
        let boot = fake_boot(600);
        let context = capture_repository_context(&busy.root).unwrap();
        let held = crate::operations::try_acquire_local_repository_write_lock(
            &context.repository_root,
            &context.checkout,
        )
        .unwrap()
        .expect("fixture repository write lock");
        let plan = plan_with_boot(&busy.root, Some(boot.clone()));
        let digest = plan.confirmation_sha256.unwrap();
        let report = apply_with_boot(&busy.root, &digest, Some(boot));
        assert_eq!(report.status, ApplyStatus::Refused);
        assert!(
            report
                .refusals
                .iter()
                .any(|refusal| refusal.code == "repository_write_lock_busy")
        );
        assert!(busy.lock.exists());
        drop(held);

        let unresolved = fixture(b"");
        let mut broker = crate::Broker::open(&unresolved.root).unwrap();
        let session = broker
            .store()
            .register_session(&crate::NewSession {
                worktree_path: unresolved.root.to_string_lossy().into_owned(),
                branch: "agent/maintenance-lock-fixture".into(),
                origin: crate::SessionOrigin::Adopted,
                task: Some("maintenance lock fixture".into()),
                diff_base: None,
                adoption_base: None,
                adopted_head: None,
                repository_contract: None,
                pid: None,
                command: None,
                log_path: None,
                agent_identity: None,
            })
            .unwrap();
        let operation = broker
            .store()
            .create_coordinated_operation(&crate::NewCoordinatedOperation {
                session_id: session.id,
                provider: crate::OperationProvider::Git,
                repository: "github.com/another/repository".into(),
                scope: "repository".into(),
                effect: crate::OperationEffect::Write,
                authorization_reason: Some("test unresolved Git write".into()),
                command_json: "[]".into(),
                pid: std::process::id() as i64,
                host_operation_id: None,
                identity_provenance: crate::OperationIdentityProvenance::LocalRepository,
            })
            .unwrap();
        broker
            .store()
            .transition_coordinated_operation(
                operation.id,
                crate::OperationStatus::Running,
                None,
                None,
            )
            .unwrap();
        drop(broker);

        let plan = plan_with_boot(&unresolved.root, Some(fake_boot(600)));
        assert!(!plan.eligible);
        assert!(refusal_code(&plan, "unresolved_repository_mutation"));
        assert!(unresolved.lock.exists());
    }
}
