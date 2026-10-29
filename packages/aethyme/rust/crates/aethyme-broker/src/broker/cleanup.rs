use super::*;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::{CStr, CString, OsString};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs::File;
use std::io::Read;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::fs::MetadataExt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Component;

const WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES: u64 = 8 * 1024;
const CLEANUP_TARGET_SNAPSHOT_UNAVAILABLE: &str = "the cleanup plan could not resolve primary HEAD, so session eligibility was not checked against a target bound to the plan";
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_ORPHAN_RESIDUE_ENTRIES: usize = 100_000;
// A full retry can enumerate once for inventory and once for removal, up to
// three times. Keep one total cap across those traversals and any refill retry.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_ORPHAN_RESIDUE_ENUMERATED_ENTRIES: usize = MAX_ORPHAN_RESIDUE_ENTRIES * 8;
// This also bounds the temporary per-directory name vectors while the full
// inventory and removal walks share one operation budget.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_ORPHAN_RESIDUE_ENUMERATED_NAME_BYTES: usize = 32 * 1024 * 1024;
// Includes each retained relative map key and its duplicated leaf name.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_ORPHAN_RESIDUE_SNAPSHOT_PATH_BYTES: usize = 16 * 1024 * 1024;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_ORPHAN_RESIDUE_DEPTH: usize = 128;
const MAX_ORPHAN_DISPOSABLE_PREFIXES: usize = 16_384;
const MAX_ORPHAN_DISPOSABLE_PREFIX_BYTES: usize = 4_097;
const MAX_ORPHAN_DISPOSABLE_PREFIX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const ORPHAN_RESIDUE_SCAN_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone, Copy)]
enum CleanupDeliveryTargets<'a> {
    /// Resolve fresh for one direct cleanup inspection.
    Fresh,
    /// Reuse the targets bound into a cleanup plan.
    Snapshot(&'a [String]),
    /// Recovery preserves every commit absent from these targets. It needs
    /// complete checkout inspection, not an exact-content landing search.
    ArchivePreservation(&'a [String]),
    /// The plan's initial primary HEAD lookup failed, so its target set is not
    /// suitable for proving that any session's work is represented.
    SnapshotUnavailable,
}

enum CleanupProvenanceAssessment {
    Checked(Box<CleanupProvenance>, String),
    ArchivePreservation { ancestry_proven: bool },
}

#[derive(Clone, Copy)]
struct CleanupStatusLimits {
    deadline: std::time::Instant,
    output_bytes: usize,
    path_fields: usize,
}

#[derive(Clone, Copy)]
enum CleanupArchiveGuard<'a> {
    LegacySpawned {
        plan: &'a crate::CleanupResolvePlan,
        archive_path: &'a Path,
    },
    Retirement {
        plan: &'a crate::CleanupResolvePlan,
        archive_path: &'a Path,
        allow_adopted: bool,
        expected_cohort: Option<&'a crate::auto_cleanup::ClosedArtifactCheckoutCohort>,
    },
}

impl<'a> CleanupArchiveGuard<'a> {
    fn plan(self) -> &'a crate::CleanupResolvePlan {
        match self {
            Self::LegacySpawned { plan, .. } | Self::Retirement { plan, .. } => plan,
        }
    }

    fn archive_path(self) -> &'a Path {
        match self {
            Self::LegacySpawned { archive_path, .. } | Self::Retirement { archive_path, .. } => {
                archive_path
            }
        }
    }

    fn permits_adopted(self) -> bool {
        matches!(
            self,
            Self::Retirement {
                allow_adopted: true,
                ..
            }
        )
    }

    fn is_retirement(self) -> bool {
        matches!(self, Self::Retirement { .. })
    }

    fn preserves_branch(self, session: &Session) -> bool {
        self.is_retirement()
            && (session.origin == SessionOrigin::Adopted
                || self.plan().branch_tip.as_deref() != Some(self.plan().head.as_str())
                || self.plan().checkout_branch_ref.as_deref()
                    != Some(self.plan().branch_ref.as_str()))
    }

    fn expected_cohort(self) -> Option<&'a crate::auto_cleanup::ClosedArtifactCheckoutCohort> {
        match self {
            Self::Retirement {
                expected_cohort, ..
            } => expected_cohort,
            Self::LegacySpawned { .. } => None,
        }
    }
}

impl CleanupStatusLimits {
    fn new(
        overall_deadline: Option<std::time::Instant>,
        output_bytes: usize,
        path_fields: usize,
    ) -> Self {
        let per_call_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        Self {
            deadline: overall_deadline.map_or(per_call_deadline, |deadline| {
                deadline.min(per_call_deadline)
            }),
            output_bytes,
            path_fields,
        }
    }
}

struct CleanupInspectionContext<'a> {
    branch_tips: Option<&'a std::collections::BTreeMap<String, String>>,
    live_checkouts: &'a [(i64, crate::auto_cleanup::ResolvedWorktreePath)],
    delivery_targets: CleanupDeliveryTargets<'a>,
    landing_cache: &'a mut crate::representation::LandingVerdictCache,
    status_limits: CleanupStatusLimits,
    size_deadline: Option<std::time::Instant>,
    allow_adopted_path: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrphanResidueKind {
    Directory,
    RegularFile,
    Symlink,
    Special,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone)]
struct OrphanResidueEntry {
    name: std::ffi::CString,
    kind: OrphanResidueKind,
    device: u64,
    inode: u64,
    tracked_object_id: Option<String>,
    tracked_executable: Option<bool>,
    pointer_sha256: Option<String>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct PinnedOrphanRoot {
    _ancestors: Vec<std::fs::File>,
    ancestry_bindings: Vec<OrphanDirectoryBinding>,
    parent: std::fs::File,
    root: std::fs::File,
    name: std::ffi::CString,
    identity: (u64, u64),
    root_generation_sha256: String,
    filesystem_identity: Vec<u8>,
    mount_identity: Vec<u8>,
}

/// Read the two small files used as ownership witnesses without following a
/// replacement symlink or blocking on a FIFO. A witness that is not a
/// bounded regular file is a conclusive non-match; other I/O errors remain
/// unavailable so callers can preserve their retry state.
fn read_bounded_ownership_witness(path: &Path) -> Result<Option<Vec<u8>>, GitError> {
    Ok(read_bounded_ownership_witness_with_metadata(path)?.map(|witness| witness.bytes))
}

struct BoundedOwnershipWitness {
    bytes: Vec<u8>,
    metadata: std::fs::Metadata,
}

fn read_bounded_ownership_witness_with_metadata(
    path: &Path,
) -> Result<Option<BoundedOwnershipWitness>, GitError> {
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
        // FILE_FLAG_OPEN_REPARSE_POINT: open the reparse point itself so the
        // handle metadata below can reject it without following its target.
        options.custom_flags(0x0020_0000);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Ok(None),
        Err(error) => return Err(ownership_inspection_error(path, "open", error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| ownership_inspection_error(path, "inspect opened", error))?;
    if !metadata.file_type().is_file() || metadata.len() > WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ownership_inspection_error(path, "read", error))?;
    if bytes.len() as u64 > WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES {
        return Ok(None);
    }
    Ok(Some(BoundedOwnershipWitness { bytes, metadata }))
}

fn ownership_inspection_error(path: &Path, action: &str, error: std::io::Error) -> GitError {
    GitError::UntrustedOutput {
        detail: format!(
            "could not {action} worktree ownership path {}: {error}",
            path.display()
        ),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
enum OrphanBlobHasher {
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl OrphanBlobHasher {
    fn new(object_id_len: usize, size: u64) -> Result<Self, String> {
        let header = format!("blob {size}\0");
        match object_id_len {
            40 => {
                let mut hasher = sha1::Sha1::new();
                hasher.update(header.as_bytes());
                Ok(Self::Sha1(hasher))
            }
            64 => {
                let mut hasher = sha2::Sha256::new();
                hasher.update(header.as_bytes());
                Ok(Self::Sha256(hasher))
            }
            _ => Err("the pinned commit has an unsupported object ID format".into()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha1(hasher) => hasher.update(bytes),
            Self::Sha256(hasher) => hasher.update(bytes),
        }
    }

    fn finish(self) -> String {
        match self {
            Self::Sha1(hasher) => format!("{:x}", hasher.finalize()),
            Self::Sha256(hasher) => format!("{:x}", hasher.finalize()),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unix_file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, Copy)]
struct OrphanFsMetadata {
    device: u64,
    inode: u64,
    mode: u32,
    size: u64,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stat_at(parent_fd: RawFd, name: &CString) -> std::io::Result<Option<OrphanFsMetadata>> {
    // SAFETY: libc::stat is a plain C output structure with integer and fixed
    // array fields; all-zero is a valid initial buffer for fstatat.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `parent_fd` is held open by the caller and `name` is one
    // NUL-terminated component. `AT_SYMLINK_NOFOLLOW` inspects the leaf itself.
    if unsafe {
        libc::fstatat(
            parent_fd,
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        return if error.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        };
    }
    #[cfg(target_os = "linux")]
    let device = stat.st_dev as u64;
    #[cfg(target_os = "macos")]
    let device = u64::try_from(stat.st_dev).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "negative device number")
    })?;
    Ok(Some(OrphanFsMetadata {
        device,
        inode: stat.st_ino as u64,
        mode: stat.st_mode as u32,
        size: u64::try_from(stat.st_size).unwrap_or(0),
    }))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn c_name(name: &std::ffi::OsStr) -> std::io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL path component"))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_root_directory() -> std::io::Result<File> {
    let path = CString::new("/").expect("static root path has no NUL");
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW;
    // SAFETY: `path` is NUL-terminated and the returned descriptor is owned below.
    let fd = unsafe { libc::open(path.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `open` returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_directory_at(parent_fd: RawFd, name: &CString) -> std::io::Result<File> {
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW;
    // SAFETY: `parent_fd` is held open and `name` is one component. No symlink
    // is followed at the directory entry being opened.
    let fd = unsafe { libc::openat(parent_fd, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_directory_chain(path: &Path) -> std::io::Result<(Vec<File>, Vec<OrphanDirectoryBinding>)> {
    let mut directories = vec![open_root_directory()?];
    let mut bindings = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                let parent = directories.last().expect("root descriptor is present");
                let name = c_name(name)?;
                let child = open_directory_at(parent.as_raw_fd(), &name)?;
                let metadata = child.metadata()?;
                bindings.push(OrphanDirectoryBinding {
                    parent_fd: parent.as_raw_fd(),
                    name,
                    identity: unix_file_identity(&metadata),
                });
                directories.push(child);
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "directory chain is not a normalized absolute path",
                ));
            }
        }
    }
    Ok((directories, bindings))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn filesystem_identity(fd: RawFd) -> std::io::Result<Vec<u8>> {
    // SAFETY: libc::statfs is a plain C output structure with integer and
    // fixed array fields; all-zero is a valid initial buffer for fstatfs.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is a live directory descriptor and `stat` is writable.
    if unsafe { libc::fstatfs(fd, &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful `fstatfs` initialized the fixed-size `f_fsid` field.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!(stat.f_fsid).cast::<u8>(),
            std::mem::size_of_val(&stat.f_fsid),
        )
    };
    Ok(bytes.to_vec())
}

#[cfg(target_os = "linux")]
fn mount_identity(fd: RawFd) -> std::io::Result<Vec<u8>> {
    let empty_path = CString::new("").expect("empty path has no NUL");
    // SAFETY: libc::statx is a plain C output structure with integer and
    // fixed array fields; all-zero is a valid initial buffer for statx.
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is live and `AT_EMPTY_PATH` requests its mount identity.
    let result = unsafe {
        libc::syscall(
            libc::SYS_statx,
            fd,
            empty_path.as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            &mut stat,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if stat.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "kernel did not provide a mount identity",
        ));
    }
    Ok(stat.stx_mnt_id.to_ne_bytes().to_vec())
}

#[cfg(target_os = "macos")]
fn mount_identity(fd: RawFd) -> std::io::Result<Vec<u8>> {
    // SAFETY: libc::statfs is a plain C output structure with integer and
    // fixed array fields; all-zero is a valid initial buffer for fstatfs.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is a live directory descriptor and `stat` is writable.
    if unsafe { libc::fstatfs(fd, &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fstatfs` fills `f_mntonname` as a NUL-terminated field.
    let mountpoint = unsafe { CStr::from_ptr(stat.f_mntonname.as_ptr()) }.to_bytes();
    if mountpoint.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "filesystem did not provide a mountpoint identity",
        ));
    }
    Ok(mountpoint.to_vec())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl PinnedOrphanRoot {
    fn open(
        path: &Path,
        proof: &crate::events::FinishCleanupRecoveryProof,
    ) -> Result<Option<Self>, String> {
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("cannot resolve orphan root: {error}"))?;
        if canonical.to_str() != Some(proof.worktree_path.as_str()) {
            return Err("orphan root path changed; preserved it for inspection".into());
        }
        let parent_path = canonical
            .parent()
            .ok_or_else(|| "orphan root has no parent directory".to_string())?;
        let leaf = canonical
            .file_name()
            .ok_or_else(|| "orphan root has no final path component".to_string())?;
        let (mut ancestors, ancestry_bindings) = open_directory_chain(parent_path)
            .map_err(|error| format!("cannot pin orphan root parent: {error}"))?;
        let parent = ancestors
            .pop()
            .ok_or_else(|| "orphan root parent chain is empty".to_string())?;
        let name = c_name(leaf).map_err(|error| format!("invalid orphan root name: {error}"))?;
        let root = match open_directory_at(parent.as_raw_fd(), &name) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot pin orphan root: {error}")),
        };
        let metadata = root
            .metadata()
            .map_err(|error| format!("cannot inspect pinned orphan root: {error}"))?;
        let identity = unix_file_identity(&metadata);
        if !metadata.is_dir() || identity != (proof.worktree_device, proof.worktree_inode) {
            return Err("orphan root identity changed; preserved it for inspection".into());
        }
        let generation = crate::root_generation::read_fd(root.as_raw_fd());
        if !proof
            .root_generation_sha256
            .as_deref()
            .is_some_and(crate::root_generation::valid_digest)
            || generation.as_deref() != proof.root_generation_sha256.as_deref()
        {
            return Err(
                "orphan root generation changed or is unavailable; preserved it for inspection"
                    .into(),
            );
        }
        let root_generation_sha256 = proof.root_generation_sha256.clone().ok_or_else(|| {
            "orphan root generation is unavailable; preserved it for inspection".to_string()
        })?;
        let root_filesystem_identity = filesystem_identity(root.as_raw_fd())
            .map_err(|error| format!("cannot inspect orphan root volume: {error}"))?;
        let root_mount_identity = mount_identity(root.as_raw_fd())
            .map_err(|error| format!("cannot inspect orphan root mount: {error}"))?;
        if metadata.dev() != parent.metadata().map_err(|error| error.to_string())?.dev()
            || filesystem_identity(parent.as_raw_fd()).map_err(|error| error.to_string())?
                != root_filesystem_identity
            || mount_identity(parent.as_raw_fd()).map_err(|error| error.to_string())?
                != root_mount_identity
        {
            return Err("orphan root is a mount point; preserved it for inspection".into());
        }
        let pinned = Self {
            _ancestors: ancestors,
            ancestry_bindings,
            parent,
            root,
            name,
            identity,
            root_generation_sha256,
            filesystem_identity: root_filesystem_identity,
            mount_identity: root_mount_identity,
        };
        pinned.ensure_binding()?;
        Ok(Some(pinned))
    }

    fn ensure_binding(&self) -> Result<(), String> {
        for binding in &self.ancestry_bindings {
            let named = stat_at(binding.parent_fd, &binding.name)
                .map_err(|error| format!("cannot recheck orphan parent binding: {error}"))?;
            if !named.is_some_and(|entry| {
                entry.device == binding.identity.0
                    && entry.inode == binding.identity.1
                    && orphan_kind(entry.mode) == OrphanResidueKind::Directory
            }) {
                return Err("orphan parent path changed; preserved it for inspection".into());
            }
        }
        let named = stat_at(self.parent.as_raw_fd(), &self.name)
            .map_err(|error| format!("cannot recheck orphan root binding: {error}"))?;
        if named.is_some_and(|entry| {
            entry.device == self.identity.0
                && entry.inode == self.identity.1
                && mode_has_type(entry.mode, libc::S_IFDIR)
        }) {
            if crate::root_generation::read_fd(self.root.as_raw_fd()).as_deref()
                == Some(self.root_generation_sha256.as_str())
            {
                Ok(())
            } else {
                Err(
                    "orphan root generation changed or is unavailable; preserved it for inspection"
                        .into(),
                )
            }
        } else {
            Err("orphan root binding changed; preserved it for inspection".into())
        }
    }

    fn remove_empty(&self) -> Result<OrphanRemovePass, String> {
        self.ensure_binding()?;
        // SAFETY: the pinned parent descriptor is live and the leaf name is
        // the recorded root binding. The kernel removes it only if it is empty.
        let result = unsafe {
            libc::unlinkat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        };
        if result == 0 {
            return Ok(OrphanRemovePass::Complete);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::DirectoryNotEmpty {
            Ok(OrphanRemovePass::Retry)
        } else if error.kind() == std::io::ErrorKind::NotFound {
            Ok(OrphanRemovePass::Complete)
        } else {
            Err(format!("cannot remove the verified orphan root: {error}"))
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrphanRemovePass {
    Complete,
    Retry,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone)]
struct OrphanDirectoryBinding {
    parent_fd: RawFd,
    name: CString,
    identity: (u64, u64),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct OrphanResidueBudget {
    deadline: std::time::Instant,
    enumerated_entries: usize,
    enumerated_name_bytes: usize,
    traversal_entries: usize,
    snapshot_path_bytes: usize,
    max_enumerated_entries: usize,
    max_enumerated_name_bytes: usize,
    max_snapshot_path_bytes: usize,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl OrphanResidueBudget {
    fn new(deadline: std::time::Instant) -> Self {
        Self::with_limits(
            deadline,
            MAX_ORPHAN_RESIDUE_ENUMERATED_ENTRIES,
            MAX_ORPHAN_RESIDUE_ENUMERATED_NAME_BYTES,
            MAX_ORPHAN_RESIDUE_SNAPSHOT_PATH_BYTES,
        )
    }

    fn with_limits(
        deadline: std::time::Instant,
        max_enumerated_entries: usize,
        max_enumerated_name_bytes: usize,
        max_snapshot_path_bytes: usize,
    ) -> Self {
        Self {
            deadline,
            enumerated_entries: 0,
            enumerated_name_bytes: 0,
            traversal_entries: 0,
            snapshot_path_bytes: 0,
            max_enumerated_entries,
            max_enumerated_name_bytes,
            max_snapshot_path_bytes,
        }
    }

    fn check_deadline(&self) -> Result<(), String> {
        if std::time::Instant::now() >= self.deadline {
            Err("orphan residue processing exceeded its time budget".into())
        } else {
            Ok(())
        }
    }

    fn record_name(&mut self, name_bytes: usize) -> Result<(), String> {
        self.check_deadline()?;
        self.traversal_entries = self.traversal_entries.saturating_add(1);
        if self.traversal_entries > MAX_ORPHAN_RESIDUE_ENTRIES {
            return Err(format!(
                "orphan residue exceeds the {}-entry safety limit",
                MAX_ORPHAN_RESIDUE_ENTRIES
            ));
        }
        self.enumerated_entries = self.enumerated_entries.saturating_add(1);
        if self.enumerated_entries > self.max_enumerated_entries {
            return Err("orphan residue exceeds its total enumeration safety limit".into());
        }
        self.enumerated_name_bytes = self.enumerated_name_bytes.saturating_add(name_bytes);
        if self.enumerated_name_bytes > self.max_enumerated_name_bytes {
            return Err("orphan residue exceeds its total name-byte safety limit".into());
        }
        Ok(())
    }

    fn retain_path(&mut self, parent: &str, name: &str) -> Result<(), String> {
        let relative_bytes = parent
            .len()
            .checked_add(if parent.is_empty() { 0 } else { 1 })
            .and_then(|bytes| bytes.checked_add(name.len()))
            .ok_or_else(|| "orphan residue path size overflow".to_string())?;
        let retained_bytes = relative_bytes
            .checked_add(name.len())
            .ok_or_else(|| "orphan residue path size overflow".to_string())?;
        self.snapshot_path_bytes = self.snapshot_path_bytes.saturating_add(retained_bytes);
        if self.snapshot_path_bytes > self.max_snapshot_path_bytes {
            return Err("orphan residue exceeds its retained path safety limit".into());
        }
        Ok(())
    }

    fn begin_inventory(&mut self) {
        self.traversal_entries = 0;
        self.snapshot_path_bytes = 0;
    }

    fn begin_removal_pass(&mut self) {
        self.traversal_entries = 0;
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct OrphanDirectoryEntries(*mut libc::DIR);

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl OrphanDirectoryEntries {
    fn open(fd: RawFd) -> std::io::Result<Self> {
        let dot = CString::new(".").expect("dot has no NUL");
        // SAFETY: `fd` is a live directory descriptor and opening `.` creates
        // an independent directory offset without following a path component.
        let duplicate = unsafe {
            libc::openat(
                fd,
                dot.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            )
        };
        if duplicate < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `duplicate` is a fresh descriptor whose ownership transfers
        // to `fdopendir` on success.
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            let error = std::io::Error::last_os_error();
            // SAFETY: `fdopendir` did not take ownership when it failed.
            unsafe { libc::close(duplicate) };
            return Err(error);
        }
        Ok(Self(stream))
    }

    fn next_name(&mut self) -> std::io::Result<Option<OsString>> {
        reset_errno();
        // SAFETY: `self.0` is a live DIR stream owned by this value.
        let entry = unsafe { libc::readdir(self.0) };
        if entry.is_null() {
            let errno = current_errno();
            return if errno == 0 {
                Ok(None)
            } else {
                Err(std::io::Error::from_raw_os_error(errno))
            };
        }
        // SAFETY: `d_name` is NUL-terminated and remains valid until the next
        // `readdir`; copy its bytes before returning.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }
            .to_bytes()
            .to_vec();
        Ok(Some(OsString::from_vec(bytes)))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for OrphanDirectoryEntries {
    fn drop(&mut self) {
        // SAFETY: this value owns the DIR stream returned by fdopendir.
        unsafe { libc::closedir(self.0) };
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn reset_errno() {
    // SAFETY: the platform errno pointer is thread-local and valid.
    unsafe { *errno_location() = 0 };
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn current_errno() -> libc::c_int {
    // SAFETY: the platform errno pointer is thread-local and valid.
    unsafe { *errno_location() }
}

#[cfg(target_os = "linux")]
unsafe fn errno_location() -> *mut libc::c_int {
    // SAFETY: this is libc's calling-thread errno pointer.
    unsafe { libc::__errno_location() }
}

#[cfg(target_os = "macos")]
unsafe fn errno_location() -> *mut libc::c_int {
    // SAFETY: this is libc's calling-thread errno pointer.
    unsafe { libc::__error() }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_orphan_directory_names(
    fd: RawFd,
    budget: &mut OrphanResidueBudget,
) -> Result<Vec<String>, String> {
    let mut stream = OrphanDirectoryEntries::open(fd)
        .map_err(|error| format!("cannot list pinned residue directory: {error}"))?;
    let mut names = Vec::new();
    loop {
        budget.check_deadline()?;
        let Some(name) = stream
            .next_name()
            .map_err(|error| format!("cannot read pinned residue directory: {error}"))?
        else {
            break;
        };
        if name == "." || name == ".." {
            continue;
        }
        budget.record_name(name.as_bytes().len())?;
        let name = name.into_string().map_err(|_| {
            "orphan residue contains a non-UTF-8 path; preserved it for inspection".to_string()
        })?;
        if name.is_empty() || name.contains('/') {
            return Err("orphan residue contains an invalid path component".into());
        }
        names.push(name);
    }
    names.sort();
    Ok(names)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_regular_file_at(parent_fd: RawFd, name: &CString) -> std::io::Result<File> {
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    // SAFETY: `parent_fd` is live and `name` is a single component; the final
    // component cannot redirect through a symlink.
    let fd = unsafe { libc::openat(parent_fd, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unlink_orphan_entry(parent_fd: RawFd, name: &CString, directory: bool) -> std::io::Result<()> {
    let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: the parent descriptor is pinned and the final component is one
    // NUL-terminated name. `unlinkat` never follows a symlink leaf.
    if unsafe { libc::unlinkat(parent_fd, name.as_ptr(), flags) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hash_regular_file_at(
    root: &PinnedOrphanRoot,
    parent_fd: RawFd,
    name: &CString,
    expected: OrphanFsMetadata,
    object_id_len: usize,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let mut file = open_regular_file_at(parent_fd, name)
        .map_err(|error| format!("cannot safely open tracked residue: {error}"))?;
    let before = file
        .metadata()
        .map_err(|error| format!("cannot inspect opened tracked residue: {error}"))?;
    if !before.is_file()
        || unix_file_identity(&before) != (expected.device, expected.inode)
        || before.len() != expected.size
        || before.mode() as u32 != expected.mode
        || filesystem_identity(file.as_raw_fd()).map_err(|error| error.to_string())?
            != root.filesystem_identity
        || mount_identity(file.as_raw_fd()).map_err(|error| error.to_string())?
            != root.mount_identity
    {
        return Err("tracked residue changed while it was opened; preserved it".into());
    }
    let mut hasher = OrphanBlobHasher::new(object_id_len, expected.size)?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if std::time::Instant::now() >= deadline {
            return Err("orphan residue inventory exceeded its time budget".into());
        }
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read tracked residue: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let after = file
        .metadata()
        .map_err(|error| format!("cannot recheck tracked residue: {error}"))?;
    if unix_file_identity(&after) != (expected.device, expected.inode)
        || after.len() != expected.size
        || after.mode() as u32 != expected.mode
    {
        return Err("tracked residue changed during inspection; preserved it".into());
    }
    let named = stat_at(parent_fd, name)
        .map_err(|error| format!("cannot recheck tracked residue binding: {error}"))?;
    if !named.is_some_and(|entry| {
        (entry.device, entry.inode) == (expected.device, expected.inode)
            && entry.mode == expected.mode
            && entry.size == expected.size
    }) {
        return Err("tracked residue binding changed during inspection; preserved it".into());
    }
    Ok(hasher.finish())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hash_symlink_at(
    root: &PinnedOrphanRoot,
    parent_fd: RawFd,
    name: &CString,
    expected: OrphanFsMetadata,
    object_id_len: usize,
) -> Result<String, String> {
    let mut target = vec![0_u8; 16 * 1024];
    // SAFETY: the parent descriptor is pinned, the component is NUL-terminated,
    // and readlinkat reads the target bytes without following them.
    let length = unsafe {
        libc::readlinkat(
            parent_fd,
            name.as_ptr(),
            target.as_mut_ptr().cast(),
            target.len(),
        )
    };
    if length < 0 {
        return Err(format!(
            "cannot read tracked symlink: {}",
            std::io::Error::last_os_error()
        ));
    }
    let length = length as usize;
    if length == target.len() {
        return Err("tracked symlink target exceeds the inspection bound".into());
    }
    let after = stat_at(parent_fd, name)
        .map_err(|error| format!("cannot recheck tracked symlink: {error}"))?
        .ok_or_else(|| "tracked symlink disappeared during inspection".to_string())?;
    if (after.device, after.inode) != (expected.device, expected.inode)
        || !mode_has_type(after.mode, libc::S_IFLNK)
        || after.device != root.identity.0
    {
        return Err("tracked symlink changed during inspection; preserved it".into());
    }
    let mut hasher = OrphanBlobHasher::new(object_id_len, length as u64)?;
    hasher.update(&target[..length]);
    Ok(hasher.finish())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_bounded_regular_file_at(
    root: &PinnedOrphanRoot,
    parent_fd: RawFd,
    name: &CString,
    expected: OrphanFsMetadata,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    let file = open_regular_file_at(parent_fd, name)
        .map_err(|error| format!("cannot safely open cleanup witness: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect cleanup witness: {error}"))?;
    if !metadata.is_file()
        || unix_file_identity(&metadata) != (expected.device, expected.inode)
        || metadata.len() > max_bytes
        || filesystem_identity(file.as_raw_fd()).map_err(|error| error.to_string())?
            != root.filesystem_identity
        || mount_identity(file.as_raw_fd()).map_err(|error| error.to_string())?
            != root.mount_identity
    {
        return Err("cleanup witness is not the expected bounded regular file".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read cleanup witness: {error}"))?;
    let after = stat_at(parent_fd, name)
        .map_err(|error| format!("cannot recheck cleanup witness: {error}"))?
        .ok_or_else(|| "cleanup witness disappeared during inspection".to_string())?;
    if bytes.len() as u64 > max_bytes
        || (after.device, after.inode) != (expected.device, expected.inode)
        || after.size != metadata.len()
        || !mode_has_type(after.mode, libc::S_IFREG)
    {
        return Err("cleanup witness changed during inspection".into());
    }
    Ok(bytes)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn path_is_under_disposable_prefix(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| {
        prefix
            .strip_suffix('/')
            .is_some_and(|root| path == root || path.starts_with(prefix))
    })
}

fn valid_disposable_ignored_prefix(prefix: &str) -> bool {
    let Some(path) = prefix.strip_suffix('/') else {
        return false;
    };
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn validate_disposable_prefixes(prefixes: &[String]) -> Result<(), String> {
    let prefix_bytes = prefixes
        .iter()
        .try_fold(0_usize, |total, prefix| total.checked_add(prefix.len()));
    if prefixes.len() > MAX_ORPHAN_DISPOSABLE_PREFIXES
        || prefixes.iter().any(|prefix| {
            prefix.len() > MAX_ORPHAN_DISPOSABLE_PREFIX_BYTES
                || !valid_disposable_ignored_prefix(prefix)
        })
        || prefix_bytes.is_none_or(|bytes| bytes > MAX_ORPHAN_DISPOSABLE_PREFIX_TOTAL_BYTES)
        || prefixes.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err("the durable cleanup proof has invalid ignored directory prefixes".into());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn tracked_tree_contains_directory(
    files: &std::collections::BTreeMap<String, crate::git::GitTreeFile>,
    directory: &str,
    deadline: std::time::Instant,
) -> Result<bool, String> {
    if std::time::Instant::now() >= deadline {
        return Err("tracked tree directory lookup exceeded its time budget".into());
    }
    let mut prefix = String::with_capacity(directory.len().saturating_add(1));
    prefix.push_str(directory);
    prefix.push('/');
    Ok(files
        .range::<str, _>((
            std::ops::Bound::Included(prefix.as_str()),
            std::ops::Bound::Unbounded,
        ))
        .next()
        .is_some_and(|(path, _)| path.starts_with(&prefix)))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn mode_has_type(mode: u32, expected: libc::mode_t) -> bool {
    u64::from(mode) & u64::from(libc::S_IFMT) == u64::from(expected)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn orphan_kind(mode: u32) -> OrphanResidueKind {
    if mode_has_type(mode, libc::S_IFDIR) {
        OrphanResidueKind::Directory
    } else if mode_has_type(mode, libc::S_IFREG) {
        OrphanResidueKind::RegularFile
    } else if mode_has_type(mode, libc::S_IFLNK) {
        OrphanResidueKind::Symlink
    } else {
        OrphanResidueKind::Special
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn relative_orphan_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn git_pointer_matches_proof(
    bytes: &[u8],
    proof: &crate::events::FinishCleanupRecoveryProof,
) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Some(pointed) = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
        .map(str::trim)
    else {
        return false;
    };
    let pointed = PathBuf::from(pointed);
    let pointed = if pointed.is_absolute() {
        pointed
    } else {
        Path::new(&proof.worktree_path).join(pointed)
    };
    let Some(parent) = pointed
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
    else {
        return false;
    };
    pointed
        .file_name()
        .is_some_and(|name| parent.join(name) == Path::new(&proof.git_admin_path))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn collect_orphan_residue(
    root: &PinnedOrphanRoot,
    directory: &File,
    relative_directory: &str,
    proof: &crate::events::FinishCleanupRecoveryProof,
    tracked_files: &std::collections::BTreeMap<String, crate::git::GitTreeFile>,
    budget: &mut OrphanResidueBudget,
    entries: &mut std::collections::BTreeMap<String, OrphanResidueEntry>,
) -> Result<(), String> {
    budget.check_deadline()?;
    let directory_metadata = directory
        .metadata()
        .map_err(|error| format!("cannot inspect pinned residue directory: {error}"))?;
    if !directory_metadata.is_dir()
        || directory_metadata.dev() != root.identity.0
        || filesystem_identity(directory.as_raw_fd()).map_err(|error| error.to_string())?
            != root.filesystem_identity
        || mount_identity(directory.as_raw_fd()).map_err(|error| error.to_string())?
            != root.mount_identity
    {
        return Err("residue directory crosses a volume or mount; preserved it".into());
    }
    let names = read_orphan_directory_names(directory.as_raw_fd(), budget)?;
    for name in names {
        budget.check_deadline()?;
        budget.retain_path(relative_directory, &name)?;
        let relative = relative_orphan_path(relative_directory, &name);
        let depth = relative.bytes().filter(|byte| *byte == b'/').count() + 1;
        if depth > MAX_ORPHAN_RESIDUE_DEPTH {
            return Err(format!(
                "orphan residue exceeds the {MAX_ORPHAN_RESIDUE_DEPTH}-level safety limit"
            ));
        }
        let name_c = c_name(std::ffi::OsStr::new(&name))
            .map_err(|error| format!("invalid residue path component: {error}"))?;
        let Some(metadata) = stat_at(directory.as_raw_fd(), &name_c)
            .map_err(|error| format!("cannot inspect residue entry {relative}: {error}"))?
        else {
            continue;
        };
        if metadata.device != root.identity.0 {
            return Err(format!(
                "residue path {relative} crosses onto another volume"
            ));
        }
        let under_ignored_prefix =
            path_is_under_disposable_prefix(&relative, &proof.disposable_ignored_root_prefixes);
        let kind = orphan_kind(metadata.mode);
        let mut entry = OrphanResidueEntry {
            name: name_c.clone(),
            kind,
            device: metadata.device,
            inode: metadata.inode,
            tracked_object_id: None,
            tracked_executable: None,
            pointer_sha256: None,
        };
        match kind {
            OrphanResidueKind::Directory => {
                if !under_ignored_prefix
                    && !tracked_tree_contains_directory(tracked_files, &relative, budget.deadline)?
                {
                    return Err(format!(
                        "unverified directory {relative} remains; preserved it for inspection"
                    ));
                }
                let child = open_directory_at(directory.as_raw_fd(), &name_c).map_err(|error| {
                    format!("cannot safely open residue directory {relative}: {error}")
                })?;
                let opened = child.metadata().map_err(|error| {
                    format!("cannot inspect opened residue directory {relative}: {error}")
                })?;
                if unix_file_identity(&opened) != (metadata.device, metadata.inode)
                    || filesystem_identity(child.as_raw_fd()).map_err(|error| error.to_string())?
                        != root.filesystem_identity
                    || mount_identity(child.as_raw_fd()).map_err(|error| error.to_string())?
                        != root.mount_identity
                {
                    return Err(format!(
                        "residue directory {relative} changed identity or crossed a mount; preserved it"
                    ));
                }
                entries.insert(relative.clone(), entry);
                collect_orphan_residue(
                    root,
                    &child,
                    &relative,
                    proof,
                    tracked_files,
                    budget,
                    entries,
                )?;
            }
            OrphanResidueKind::Special => {
                return Err(format!(
                    "special path {relative} remains; preserved it for inspection"
                ));
            }
            OrphanResidueKind::RegularFile if relative == ".git" => {
                let bytes = read_bounded_regular_file_at(
                    root,
                    directory.as_raw_fd(),
                    &name_c,
                    metadata,
                    WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES,
                )?;
                if (metadata.device, metadata.inode)
                    != (proof.git_pointer_device, proof.git_pointer_inode)
                    || !git_pointer_matches_proof(&bytes, proof)
                {
                    return Err(
                        "the remaining .git pointer no longer matches the cleanup proof".into(),
                    );
                }
                entry.pointer_sha256 = Some(format!("{:x}", sha2::Sha256::digest(&bytes)));
                entries.insert(relative, entry);
            }
            OrphanResidueKind::RegularFile | OrphanResidueKind::Symlink => {
                if let Some(tracked) = tracked_files.get(&relative) {
                    let expected_symlink = tracked.mode == "120000";
                    if (kind == OrphanResidueKind::Symlink) != expected_symlink {
                        return Err(format!(
                            "tracked path {relative} changed type; preserved it for inspection"
                        ));
                    }
                    let actual = if expected_symlink {
                        hash_symlink_at(
                            root,
                            directory.as_raw_fd(),
                            &name_c,
                            metadata,
                            tracked.object_id.len(),
                        )?
                    } else {
                        let expected_executable = tracked.mode == "100755";
                        let actual_executable = metadata.mode & 0o111 != 0;
                        if expected_executable != actual_executable {
                            return Err(format!(
                                "tracked path {relative} changed executable mode; preserved it for inspection"
                            ));
                        }
                        entry.tracked_executable = Some(expected_executable);
                        hash_regular_file_at(
                            root,
                            directory.as_raw_fd(),
                            &name_c,
                            metadata,
                            tracked.object_id.len(),
                            budget.deadline,
                        )?
                    };
                    if actual != tracked.object_id {
                        return Err(format!(
                            "tracked path {relative} changed after cleanup was authorized; preserved it for inspection"
                        ));
                    }
                    entry.tracked_object_id = Some(tracked.object_id.clone());
                } else if under_ignored_prefix {
                    if kind == OrphanResidueKind::RegularFile {
                        verify_regular_file_mount(root, directory.as_raw_fd(), &name_c, metadata)?;
                    }
                } else {
                    return Err(format!(
                        "unverified path {relative} remains; preserved it for inspection"
                    ));
                }
                entries.insert(relative, entry);
            }
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn verify_regular_file_mount(
    root: &PinnedOrphanRoot,
    parent_fd: RawFd,
    name: &CString,
    expected: OrphanFsMetadata,
) -> Result<(), String> {
    let file = open_regular_file_at(parent_fd, name)
        .map_err(|error| format!("cannot safely inspect disposable file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect disposable file: {error}"))?;
    if !metadata.is_file()
        || unix_file_identity(&metadata) != (expected.device, expected.inode)
        || filesystem_identity(file.as_raw_fd()).map_err(|error| error.to_string())?
            != root.filesystem_identity
        || mount_identity(file.as_raw_fd()).map_err(|error| error.to_string())?
            != root.mount_identity
    {
        return Err("disposable file changed identity or crossed a mount; preserved it".into());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ensure_orphan_directory_bindings(
    root: &PinnedOrphanRoot,
    bindings: &[OrphanDirectoryBinding],
) -> Result<(), String> {
    root.ensure_binding()?;
    for binding in bindings {
        let named = stat_at(binding.parent_fd, &binding.name)
            .map_err(|error| format!("cannot recheck residue directory binding: {error}"))?;
        if !named.is_some_and(|entry| {
            entry.device == binding.identity.0
                && entry.inode == binding.identity.1
                && orphan_kind(entry.mode) == OrphanResidueKind::Directory
        }) {
            return Err("residue directory binding changed; preserved the remainder".into());
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_orphan_directory_contents(
    root: &PinnedOrphanRoot,
    directory: &File,
    relative_directory: &str,
    bindings: &mut Vec<OrphanDirectoryBinding>,
    entries: &std::collections::BTreeMap<String, OrphanResidueEntry>,
    proof: &crate::events::FinishCleanupRecoveryProof,
    budget: &mut OrphanResidueBudget,
) -> Result<OrphanRemovePass, String> {
    if budget.check_deadline().is_err() {
        return Err(
            "orphan residue removal exceeded its time budget; preserved the remainder".into(),
        );
    }
    ensure_orphan_directory_bindings(root, bindings)?;
    let names = read_orphan_directory_names(directory.as_raw_fd(), budget)?;
    for name in &names {
        let relative = relative_orphan_path(relative_directory, name);
        if !entries.contains_key(&relative) {
            return Ok(OrphanRemovePass::Retry);
        }
    }

    for directories_first in [true, false] {
        for name in &names {
            if budget.check_deadline().is_err() {
                return Err(
                    "orphan residue removal exceeded its time budget; preserved the remainder"
                        .into(),
                );
            }
            let relative = relative_orphan_path(relative_directory, name);
            let entry = entries
                .get(&relative)
                .expect("directory listing was checked against the snapshot");
            let is_directory = entry.kind == OrphanResidueKind::Directory;
            if is_directory != directories_first {
                continue;
            }
            ensure_orphan_directory_bindings(root, bindings)?;
            let current = stat_at(directory.as_raw_fd(), &entry.name)
                .map_err(|error| format!("cannot recheck residue path {relative}: {error}"))?;
            let Some(current) = current else {
                continue;
            };
            if (current.device, current.inode) != (entry.device, entry.inode)
                || orphan_kind(current.mode) != entry.kind
                || current.device != root.identity.0
            {
                return Err(format!(
                    "residue path {relative} changed after verification; preserved it"
                ));
            }
            if is_directory {
                let child =
                    open_directory_at(directory.as_raw_fd(), &entry.name).map_err(|error| {
                        format!("cannot safely reopen residue directory {relative}: {error}")
                    })?;
                let child_metadata = child.metadata().map_err(|error| {
                    format!("cannot inspect reopened residue directory {relative}: {error}")
                })?;
                if unix_file_identity(&child_metadata) != (entry.device, entry.inode)
                    || filesystem_identity(child.as_raw_fd()).map_err(|error| error.to_string())?
                        != root.filesystem_identity
                    || mount_identity(child.as_raw_fd()).map_err(|error| error.to_string())?
                        != root.mount_identity
                {
                    return Err(format!(
                        "residue directory {relative} changed identity or crossed a mount; preserved it"
                    ));
                }
                bindings.push(OrphanDirectoryBinding {
                    parent_fd: directory.as_raw_fd(),
                    name: entry.name.clone(),
                    identity: (entry.device, entry.inode),
                });
                let outcome = remove_orphan_directory_contents(
                    root, &child, &relative, bindings, entries, proof, budget,
                );
                bindings.pop();
                if outcome? == OrphanRemovePass::Retry {
                    return Ok(OrphanRemovePass::Retry);
                }
                ensure_orphan_directory_bindings(root, bindings)?;
                let current = stat_at(directory.as_raw_fd(), &entry.name).map_err(|error| {
                    format!("cannot recheck residue directory {relative}: {error}")
                })?;
                if current.is_some_and(|current| {
                    (current.device, current.inode) != (entry.device, entry.inode)
                        || orphan_kind(current.mode) != OrphanResidueKind::Directory
                }) {
                    return Err(format!(
                        "residue directory {relative} changed before unlink; preserved it"
                    ));
                }
                match unlink_orphan_entry(directory.as_raw_fd(), &entry.name, true) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                        return Ok(OrphanRemovePass::Retry);
                    }
                    Err(error) => {
                        return Err(format!(
                            "cannot remove verified residue directory {relative}: {error}"
                        ));
                    }
                }
            } else {
                match entry.kind {
                    OrphanResidueKind::RegularFile => {
                        if let Some(expected) = entry.tracked_object_id.as_deref() {
                            if let Some(expected_executable) = entry.tracked_executable
                                && (current.mode & 0o111 != 0) != expected_executable
                            {
                                return Err(format!(
                                    "tracked path {relative} changed executable mode during removal; preserved it"
                                ));
                            }
                            if hash_regular_file_at(
                                root,
                                directory.as_raw_fd(),
                                &entry.name,
                                current,
                                expected.len(),
                                budget.deadline,
                            )? != expected
                            {
                                return Err(format!(
                                    "tracked path {relative} changed during removal; preserved it"
                                ));
                            }
                        } else if let Some(expected) = entry.pointer_sha256.as_deref() {
                            let bytes = read_bounded_regular_file_at(
                                root,
                                directory.as_raw_fd(),
                                &entry.name,
                                current,
                                WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES,
                            )?;
                            if format!("{:x}", sha2::Sha256::digest(&bytes)) != expected
                                || !git_pointer_matches_proof(&bytes, proof)
                            {
                                return Err(
                                    "the .git pointer changed during removal; preserved it".into(),
                                );
                            }
                        } else {
                            verify_regular_file_mount(
                                root,
                                directory.as_raw_fd(),
                                &entry.name,
                                current,
                            )?;
                        }
                    }
                    OrphanResidueKind::Symlink => {
                        if let Some(expected) = entry.tracked_object_id.as_deref()
                            && hash_symlink_at(
                                root,
                                directory.as_raw_fd(),
                                &entry.name,
                                current,
                                expected.len(),
                            )? != expected
                        {
                            return Err(format!(
                                "tracked symlink {relative} changed during removal; preserved it"
                            ));
                        }
                    }
                    OrphanResidueKind::Directory | OrphanResidueKind::Special => {
                        return Err(format!(
                            "unsafe residue type at {relative}; preserved it for inspection"
                        ));
                    }
                }
                ensure_orphan_directory_bindings(root, bindings)?;
                match unlink_orphan_entry(directory.as_raw_fd(), &entry.name, false) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!(
                            "cannot remove verified residue {relative}: {error}"
                        ));
                    }
                }
            }
        }
    }
    Ok(OrphanRemovePass::Complete)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_orphan_residue_pass(
    root: &PinnedOrphanRoot,
    entries: &std::collections::BTreeMap<String, OrphanResidueEntry>,
    proof: &crate::events::FinishCleanupRecoveryProof,
    budget: &mut OrphanResidueBudget,
) -> Result<OrphanRemovePass, String> {
    budget.begin_removal_pass();
    root.ensure_binding()?;
    let outcome = remove_orphan_directory_contents(
        root,
        &root.root,
        "",
        &mut Vec::new(),
        entries,
        proof,
        budget,
    )?;
    if outcome == OrphanRemovePass::Retry {
        return Ok(outcome);
    }
    root.remove_empty()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_authorized_orphan_tree(
    repository: &crate::git::GitRepo,
    root_path: &Path,
    proof: &crate::events::FinishCleanupRecoveryProof,
) -> Result<(), String> {
    validate_disposable_prefixes(&proof.disposable_ignored_root_prefixes)?;
    let deadline = std::time::Instant::now() + ORPHAN_RESIDUE_SCAN_BUDGET;
    let mut budget = OrphanResidueBudget::new(deadline);
    let Some(root) = PinnedOrphanRoot::open(root_path, proof)? else {
        return Ok(());
    };
    for attempt in 1..=3 {
        if std::time::Instant::now() >= deadline {
            return Err(
                "orphan residue processing exceeded its time budget; preserved the remainder"
                    .into(),
            );
        }
        root.ensure_binding()?;
        let tracked = repository
            .tracked_tree_files_at(&proof.head, deadline)
            .map_err(|error| format!("cannot inspect the pinned tracked tree: {error}"))?;
        if tracked.iter().any(|entry| entry.mode == "160000") {
            return Err("the pinned checkout contains a submodule; preserved orphan residue for manual inspection".into());
        }
        let tracked_files = tracked
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut entries = std::collections::BTreeMap::new();
        budget.begin_inventory();
        collect_orphan_residue(
            &root,
            &root.root,
            "",
            proof,
            &tracked_files,
            &mut budget,
            &mut entries,
        )?;
        match remove_orphan_residue_pass(&root, &entries, proof, &mut budget)? {
            OrphanRemovePass::Complete => return Ok(()),
            OrphanRemovePass::Retry if attempt < 3 => {}
            OrphanRemovePass::Retry => break,
        }
    }
    Err("new residue kept the orphan directory non-empty after three bounded passes".into())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn remove_authorized_orphan_tree(
    _repository: &crate::git::GitRepo,
    _root: &Path,
    _proof: &crate::events::FinishCleanupRecoveryProof,
) -> Result<(), String> {
    Err("safe orphan residue removal is unavailable on this platform".into())
}

fn worktree_size_estimate(
    path: &Path,
    record_key: &str,
    worktree_present: bool,
    broker_owned: bool,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
    measure: impl FnOnce(&Path) -> Option<crate::disk_headroom::DirectoryUsage>,
) -> (Option<u64>, Option<u64>) {
    if !worktree_present {
        return (Some(0), Some(0));
    }
    if !broker_owned {
        return (None, None);
    }
    if scan.measures() {
        let measured = measure(path);
        if let Some(usage) = measured {
            records.record_usage(record_key, usage.bytes, Some(usage.inodes), now_ms());
        }
        measured
            .map(|usage| (Some(usage.bytes), Some(usage.inodes)))
            .unwrap_or((None, None))
    } else {
        records
            .get(record_key)
            .map(|record| (Some(record.bytes), record.inodes))
            .unwrap_or((None, None))
    }
}

impl Broker {
    /// Claim repository-wide checkout lifecycle mutations.
    ///
    /// `Broker::open` resolves both fields from the repository's primary
    /// checkout, even when the command started inside a linked worktree. Check
    /// that invariant again before deriving the lock location so the claim is
    /// always stored in the same repository state directory, outside linked
    /// checkouts that a lifecycle operation may remove.
    pub(crate) fn claim_worktree_lifecycle(
        &self,
    ) -> Result<crate::file_lock::ExclusiveFileLock, BrokerOpError> {
        self.store.require_live_schema_compatible()?;
        let main_root = self
            .main_root
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: self.main_root.clone(),
                source,
            })?;
        let broker_root = self
            .repo
            .root()
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: self.repo.root().to_path_buf(),
                source,
            })?;
        let common_dir = self
            .main_git_common_dir()?
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: self.main_root.join(".git"),
                source,
            })?;
        if broker_root != main_root || common_dir.parent() != Some(main_root.as_path()) {
            return Err(GitError::UntrustedOutput {
                detail:
                    "worktree lifecycle claim could not confirm the primary repository identity"
                        .into(),
            }
            .into());
        }
        #[cfg(test)]
        if let Some(observer) = &self.worktree_lifecycle_test_observer {
            observer(&main_root);
        }
        let claim =
            crate::file_lock::acquire_repository_lifecycle_lock(&main_root).map_err(|source| {
                BrokerError::Io {
                    path: main_root.join(".aethyme/run"),
                    source,
                }
            })?;
        // The floor may have advanced while this controller waited. Keep the
        // claim only if the currently committed markers still permit us.
        self.store.require_live_schema_compatible()?;
        Ok(claim)
    }

    /// Try to claim repository-wide checkout lifecycle mutation without
    /// waiting. Bounded maintenance uses this to skip a checkout rather than
    /// exceed its deadline while another creation, adoption, or removal runs.
    pub(crate) fn try_claim_worktree_lifecycle(
        &self,
    ) -> Result<Option<crate::file_lock::ExclusiveFileLock>, BrokerOpError> {
        self.store.require_live_schema_compatible()?;
        let main_root = self
            .main_root
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: self.main_root.clone(),
                source,
            })?;
        let broker_root = self
            .repo
            .root()
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: self.repo.root().to_path_buf(),
                source,
            })?;
        let common_dir = self
            .main_git_common_dir()?
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: self.main_root.join(".git"),
                source,
            })?;
        if broker_root != main_root || common_dir.parent() != Some(main_root.as_path()) {
            return Err(GitError::UntrustedOutput {
                detail:
                    "worktree lifecycle claim could not confirm the primary repository identity"
                        .into(),
            }
            .into());
        }
        let claim = crate::file_lock::try_acquire_repository_lifecycle_lock(&main_root).map_err(
            |source| BrokerError::Io {
                path: main_root.join(".aethyme/run"),
                source,
            },
        )?;
        if claim.is_some() {
            // The floor may have advanced while the nonblocking claim was
            // acquired. Keep it only if current committed markers permit us.
            self.store.require_live_schema_compatible()?;
        }
        Ok(claim)
    }

    /// Whether a directory is the remains of a worktree of *this* repository
    /// whose registration is already gone.
    ///
    /// The `.git` file survives the interrupted removal pointing at
    /// `<common dir>/worktrees/<name>`, and that pointer is the evidence. It is
    /// checked rather than trusted for containment -- the caller has already
    /// proven the path sits under the broker-owned root, so this only has to
    /// establish which repository the remains belong to, not that they are safe
    /// to touch.
    pub(super) fn is_orphaned_worktree_of_this_repository_checked(
        &self,
        path: &Path,
    ) -> Result<bool, GitError> {
        let expected = self.main_git_common_dir()?;
        let pointer_path = path.join(".git");
        let Some(pointer) = read_bounded_ownership_witness(&pointer_path)? else {
            return Ok(false);
        };
        let Ok(pointer) = std::str::from_utf8(&pointer) else {
            return Ok(false);
        };
        let Some(gitdir) = pointer.trim().strip_prefix("gitdir:") else {
            return Ok(false);
        };
        let gitdir = PathBuf::from(gitdir.trim());
        let gitdir = if gitdir.is_absolute() {
            gitdir
        } else {
            path.join(gitdir)
        };
        // The gitdir itself is gone -- that is the definition of this state --
        // so compare the `worktrees/<name>` parent it named, not the leaf.
        let Some(worktrees_dir) = gitdir.parent() else {
            return Ok(false);
        };
        if worktrees_dir.file_name() != Some(std::ffi::OsStr::new("worktrees")) {
            return Ok(false);
        }
        let Some(claimed_common_dir) = worktrees_dir.parent() else {
            return Ok(false);
        };
        match std::fs::symlink_metadata(&gitdir) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ownership_inspection_error(&gitdir, "inspect", error)),
        }
        let canonical = |candidate: &Path| match candidate.canonicalize() {
            Ok(path) => Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(candidate.to_path_buf())
            }
            Err(error) => Err(ownership_inspection_error(candidate, "resolve", error)),
        };
        Ok(canonical(claimed_common_dir)? == canonical(&expected)?)
    }

    pub(super) fn worktree_root_marker_matches_checked(
        &self,
        root: &Path,
    ) -> Result<bool, GitError> {
        let marker_path = root.join(WORKTREE_ROOT_MARKER);
        let Some(bytes) = read_bounded_ownership_witness(&marker_path)? else {
            return Ok(false);
        };
        let Ok(marker) = serde_json::from_slice::<WorktreeRootMarker>(&bytes) else {
            return Ok(false);
        };
        if marker.schema_version != WORKTREE_ROOT_SCHEMA_VERSION {
            return Ok(false);
        }
        let common_dir = self.main_git_common_dir()?;
        let repository_key = crate::host_state::repository_key(&self.main_root, Some(&common_dir));
        Ok(marker.repository_key == repository_key && marker.repository_root == self.main_root)
    }

    /// The reason for a cleanup inspection that could not complete. In a
    /// shallow clone the usual cause is a merge-base across the shallow
    /// boundary, which git reports with no message at all, so the reason
    /// names the clone and how to deepen it (#525).
    pub(super) fn cleanup_inspection_failure(&self, error: &dyn std::fmt::Display) -> String {
        let mut reason = format!("cleanup inspection failed: {error}");
        if self.repo.is_shallow() {
            reason.push_str(
                " (this repository is a shallow clone, so history older than its \
                 shallow boundary is missing and the session cannot be related to a \
                 delivery target; deepen it with `aethyme broker advanced git --session \
                 <id> --repo <owner/name> --reason \"<authorization>\" -- fetch \
                 --shallow-since=<date> origin <default-branch>`, or `--unshallow`, \
                 then re-plan)",
            );
        }
        reason
    }

    pub(super) fn cleanup_eligibility(
        &self,
        session_id: i64,
        worktree_path: &Path,
    ) -> Result<(CleanupDisposition, String, Option<CleanupProvenance>), BrokerOpError> {
        let delivery_targets = self.cleanup_delivery_targets()?;
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        self.cleanup_eligibility_with_targets_and_cache(
            session_id,
            worktree_path,
            &delivery_targets,
            &mut landing_cache,
        )
    }

    fn cleanup_eligibility_with_targets_and_cache(
        &self,
        session_id: i64,
        worktree_path: &Path,
        delivery_targets: &[String],
        landing_cache: &mut crate::representation::LandingVerdictCache,
    ) -> Result<(CleanupDisposition, String, Option<CleanupProvenance>), BrokerOpError> {
        self.cleanup_eligibility_with_status_limits(
            session_id,
            worktree_path,
            delivery_targets,
            landing_cache,
            CleanupStatusLimits::new(
                self.landing_deadline.get(),
                crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
                crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            ),
        )
    }

    fn cleanup_eligibility_with_status_limits(
        &self,
        session_id: i64,
        worktree_path: &Path,
        delivery_targets: &[String],
        landing_cache: &mut crate::representation::LandingVerdictCache,
        status_limits: CleanupStatusLimits,
    ) -> Result<(CleanupDisposition, String, Option<CleanupProvenance>), BrokerOpError> {
        self.cleanup_eligibility_with_provenance(worktree_path, status_limits, |session_head| {
            let session = self.store.session(session_id)?;
            self.cleanup_provenance_with_cache(
                &session,
                session_head,
                delivery_targets,
                landing_cache,
            )
            .map(|(provenance, reason)| {
                CleanupProvenanceAssessment::Checked(Box::new(provenance), reason)
            })
        })
    }

    /// Run the complete checkout/status preflight before classifying a missing
    /// landing proof. Recovery archives can preserve unproven commits; an
    /// incomplete inventory must never be treated as only unavailable proof.
    fn cleanup_eligibility_with_provenance<F>(
        &self,
        worktree_path: &Path,
        status_limits: CleanupStatusLimits,
        inspect_provenance: F,
    ) -> Result<(CleanupDisposition, String, Option<CleanupProvenance>), BrokerOpError>
    where
        F: FnOnce(&str) -> Result<CleanupProvenanceAssessment, BrokerOpError>,
    {
        let _inspection_deadline = crate::git::limit_git_until(status_limits.deadline);
        if worktree_path == self.main_root.as_path() {
            return Ok((
                CleanupDisposition::UnsafePath,
                "the primary checkout is never removable by broker cleanup".into(),
                None,
            ));
        }
        let metadata =
            std::fs::symlink_metadata(worktree_path).map_err(|source| BrokerError::Io {
                path: worktree_path.to_path_buf(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            return Ok((
                CleanupDisposition::UnsafePath,
                "worktree path is a symlink".into(),
                None,
            ));
        }

        let checkout = GitRepo::discover(worktree_path)?;
        let canonical_path = worktree_path
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: worktree_path.to_path_buf(),
                source,
            })?;
        if checkout.root() != canonical_path {
            return Ok((
                CleanupDisposition::UnsafePath,
                "worktree path resolves to a parent repository rather than its own checkout".into(),
                None,
            ));
        }
        match checkout.is_dirty_until_with_limits(
            status_limits.deadline,
            status_limits.output_bytes,
            status_limits.path_fields,
        ) {
            Ok(false) => {}
            Ok(true) => {
                return Ok((
                    CleanupDisposition::Dirty,
                    "worktree has uncommitted or untracked changes".into(),
                    None,
                ));
            }
            Err(error) => {
                return Ok((
                    CleanupDisposition::InspectionFailed,
                    self.cleanup_inspection_failure(&error),
                    None,
                ));
            }
        }

        let ignored_verdict = crate::auto_cleanup::AutoCleanupPolicy::load(self.main_root())
            .and_then(|policy| crate::auto_cleanup::ignored_content_reason(worktree_path, &policy));
        match ignored_verdict {
            Ok(None) => {}
            Ok(Some(reason)) | Err(reason) => {
                return Ok((CleanupDisposition::Dirty, reason, None));
            }
        }

        let session_head = checkout.head_commit()?;
        let (provenance, reason) = match inspect_provenance(&session_head) {
            Ok(CleanupProvenanceAssessment::Checked(provenance, reason)) => (*provenance, reason),
            Ok(CleanupProvenanceAssessment::ArchivePreservation { ancestry_proven }) => {
                return Ok(if ancestry_proven {
                    (
                        CleanupDisposition::Eligible,
                        "session HEAD is reachable from a delivery target".into(),
                        None,
                    )
                } else {
                    (
                        CleanupDisposition::UnprovenProvenance,
                        "recovery preserves commits without evaluating exact-content landing proof"
                            .into(),
                        None,
                    )
                });
            }
            Err(BrokerOpError::RepresentationUnavailable { reason }) => {
                return Ok((
                    CleanupDisposition::UnprovenProvenance,
                    format!("landing proof unavailable; preserved the checkout: {reason}"),
                    None,
                ));
            }
            Err(error) => return Err(error),
        };
        let disposition = match provenance.representation {
            CleanupRepresentation::Represented => CleanupDisposition::Eligible,
            CleanupRepresentation::Pending => CleanupDisposition::PendingCommits,
            CleanupRepresentation::Unproven => CleanupDisposition::UnprovenProvenance,
        };
        Ok((disposition, reason, Some(provenance)))
    }

    /// The commits cleanup judges a session's work against: the primary
    /// checkout's HEAD, the integration tip and the configured upstream,
    /// sorted and deduplicated.
    pub(crate) fn cleanup_delivery_targets(&self) -> Result<Vec<String>, BrokerOpError> {
        let mut delivery_targets = vec![self.repo.head_commit()?];
        if let Some(integration) = self.integration_tip() {
            delivery_targets.push(integration);
        }
        if let Some((_upstream, head)) = self.repo.tracking_upstream() {
            delivery_targets.push(head);
        }
        delivery_targets.sort();
        delivery_targets.dedup();
        Ok(delivery_targets)
    }

    /// Whether a session's work is represented on a delivery target.
    ///
    /// Two kinds of evidence answer this, and both are needed. Ancestry is the
    /// primary one: it is cheap, always available, and needs no prior record.
    /// It is also blind to a squash merge, which rewrites the SHA so the
    /// session's commits appear nowhere in the target's history even though
    /// its content is there -- and a session that merged that way is left
    /// uncleanable forever (#164). A recorded representation covers exactly
    /// that gap, so it is consulted only when ancestry comes up short.
    pub(super) fn cleanup_provenance(
        &self,
        session: &Session,
        session_head: &str,
        delivery_targets: &[String],
    ) -> Result<(CleanupProvenance, String), BrokerOpError> {
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        self.cleanup_provenance_with_cache(
            session,
            session_head,
            delivery_targets,
            &mut landing_cache,
        )
    }

    fn cleanup_provenance_with_cache(
        &self,
        session: &Session,
        session_head: &str,
        delivery_targets: &[String],
        landing_cache: &mut crate::representation::LandingVerdictCache,
    ) -> Result<(CleanupProvenance, String), BrokerOpError> {
        let (mut provenance, reason) =
            self.cleanup_provenance_from_ancestry(session, session_head, delivery_targets)?;
        if matches!(
            provenance.representation,
            CleanupRepresentation::Represented
        ) {
            return Ok((provenance, reason));
        }
        let Some(evidence) =
            self.recorded_representation_evidence(session, session_head, delivery_targets)?
        else {
            // The predicate the representation scan and the cleanup audit use
            // (#408). Without it the audit called rebased-then-merged work
            // `in_target` while this plan held the same worktree as unproven.
            // Every positive names fixed commits, so the plan digest over this
            // reason stays stable until a delivery target moves.
            if let Some((target, evidence, landed_by)) = self
                .landing_on_delivery_targets_with_cache(
                    session_head,
                    delivery_targets,
                    landing_cache,
                )?
            {
                provenance.representation = CleanupRepresentation::Represented;
                provenance.pending_commit_count = 0;
                provenance.represented_on = Some(target.clone());
                let by = landed_by
                    .as_deref()
                    .map(|commit| format!(" (landed by {})", short_commit(commit)))
                    .unwrap_or_default();
                provenance.represented_by_commit = landed_by;
                return Ok((
                    provenance,
                    format!(
                        "session head {} is represented on delivery target {} by {}{by}",
                        short_commit(session_head),
                        short_commit(&target),
                        evidence.as_str()
                    ),
                ));
            }
            // Last resort, and the one that survives any merge strategy: the
            // commits exist on a remote, so this directory is not where the
            // work lives.
            if let Some((remote_ref, tracked)) = self.remote_durability_evidence(session_head) {
                provenance.representation = CleanupRepresentation::Represented;
                provenance.pending_commit_count = 0;
                provenance.represented_on = Some(tracked.clone());
                return Ok((
                    provenance,
                    format!(
                        "session head {} is reachable from {}, verified current against the remote",
                        short_commit(session_head),
                        remote_ref
                    ),
                ));
            }
            return Ok((provenance, reason));
        };
        provenance.representation = CleanupRepresentation::Represented;
        provenance.pending_commit_count = 0;
        let reason = match evidence {
            RecordedRepresentationEvidence::Landed {
                representing,
                target,
            } => {
                let text = format!(
                    "recorded representation: session head {} landed as {} on delivery target {}",
                    short_commit(session_head),
                    short_commit(&representing),
                    short_commit(&target)
                );
                provenance.represented_on = Some(target);
                provenance.represented_by_commit = Some(representing);
                text
            }
            RecordedRepresentationEvidence::AlreadyHeld => format!(
                "recorded representation: the default branch already held the net content of \
                 session head {}",
                short_commit(session_head)
            ),
        };
        Ok((provenance, reason))
    }

    /// The first delivery target `head`'s work landed on, with its evidence.
    ///
    /// Ancestry is asked of every target before any target gets the deep
    /// content and patch search (#588). The targets arrive sorted by SHA, so
    /// asking each one the full question in turn made a merged branch pay a
    /// candidate walk over whichever non-containing target sorted first --
    /// a trailing primary checkout, or an integration branch that diverged --
    /// before reaching the one a single `merge-base --is-ancestor` settles.
    /// Only squash and rebase deliveries, which ancestry cannot see, reach the
    /// search.
    pub(super) fn landing_on_delivery_targets(
        &self,
        head: &str,
        delivery_targets: &[String],
    ) -> Result<Option<(String, crate::LandingEvidence, Option<String>)>, BrokerOpError> {
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        self.landing_on_delivery_targets_with_cache(head, delivery_targets, &mut landing_cache)
    }

    fn landing_on_delivery_targets_with_cache(
        &self,
        head: &str,
        delivery_targets: &[String],
        landing_cache: &mut crate::representation::LandingVerdictCache,
    ) -> Result<Option<(String, crate::LandingEvidence, Option<String>)>, BrokerOpError> {
        if let Some(target) = delivery_targets
            .iter()
            .find(|target| self.repo.is_ancestor(head, target))
        {
            return Ok(Some((
                target.clone(),
                crate::LandingEvidence::Ancestry,
                None,
            )));
        }
        for target in delivery_targets {
            if let crate::LandingVerdict::Landed {
                evidence,
                landed_by,
            } = crate::representation::work_landed_with_cache(
                &self.repo,
                head,
                target,
                self.landing_deadline.get(),
                crate::representation::LandingSearchProfile::default(),
                landing_cache,
            )? {
                return Ok(Some((target.clone(), evidence, landed_by)));
            }
        }
        Ok(None)
    }

    /// A recorded representation, when it proves this exact head landed.
    ///
    /// The record is evidence because of how narrowly it is bound: to one
    /// reviewed `(session, head)` pair, naming the one commit that carried the
    /// work. This checks that binding rather than the far weaker claim that
    /// the content turns up somewhere. A record written for a different head
    /// does not apply -- a session that has committed since is judged on its
    /// current head and finds nothing here -- and a representing commit that
    /// has not reached a delivery target proves nothing either.
    pub(super) fn recorded_representation_evidence(
        &self,
        session: &Session,
        session_head: &str,
        delivery_targets: &[String],
    ) -> Result<Option<RecordedRepresentationEvidence>, BrokerOpError> {
        let Some(record) = self
            .store
            .session_representation(session.id, session_head)?
        else {
            return Ok(None);
        };
        let Some(representing) = record.representing_commit else {
            // Recorded with no carrying commit: the branch already held this
            // head's net content, so there is nothing left to locate.
            return Ok(Some(RecordedRepresentationEvidence::AlreadyHeld));
        };
        let Some(target) = delivery_targets
            .iter()
            .find(|target| self.repo.is_ancestor(&representing, target))
            .cloned()
        else {
            return Ok(None);
        };
        Ok(Some(RecordedRepresentationEvidence::Landed {
            representing,
            target,
        }))
    }

    pub(super) fn cleanup_provenance_from_ancestry(
        &self,
        session: &Session,
        session_head: &str,
        delivery_targets: &[String],
    ) -> Result<(CleanupProvenance, String), BrokerOpError> {
        let released_queue_entry_id = if session.accepted_queue_entry_id.is_none() {
            self.store.released_checkpoint_queue_entry(session.id)?
        } else {
            None
        };
        let accepted_queue_entry_id = session.accepted_queue_entry_id.or(released_queue_entry_id);
        let mut provenance = CleanupProvenance {
            representation: CleanupRepresentation::Unproven,
            session_head: session_head.into(),
            adopted_head: session.adopted_head.clone(),
            accepted_session_head: session.accepted_session_head.clone(),
            accepted_integration_commit: session.accepted_integration_commit.clone(),
            accepted_integration_tree: session.accepted_integration_tree.clone(),
            accepted_queue_entry_id,
            accepted_queue_status: None,
            represented_on: None,
            represented_by_commit: None,
            pending_commit_count: 0,
        };

        let Some(accepted_head) = session.accepted_session_head.as_deref() else {
            let adopted_head = session
                .adopted_head
                .as_deref()
                .or(session.adoption_base.as_deref())
                .or(session.diff_base.as_deref());
            if adopted_head == Some(session_head) {
                let represented_on = delivery_targets
                    .iter()
                    .find(|target| self.repo.is_ancestor(session_head, target))
                    .cloned();
                if let Some(represented_on) = represented_on {
                    provenance.representation = CleanupRepresentation::Represented;
                    provenance.represented_on = Some(represented_on.clone());
                    return Ok((
                        provenance,
                        format!(
                            "unchanged adoption boundary is represented on delivery target {}",
                            short_commit(&represented_on)
                        ),
                    ));
                }
                return Ok((
                    provenance,
                    "unchanged adoption boundary is not represented on any delivery target".into(),
                ));
            }
            if let Some(adopted_head) = adopted_head
                && self.repo.is_ancestor(adopted_head, session_head)
            {
                let pending = self.repo.commit_count_between(adopted_head, session_head)?;
                provenance.representation = CleanupRepresentation::Pending;
                provenance.pending_commit_count = pending;
                return Ok((
                    provenance,
                    format!(
                        "{pending} session commit(s) after the adoption boundary have never been accepted"
                    ),
                ));
            }
            return Ok((
                provenance,
                "session HEAD cannot be related safely to its recorded adoption boundary".into(),
            ));
        };

        if accepted_head != session_head {
            if self.repo.is_ancestor(accepted_head, session_head) {
                let pending = self
                    .repo
                    .commit_count_between(accepted_head, session_head)?;
                provenance.representation = CleanupRepresentation::Pending;
                provenance.pending_commit_count = pending;
                return Ok((
                    provenance,
                    format!(
                        "{pending} session commit(s) remain after the last accepted checkpoint"
                    ),
                ));
            }
            return Ok((
                provenance,
                "session HEAD rewrites or diverges from the last accepted checkpoint".into(),
            ));
        }

        let (Some(queue_entry_id), Some(integration_commit), Some(integration_tree)) = (
            accepted_queue_entry_id,
            session.accepted_integration_commit.as_deref(),
            session.accepted_integration_tree.as_deref(),
        ) else {
            return Ok((
                provenance,
                "accepted checkpoint is missing queue, integration commit, or integration tree provenance"
                    .into(),
            ));
        };
        let entry = self.store.merge_queue_entry_if_exists(queue_entry_id)?;
        let Some(entry) = entry else {
            if released_queue_entry_id == Some(queue_entry_id)
                && self.repo.commit_tree_id(integration_commit).ok().as_deref()
                    == Some(integration_tree)
            {
                let represented_on = delivery_targets
                    .iter()
                    .find(|target| self.repo.is_ancestor(integration_commit, target))
                    .cloned();
                if let Some(represented_on) = represented_on {
                    provenance.representation = CleanupRepresentation::Represented;
                    provenance.represented_on = Some(represented_on.clone());
                    return Ok((
                        provenance,
                        format!(
                            "accepted queue entry {queue_entry_id} was GC-released after its checkpoint pin; integration commit is represented on {represented_on}"
                        ),
                    ));
                }
            }
            return Ok((
                provenance,
                format!("accepted queue entry {queue_entry_id} is missing"),
            ));
        };
        provenance.accepted_queue_status = Some(entry.status);
        if entry.session_id != session.id
            || entry.head_commit != accepted_head
            || !matches!(
                entry.status,
                MergeStatus::Promoted | MergeStatus::ExternallyLanded | MergeStatus::Superseded
            )
        {
            return Ok((
                provenance,
                format!(
                    "accepted queue entry {queue_entry_id} does not prove this session checkpoint"
                ),
            ));
        }
        if self.repo.commit_tree_id(integration_commit).ok().as_deref() != Some(integration_tree) {
            return Ok((
                provenance,
                "accepted integration commit and tree provenance do not match".into(),
            ));
        }
        let represented_on = delivery_targets
            .iter()
            .find(|target| self.repo.is_ancestor(integration_commit, target))
            .cloned();
        let Some(represented_on) = represented_on else {
            return Ok((
                provenance,
                "accepted integration commit is not represented on local main, integration, or configured upstream"
                    .into(),
            ));
        };
        provenance.representation = CleanupRepresentation::Represented;
        provenance.represented_on = Some(represented_on.clone());
        Ok((
            provenance,
            format!(
                "accepted checkpoint is represented on delivery target {}",
                short_commit(&represented_on)
            ),
        ))
    }

    /// Eligibility judged from the session branch rather than the worktree.
    fn branch_provenance_disposition(
        &self,
        session: &Session,
        session_head: &str,
        delivery_targets: CleanupDeliveryTargets<'_>,
        landing_cache: &mut crate::representation::LandingVerdictCache,
    ) -> Result<(CleanupDisposition, String, Option<CleanupProvenance>), BrokerOpError> {
        let owned_delivery_targets;
        let delivery_targets = match delivery_targets {
            CleanupDeliveryTargets::ArchivePreservation(_) => {
                return Ok((
                    CleanupDisposition::UnprovenProvenance,
                    "recovery requires a complete readable checkout inventory".into(),
                    None,
                ));
            }
            CleanupDeliveryTargets::Snapshot(targets) => targets,
            CleanupDeliveryTargets::Fresh => {
                owned_delivery_targets = self.cleanup_delivery_targets()?;
                &owned_delivery_targets
            }
            CleanupDeliveryTargets::SnapshotUnavailable => {
                return Ok((
                    CleanupDisposition::InspectionFailed,
                    CLEANUP_TARGET_SNAPSHOT_UNAVAILABLE.into(),
                    None,
                ));
            }
        };
        Ok(
            match self.cleanup_provenance_with_cache(
                session,
                session_head,
                delivery_targets,
                landing_cache,
            ) {
                Ok((provenance, reason)) => {
                    let disposition = match provenance.representation {
                        CleanupRepresentation::Represented => CleanupDisposition::Eligible,
                        CleanupRepresentation::Pending => CleanupDisposition::PendingCommits,
                        CleanupRepresentation::Unproven => CleanupDisposition::UnprovenProvenance,
                    };
                    (disposition, reason, Some(provenance))
                }
                Err(error) => (
                    CleanupDisposition::InspectionFailed,
                    self.cleanup_inspection_failure(&error),
                    None,
                ),
            },
        )
    }

    pub(super) fn cleanup_item(
        &self,
        session: &Session,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        let mut records = crate::measurement::SizeRecords::default();
        self.cleanup_item_scanned(session, crate::SizeScan::Measure, &mut records)
    }

    /// One retained worktree, sized according to `scan`.
    ///
    /// `estimated_bytes: None` already meant "the broker does not know", so a
    /// recorded-size pass reuses it rather than inventing a second way to say
    /// so. Everything else here -- git dirtiness, ancestry, provenance -- is
    /// milliseconds and runs either way; the walk is the whole cost (#176).
    pub(crate) fn cleanup_item_scanned(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        self.cleanup_item_scanned_with_status_limits(
            session,
            scan,
            records,
            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
        )
    }

    pub(crate) fn cleanup_item_scanned_with_status_limits(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
        status_output_bytes: usize,
        status_path_fields: usize,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        self.cleanup_item_scanned_with_limits(
            session,
            scan,
            records,
            CleanupStatusLimits::new(
                self.landing_deadline.get(),
                status_output_bytes,
                status_path_fields,
            ),
        )
    }

    pub(crate) fn cleanup_item_for_recovery_archive_with_status_limits(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
        status_output_bytes: usize,
        status_path_fields: usize,
        delivery_targets: &[String],
        allow_adopted_path: bool,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        let status_limits = CleanupStatusLimits::new(
            self.landing_deadline.get(),
            status_output_bytes,
            status_path_fields,
        );
        let live_checkouts = self.cleanup_live_checkout_snapshot(Some(status_limits.deadline))?;
        self.cleanup_item_for_recovery_archive_with_limits_and_live_checkouts(
            session,
            scan,
            records,
            status_limits,
            delivery_targets,
            allow_adopted_path,
            &live_checkouts,
        )
    }

    pub(crate) fn cleanup_item_for_recovery_archive_with_status_limits_and_live_checkouts(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
        status_output_bytes: usize,
        status_path_fields: usize,
        delivery_targets: &[String],
        allow_adopted_path: bool,
        live_checkouts: &[(i64, crate::auto_cleanup::ResolvedWorktreePath)],
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        self.cleanup_item_for_recovery_archive_with_limits_and_live_checkouts(
            session,
            scan,
            records,
            CleanupStatusLimits::new(
                self.landing_deadline.get(),
                status_output_bytes,
                status_path_fields,
            ),
            delivery_targets,
            allow_adopted_path,
            live_checkouts,
        )
    }

    fn cleanup_item_for_recovery_archive_with_limits(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
        status_limits: CleanupStatusLimits,
        delivery_targets: &[String],
        allow_adopted_path: bool,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        let live_checkouts = self.cleanup_live_checkout_snapshot(Some(status_limits.deadline))?;
        self.cleanup_item_for_recovery_archive_with_limits_and_live_checkouts(
            session,
            scan,
            records,
            status_limits,
            delivery_targets,
            allow_adopted_path,
            &live_checkouts,
        )
    }

    fn cleanup_item_for_recovery_archive_with_limits_and_live_checkouts(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
        status_limits: CleanupStatusLimits,
        delivery_targets: &[String],
        allow_adopted_path: bool,
        live_checkouts: &[(i64, crate::auto_cleanup::ResolvedWorktreePath)],
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        self.cleanup_item_scanned_with_tips(
            session,
            scan,
            records,
            CleanupInspectionContext {
                branch_tips: None,
                live_checkouts,
                delivery_targets: CleanupDeliveryTargets::ArchivePreservation(delivery_targets),
                landing_cache: &mut landing_cache,
                status_limits,
                size_deadline: None,
                allow_adopted_path,
            },
        )
    }

    /// An explicitly opted-in adopted retirement may inspect a path outside
    /// Aethyme's managed worktree root only if Git registers that exact real
    /// linked checkout in this repository. Ordinary cleanup never calls this.
    fn registered_adopted_worktree_matches(
        &self,
        session: &Session,
        path: &Path,
        deadline: std::time::Instant,
    ) -> Result<bool, BrokerOpError> {
        if session.origin != SessionOrigin::Adopted
            || !path.is_absolute()
            || path == self.main_root.as_path()
            || path != Path::new(&session.worktree_path)
        {
            return Ok(false);
        }
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => {
                return Err(BrokerError::Io {
                    path: path.to_path_buf(),
                    source,
                }
                .into());
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Ok(false);
        }
        let canonical_path = path.canonicalize().map_err(|source| BrokerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if canonical_path != path {
            return Ok(false);
        }
        let main_root = self
            .main_root
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: self.main_root.clone(),
                source,
            })?;
        if canonical_path == main_root {
            return Ok(false);
        }

        let _deadline = crate::git::limit_git_until(deadline);
        let checkout = match GitRepo::discover(&canonical_path) {
            Ok(checkout) => checkout,
            Err(_) => return Ok(false),
        };
        if checkout.root() != canonical_path.as_path() {
            return Ok(false);
        }
        let expected_common = self
            .main_git_common_dir()?
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: self.main_root.join(".git"),
                source,
            })?;
        let actual_common = checkout
            .git_common_dir()?
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: canonical_path.join(".git"),
                source,
            })?;
        if actual_common != expected_common {
            return Ok(false);
        }

        let head = checkout.head_commit()?;
        let registrations = self.repo.worktree_inventory()?;
        let matching = registrations
            .iter()
            .filter(|entry| entry.path == canonical_path)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Ok(false);
        }
        let entry = matching[0];
        Ok(!entry.bare
            && !entry.prunable
            && !entry.locked
            && entry.head.as_deref() == Some(head.as_str())
            && entry
                .branch
                .as_deref()
                .is_none_or(|branch| branch.starts_with("refs/heads/")))
    }

    fn archive_preservation_ancestry(
        &self,
        head: &str,
        delivery_targets: &[String],
        deadline: std::time::Instant,
    ) -> Result<CleanupProvenanceAssessment, BrokerOpError> {
        let _deadline = crate::git::limit_git_until(deadline);
        let mut unavailable = None;
        for target in delivery_targets {
            match self.repo_handle().is_ancestor_checked(head, target) {
                Ok(true) => {
                    return Ok(CleanupProvenanceAssessment::ArchivePreservation {
                        ancestry_proven: true,
                    });
                }
                Ok(false) => {}
                Err(error) => unavailable = Some(error.to_string()),
            }
        }
        if let Some(reason) = unavailable {
            // The caller classifies unavailable proof as unproven only after
            // complete checkout inspection. No negative landing claim is made.
            return Err(BrokerOpError::RepresentationUnavailable { reason });
        }
        Ok(CleanupProvenanceAssessment::ArchivePreservation {
            ancestry_proven: false,
        })
    }

    fn cleanup_item_scanned_with_limits(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
        status_limits: CleanupStatusLimits,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        let live_checkouts = self.cleanup_live_checkout_snapshot(Some(status_limits.deadline))?;
        self.cleanup_item_scanned_with_tips(
            session,
            scan,
            records,
            CleanupInspectionContext {
                branch_tips: None,
                live_checkouts: &live_checkouts,
                delivery_targets: CleanupDeliveryTargets::Fresh,
                landing_cache: &mut landing_cache,
                status_limits,
                size_deadline: None,
                allow_adopted_path: false,
            },
        )
    }

    /// [`Self::cleanup_item_scanned`] with every local branch tip already read
    /// (see [`GitRepo::local_branch_tips`]). A plan visits every session ever
    /// recorded, most of whose branch and worktree are long gone; looking the
    /// branch up in one listing lets those return without forking `git`.
    fn cleanup_item_scanned_with_tips(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
        inspection: CleanupInspectionContext<'_>,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        let CleanupInspectionContext {
            branch_tips,
            live_checkouts,
            delivery_targets,
            landing_cache,
            status_limits,
            size_deadline,
            allow_adopted_path,
        } = inspection;
        let worktree_path = PathBuf::from(&session.worktree_path);
        let worktree_present = worktree_path.exists();
        let branch_ref = format!("refs/heads/{}", session.branch);
        let branch_tip = match branch_tips {
            Some(tips) => tips.get(&branch_ref).cloned(),
            None => self.repo.resolve_ref(&branch_ref),
        };
        if !worktree_present && branch_tip.is_none() {
            return Ok(None);
        }
        let unreachable = (!worktree_present)
            .then(|| {
                crate::worktree_location::unavailable_configured_location(
                    &self.main_root,
                    &worktree_path,
                )
            })
            .flatten();
        let broker_owned =
            unreachable.is_none() && self.is_broker_owned_worktree(session, &worktree_path);
        let adopted_archive_path = if !broker_owned
            && allow_adopted_path
            && session.origin == SessionOrigin::Adopted
            && unreachable.is_none()
        {
            self.registered_adopted_worktree_matches(
                session,
                &worktree_path,
                status_limits.deadline,
            )?
        } else {
            false
        };
        let live_owner = if worktree_present {
            overlapping_live_checkout(session.id, &worktree_path, live_checkouts)
        } else {
            Ok(None)
        };
        let (estimated_bytes, estimated_inodes) = worktree_size_estimate(
            &worktree_path,
            &session.worktree_path,
            worktree_present,
            broker_owned && matches!(&live_owner, Ok(None)),
            scan,
            records,
            |path| match size_deadline {
                Some(deadline) => crate::disk_headroom::directory_usage_bounded(path, deadline),
                None => crate::disk_headroom::directory_usage_without_following_links(path).ok(),
            },
        );
        let (mut disposition, mut reason, provenance) = if let Err(reason) = &live_owner {
            (CleanupDisposition::InspectionFailed, reason.clone(), None)
        } else if let Ok(Some(live_id)) = live_owner {
            (
                CleanupDisposition::UnsafePath,
                format!(
                    "checkout overlaps live session {live_id}; preserved its worktree and branch"
                ),
                None,
            )
        } else if let Some(reason) = unreachable {
            // An unplugged drive is not a deleted worktree. Proposing cleanup
            // here would act on a checkout nobody can see, so it stays blocked
            // until the volume is back.
            (CleanupDisposition::InspectionFailed, reason, None)
        } else if !broker_owned && !adopted_archive_path {
            (
                CleanupDisposition::UnsafePath,
                "session path is outside the broker-owned worktree directory and is not an exact registered adopted checkout".into(),
                None,
            )
        } else if worktree_present {
            if let Some(proof) = self.interrupted_finish_orphan_cleanup_proof(session) {
                self.branch_provenance_disposition(
                    session,
                    &proof.head,
                    delivery_targets,
                    landing_cache,
                )?
            } else {
                match self.is_orphaned_worktree_of_this_repository_checked(&worktree_path) {
                    Err(error) => (
                        CleanupDisposition::InspectionFailed,
                        self.cleanup_inspection_failure(&error),
                        None,
                    ),
                    Ok(true) => (
                        CleanupDisposition::UnprovenProvenance,
                        "the orphaned checkout has no matching unfinished finish-cleanup proof; preserved it for inspection or an explicit force cleanup".into(),
                        None,
                    ),
                    Ok(false) if is_orphaned_worktree_directory(&worktree_path) => (
                        CleanupDisposition::InspectionFailed,
                        "the checkout is not a usable worktree and its repository ownership could not be proven; preserved it for inspection".into(),
                        None,
                    ),
                    Ok(false) => {
                        let eligibility = match delivery_targets {
                            CleanupDeliveryTargets::ArchivePreservation(targets) => self
                                .cleanup_eligibility_with_provenance(
                                    &worktree_path,
                                    status_limits,
                                    |head| self.archive_preservation_ancestry(head, targets, status_limits.deadline),
                                ),
                            CleanupDeliveryTargets::Snapshot(targets) => self
                                .cleanup_eligibility_with_status_limits(
                                    session.id,
                                    &worktree_path,
                                    targets,
                                    landing_cache,
                                    status_limits,
                                ),
                            CleanupDeliveryTargets::Fresh => {
                                let targets = self.cleanup_delivery_targets()?;
                                self.cleanup_eligibility_with_status_limits(
                                    session.id,
                                    &worktree_path,
                                    &targets,
                                    landing_cache,
                                    status_limits,
                                )
                            }
                            CleanupDeliveryTargets::SnapshotUnavailable => Ok((
                                CleanupDisposition::InspectionFailed,
                                CLEANUP_TARGET_SNAPSHOT_UNAVAILABLE.into(),
                                None,
                            )),
                        };
                        match eligibility {
                            Ok(result) => result,
                            Err(error) => (
                                CleanupDisposition::InspectionFailed,
                                self.cleanup_inspection_failure(&error),
                                None,
                            ),
                        }
                    }
                }
            }
        } else if let Some(session_head) = branch_tip.as_deref() {
            // Once the worktree is gone, only the branch remains to judge its
            // commit provenance. Existing orphan directories take the
            // proof-bound branch above because their remaining bytes are not
            // described by the commit alone.
            self.branch_provenance_disposition(
                session,
                session_head,
                delivery_targets,
                landing_cache,
            )?
        } else {
            unreachable!("entries without a worktree or a branch return None above")
        };
        if worktree_present
            && let Some(worktree_head) = provenance
                .as_ref()
                .map(|provenance| provenance.session_head.as_str())
            && branch_tip.as_deref() != Some(worktree_head)
        {
            // A detached worktree sitting somewhere other than its branch is
            // two commits to account for, not one, and the provenance above
            // only spoke for the branch. Removing the directory would drop
            // whatever the worktree is detached on.
            //
            // Unless both are durable. Then nothing is dropped, and refusing
            // only keeps a directory whose entire content exists on a remote.
            // Both must prove it: proving one and guessing the other is how a
            // detached head gets discarded quietly.
            let worktree_durable = self.remote_durability_evidence(worktree_head);
            let branch_durable = match branch_tip.as_deref() {
                // No branch left to lose, so the worktree head answers alone.
                None => Some(("(no branch ref)".to_string(), String::new())),
                Some(tip) => self.remote_durability_evidence(tip),
            };
            match (worktree_durable, branch_durable) {
                (Some((worktree_ref, _)), Some(_)) => {
                    disposition = CleanupDisposition::Eligible;
                    reason = format!(
                        "worktree is detached on {}, which diverges from the session branch; both are reachable from remote refs and verified current, so neither is lost ({worktree_ref})",
                        short_commit(worktree_head)
                    );
                }
                _ => {
                    disposition = CleanupDisposition::UnprovenProvenance;
                    reason = "session branch ref is missing or does not match the retained                               worktree HEAD"
                        .into();
                }
            }
        }
        let session_head = provenance
            .as_ref()
            .map(|provenance| provenance.session_head.as_str())
            .or(branch_tip.as_deref());
        let mut inspection_commands = Vec::new();
        if let Some(session_head) = session_head {
            inspection_commands.push(format!("git show --stat --oneline {session_head}"));
        }
        if let Some(provenance) = provenance.as_ref()
            && let Some(accepted_head) = provenance.accepted_session_head.as_deref()
            && accepted_head != provenance.session_head
        {
            inspection_commands.push(format!(
                "git log --oneline {accepted_head}..{}",
                provenance.session_head
            ));
        }
        Ok(Some(CleanupWorktreePlan {
            session_id: session.id,
            worktree_path: session.worktree_path.clone(),
            worktree_present,
            branch_ref,
            branch_tip: branch_tip.clone(),
            delete_branch: branch_tip.is_some(),
            origin: session.origin,
            disposition,
            provenance,
            estimated_bytes,
            estimated_inodes,
            reason,
            inspection_commands,
            force_cleanup_command: format!("aethyme broker finish cleanup {} --force", session.id),
        }))
    }

    /// Read-only inventory of retained broker-owned worktrees belonging to
    /// sessions that are already closed in broker state.
    ///
    /// Walks and sizes every retained worktree. This is the expensive path and
    /// the only one whose digest may authorize a removal.
    pub fn cleanup_plan(&self) -> Result<CleanupPlan, BrokerOpError> {
        self.cleanup_plan_scanned(crate::SizeScan::Measure)
    }

    /// The same inventory, assembled from sizes an earlier walk recorded.
    ///
    /// This is what `broker status` and `doctor` use. It touches no worktree
    /// contents, so its byte totals omit anything never measured -- read
    /// `unmeasured_worktree_count` before reporting them as totals -- and it
    /// carries no digest, because a plan that does not know how big things are
    /// must not be able to authorize removing them.
    ///
    /// Before returning it spends `routine_size_budget_ms` measuring at most
    /// one directory, so the records fill in over successive routine checks
    /// instead of waiting for somebody to run `gc plan` (#176).
    pub fn cleanup_plan_recorded(&self) -> Result<CleanupPlan, BrokerOpError> {
        self.cleanup_plan_recorded_within(HEALTH_CHECK_ELIGIBILITY_BUDGET)
    }

    /// [`Self::cleanup_plan_recorded`] with an explicit eligibility budget.
    /// Once `budget` is spent the remaining worktrees are listed with
    /// eligibility not inspected; the plan carries no digest either way, so a
    /// shorter budget can never authorize a removal.
    pub fn cleanup_plan_recorded_within(
        &self,
        budget: std::time::Duration,
    ) -> Result<CleanupPlan, BrokerOpError> {
        let plan = self.cleanup_plan_scanned_within(crate::SizeScan::Recorded, Some(budget))?;
        self.warm_one_size_record(&plan)?;
        Ok(plan)
    }

    fn cleanup_plan_recorded_until(
        &self,
        deadline: std::time::Instant,
    ) -> Result<CleanupPlan, BrokerOpError> {
        let budget = deadline.saturating_duration_since(std::time::Instant::now());
        let plan = self.cleanup_plan_scanned_within(crate::SizeScan::Recorded, Some(budget))?;
        if std::time::Instant::now() < deadline {
            self.warm_one_size_record(&plan)?;
        }
        Ok(plan)
    }

    /// A recorded-size pass is bounded by [`HEALTH_CHECK_ELIGIBILITY_BUDGET`];
    /// a measuring pass, the only one whose digest authorizes removal, always
    /// inspects every worktree.
    pub(crate) fn cleanup_plan_scanned(
        &self,
        scan: crate::SizeScan,
    ) -> Result<CleanupPlan, BrokerOpError> {
        let budget = (!scan.measures()).then_some(HEALTH_CHECK_ELIGIBILITY_BUDGET);
        self.cleanup_plan_scanned_within(scan, budget)
    }

    pub(crate) fn cleanup_plan_scanned_within(
        &self,
        scan: crate::SizeScan,
        budget: Option<std::time::Duration>,
    ) -> Result<CleanupPlan, BrokerOpError> {
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        self.cleanup_plan_scanned_within_and_cache(scan, budget, &mut landing_cache)
    }

    pub(crate) fn cleanup_plan_scanned_until(
        &self,
        scan: crate::SizeScan,
        deadline: std::time::Instant,
    ) -> Result<(CleanupPlan, bool), BrokerOpError> {
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        let budget_label = (!scan.measures()).then_some(HEALTH_CHECK_ELIGIBILITY_BUDGET);
        let records = crate::measurement::load_size_records_until(&self.main_root, deadline);
        let records_complete = records.is_some();
        let plan = self.cleanup_plan_scanned_core(
            scan,
            Some(deadline),
            budget_label,
            &mut landing_cache,
            records.unwrap_or_default(),
            records_complete,
        )?;
        Ok((plan, records_complete))
    }

    pub(crate) fn cleanup_plan_scanned_within_and_cache(
        &self,
        scan: crate::SizeScan,
        budget: Option<std::time::Duration>,
        landing_cache: &mut crate::representation::LandingVerdictCache,
    ) -> Result<CleanupPlan, BrokerOpError> {
        debug_assert!(
            budget.is_none() || !scan.measures(),
            "a measuring plan authorizes removal and must inspect every worktree"
        );
        let deadline = budget.map(|budget| std::time::Instant::now() + budget);
        self.cleanup_plan_scanned_core(
            scan,
            deadline,
            budget,
            landing_cache,
            crate::measurement::load_size_records(&self.main_root),
            true,
        )
    }

    fn cleanup_plan_scanned_core(
        &self,
        scan: crate::SizeScan,
        deadline: Option<std::time::Instant>,
        budget: Option<std::time::Duration>,
        landing_cache: &mut crate::representation::LandingVerdictCache,
        mut records: crate::measurement::SizeRecords,
        records_complete: bool,
    ) -> Result<CleanupPlan, BrokerOpError> {
        // Cleared on every exit below, including an error return.
        struct ClearOnDrop<'a>(&'a std::cell::Cell<Option<std::time::Instant>>);
        impl Drop for ClearOnDrop<'_> {
            fn drop(&mut self) {
                self.0.set(None);
            }
        }
        self.landing_deadline.set(deadline);
        let _clear_landing_deadline = ClearOnDrop(&self.landing_deadline);
        let target_snapshot = self.cleanup_target_snapshot();
        let mut plan = CleanupPlan {
            target_snapshot: target_snapshot.refs.clone(),
            ..CleanupPlan::default()
        };
        let mut retained = crate::MeasuredTotal::default();
        let mut reclaimable = crate::MeasuredTotal::default();
        let mut known = std::collections::BTreeSet::new();
        // One listing instead of one `rev-parse` per session ever recorded:
        // this plan is on `broker status`'s path through `cleanup_retention`.
        let branch_tips = self.repo.local_branch_tips();
        // An incomplete ownership index cannot authorize any candidate. A
        // bounded health report must defer inspection, rather than fail the
        // entire status command. Unbounded removal plans remain strict.
        let live_checkouts = if deadline.is_some() {
            self.cleanup_live_checkout_snapshot_until(deadline)?
        } else {
            Some(self.cleanup_live_checkout_snapshot(deadline)?)
        };
        let sessions = self.store.cleaned_sessions()?;
        for (index, session) in sessions.iter().enumerate() {
            if session.origin != SessionOrigin::Spawned {
                continue;
            }
            if scan.measures()
                && deadline.is_some()
                && (live_checkouts.is_none()
                    || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline))
            {
                plan.eligibility_not_inspected_count += sessions[index..]
                    .iter()
                    .filter(|session| session.origin == SessionOrigin::Spawned)
                    .count();
                break;
            }
            let mut over_budget = live_checkouts.is_none()
                || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
            let deferred_reason = if scan.measures() {
                "GC plan reached its shared time budget; repeat with `aethyme broker gc plan --full-scan` for a complete inspection".to_owned()
            } else {
                format!(
                    "eligibility not inspected within the {} s health-check budget; run `aethyme broker gc plan` for the full inspection",
                    budget.unwrap_or_default().as_secs()
                )
            };
            let item = if over_budget {
                self.cleanup_item_not_inspected(
                    session,
                    &records,
                    branch_tips.as_ref(),
                    deferred_reason.clone(),
                )
            } else {
                let item = self.cleanup_item_scanned_with_tips(
                    session,
                    scan,
                    &mut records,
                    CleanupInspectionContext {
                        branch_tips: branch_tips.as_ref(),
                        live_checkouts: live_checkouts
                            .as_deref()
                            .expect("an inspected candidate requires a complete ownership index"),
                        delivery_targets: target_snapshot
                            .delivery_targets
                            .as_deref()
                            .map(CleanupDeliveryTargets::Snapshot)
                            .unwrap_or(CleanupDeliveryTargets::SnapshotUnavailable),
                        landing_cache,
                        status_limits: CleanupStatusLimits::new(
                            deadline,
                            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
                            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
                        ),
                        size_deadline: deadline,
                        allow_adopted_path: false,
                    },
                )?;
                if scan.measures()
                    && deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
                {
                    plan.eligibility_not_inspected_count += sessions[index..]
                        .iter()
                        .filter(|session| session.origin == SessionOrigin::Spawned)
                        .count();
                    break;
                }
                // A landing search the deadline stopped reads as a failed
                // inspection; it is the budget, so say so instead (#460).
                if item.as_ref().is_some_and(|item| {
                    item.disposition == CleanupDisposition::InspectionFailed || scan.measures()
                }) && deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
                {
                    over_budget = true;
                    self.cleanup_item_not_inspected(
                        session,
                        &records,
                        branch_tips.as_ref(),
                        deferred_reason,
                    )
                } else {
                    item
                }
            };
            let Some(item) = item else {
                continue;
            };
            if over_budget {
                plan.eligibility_not_inspected_count += 1;
            }
            known.insert(item.worktree_path.clone());
            if item.worktree_present {
                plan.retained_worktree_count += 1;
            }
            if item.branch_tip.is_some() {
                plan.retained_branch_count += 1;
            }
            let measured_at = records
                .get(&item.worktree_path)
                .map(|record| record.measured_at_ms);
            let eligible = item.eligible();
            match (item.estimated_bytes, measured_at) {
                // A worktree that is gone contributes a known zero and no
                // record; there was nothing to walk.
                (Some(bytes), None) => {
                    retained.add_measured(bytes, i64::MAX);
                    if eligible {
                        reclaimable.add_measured(bytes, i64::MAX);
                    }
                }
                (Some(bytes), Some(at)) => {
                    retained.add_measured(bytes, at);
                    if eligible {
                        reclaimable.add_measured(bytes, at);
                    }
                }
                (None, _) => {
                    retained.add_unmeasured();
                    if eligible {
                        reclaimable.add_unmeasured();
                    }
                }
            }
            if eligible {
                if item.worktree_present {
                    plan.eligible_worktree_count += 1;
                }
                if item.branch_tip.is_some() {
                    plan.eligible_branch_count += 1;
                }
            }
            plan.worktrees.push(item);
        }
        plan.estimated_retained_bytes = retained.bytes;
        plan.estimated_reclaimable_bytes = reclaimable.bytes;
        plan.unmeasured_worktree_count = retained.unmeasured;
        if scan.measures() && records_complete {
            // Only a pass that enumerated everything may prune: a routine
            // check sees whatever subset it asked about.
            if deadline.is_none() {
                records.retain_paths(&known);
            }
            crate::warn_unrecorded(
                "save worktree size records",
                crate::measurement::save_size_records(&self.main_root, &records),
            );
            if deadline.is_none() && plan.eligibility_not_inspected_count == 0 {
                let bytes = serde_json::to_vec(&plan)?;
                plan.digest = format!("{:x}", Sha256::digest(bytes));
            }
        }
        // Set after the digest, deliberately. The digest is what an operator
        // confirms and what `cleanup_cleaned_worktrees` re-plans to match, so
        // it must cover what would be removed and nothing else. *When* the
        // sizes were taken is a reporting field; hashing it would make every
        // plan's digest unique to the instant it was built and no confirmation
        // could ever match (#176).
        plan.sizes_measured_at_ms = retained
            .oldest_measured_at_ms
            .filter(|oldest| *oldest != i64::MAX);
        Ok(plan)
    }

    /// Inventory only: one branch listing and filesystem existence checks.
    /// No content comparison, directory walk, or deletion authorization.
    pub(super) fn cleanup_plan_observed(&self) -> Result<(CleanupPlan, usize), BrokerOpError> {
        self.cleanup_plan_observed_with_budget(std::time::Duration::from_millis(250))
    }

    pub(super) fn cleanup_plan_observed_with_budget(
        &self,
        budget: std::time::Duration,
    ) -> Result<(CleanupPlan, usize), BrokerOpError> {
        let mut plan = CleanupPlan::default();
        let records = crate::measurement::load_size_records(&self.main_root);
        let Some(tips) = self.repo.local_branch_tips() else {
            return Ok((plan, self.store.cleaned_sessions()?.len()));
        };
        let mut retained = crate::MeasuredTotal::default();
        let sessions = self.store.cleaned_sessions()?;
        let started = std::time::Instant::now();
        let mut deferred = 0;
        for (index, session) in sessions.iter().enumerate() {
            if started.elapsed() >= budget {
                deferred = sessions.len() - index;
                break;
            }
            if session.origin != SessionOrigin::Spawned {
                continue;
            }
            let Some(item) = self.cleanup_item_not_inspected(
                session,
                &records,
                Some(&tips),
                "eligibility not inspected by routine status".into(),
            ) else {
                continue;
            };
            let record = records.get(&session.worktree_path);
            if item.worktree_present {
                plan.retained_worktree_count += 1;
            }
            if item.branch_tip.is_some() {
                plan.retained_branch_count += 1;
            }
            match item.estimated_bytes {
                Some(bytes) => retained
                    .add_measured(bytes, record.map(|r| r.measured_at_ms).unwrap_or(i64::MAX)),
                None => retained.add_unmeasured(),
            }
            plan.worktrees.push(item);
        }
        plan.estimated_retained_bytes = retained.bytes;
        plan.unmeasured_worktree_count = retained.unmeasured;
        plan.sizes_measured_at_ms = retained.oldest_measured_at_ms.filter(|at| *at != i64::MAX);
        Ok((plan, deferred))
    }

    /// A plan entry for a closed session's worktree that lists what is on
    /// disk without judging eligibility: existence, the branch tip and the
    /// recorded size only. It is never eligible, so it cannot authorize a
    /// removal. `None` when there is nothing left to list.
    pub(super) fn cleanup_item_not_inspected(
        &self,
        session: &Session,
        records: &crate::measurement::SizeRecords,
        tips: Option<&std::collections::BTreeMap<String, String>>,
        reason: String,
    ) -> Option<CleanupWorktreePlan> {
        let path = Path::new(&session.worktree_path);
        if !self.is_broker_owned_worktree(session, path) {
            return None;
        }
        let present = path.exists();
        let branch_ref = format!("refs/heads/{}", session.branch);
        let tip = tips.and_then(|tips| tips.get(&branch_ref).cloned());
        if !present && tip.is_none() {
            return None;
        }
        let (estimated_bytes, estimated_inodes) = if present {
            records
                .get(&session.worktree_path)
                .map(|record| (Some(record.bytes), record.inodes))
                .unwrap_or((None, None))
        } else {
            (Some(0), Some(0))
        };
        Some(CleanupWorktreePlan {
            session_id: session.id,
            worktree_path: session.worktree_path.clone(),
            worktree_present: present,
            branch_ref,
            branch_tip: tip,
            delete_branch: false,
            origin: session.origin,
            disposition: CleanupDisposition::InspectionFailed,
            provenance: None,
            estimated_bytes,
            estimated_inodes,
            reason,
            inspection_commands: vec!["aethyme broker gc plan".into()],
            force_cleanup_command: String::new(),
        })
    }

    /// Measure one directory the broker has never sized, or whose recorded
    /// size has aged out, and write the result down.
    ///
    /// Bounded to one directory and to `routine_size_budget_ms`, because the
    /// caller is a routine check. A measurement that does not finish inside
    /// the budget records nothing: a partial sum written down as a size would
    /// be indistinguishable from a real one afterwards.
    pub(super) fn warm_one_size_record(&self, plan: &CleanupPlan) -> Result<(), BrokerOpError> {
        // Status must remain able to report a malformed policy. It uses the
        // conservative defaults for its picture and simply skips warming when
        // the configured warming budget cannot be trusted.
        let Ok(policy) = crate::load_retention_policy(&self.main_root) else {
            return Ok(());
        };
        let mut records = crate::measurement::load_size_records(&self.main_root);
        let paths = size_warm_candidates(plan);
        let ttl_ms = i64::from(policy.size_record_ttl_hours).saturating_mul(3_600_000);
        let Some(path) = records.next_to_measure(&paths, now_ms(), ttl_ms) else {
            return Ok(());
        };
        // No explicit test for `routine_size_budget_ms == 0`: a walk checks the
        // deadline before its first `read_dir`, so a zero budget cannot
        // complete a measurement and nothing is ever recorded. An early return
        // here would say the same thing twice, and only one of the two could
        // be falsified by a test.
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(policy.routine_size_budget_ms);
        let Some(usage) = crate::disk_headroom::directory_usage_bounded(Path::new(&path), deadline)
        else {
            return Ok(());
        };
        records.record_usage(&path, usage.bytes, Some(usage.inodes), now_ms());
        crate::warn_unrecorded(
            "save worktree size records",
            crate::measurement::save_size_records(&self.main_root, &records),
        );
        Ok(())
    }

    /// Remove every currently eligible worktree from a read-only cleanup
    /// plan. Apply revalidates each worktree independently and never forces a
    /// dirty, adopted, symlinked, or unrepresented checkout.
    pub fn cleanup_cleaned_worktrees(
        &mut self,
        apply: bool,
        confirm: Option<&str>,
    ) -> Result<CleanupSweepReport, BrokerOpError> {
        let plan = self.cleanup_plan()?;
        let mut removed_session_ids = Vec::new();
        let mut failures = Vec::new();
        if apply {
            let confirm = confirm.ok_or(BrokerOpError::CleanupConfirmationNotSha256)?;
            if confirm.len() != 64 || !confirm.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(BrokerOpError::CleanupConfirmationNotSha256);
            }
            if confirm != plan.digest {
                return Err(BrokerOpError::CleanupConfirmationMismatch {
                    actual: confirm.into(),
                });
            }
            for item in plan.worktrees.iter().filter(|item| item.eligible()) {
                match self.cleanup(item.session_id, false) {
                    Ok(()) => removed_session_ids.push(item.session_id),
                    Err(error) => failures.push(CleanupSweepFailure {
                        session_id: item.session_id,
                        reason: error.to_string(),
                    }),
                }
            }
        }
        Ok(CleanupSweepReport {
            applied: apply,
            plan,
            removed_session_ids,
            failures,
        })
    }

    /// The retention picture `broker status` shows.
    ///
    /// Deliberately the recorded-size path. Status is the mandated first step
    /// of every session, so it runs constantly; sizing every retained worktree
    /// here is what made the routine check and the five-minute audit the same
    /// code (#176).
    pub(super) fn cleanup_retention(&self, now_ms: i64) -> Result<CleanupRetention, BrokerOpError> {
        self.cleanup_retention_with_audit(now_ms, true)
    }

    pub(super) fn cleanup_retention_with_audit(
        &self,
        now_ms: i64,
        audit: bool,
    ) -> Result<CleanupRetention, BrokerOpError> {
        let (policy, retention_config) = match crate::load_retention_policy_report(&self.main_root)
        {
            Ok(report) => (
                report.policy,
                RetentionConfigStatus {
                    warnings: report.warnings,
                    error: None,
                },
            ),
            Err(error) => (
                crate::RetentionPolicy::default(),
                RetentionConfigStatus {
                    warnings: Vec::new(),
                    error: Some(error.to_string()),
                },
            ),
        };
        let deadline = crate::git::active_git_deadline();
        let (plan, deferred) = if audit {
            let plan = match deadline {
                Some(deadline) => self.cleanup_plan_recorded_until(deadline)?,
                None => self.cleanup_plan_recorded()?,
            };
            let deferred = plan.eligibility_not_inspected_count;
            (plan, deferred)
        } else {
            self.cleanup_plan_observed()?
        };
        let closed_sessions = self.store.cleaned_sessions()?;
        let (worktree_inodes, worktree_inode_unmeasured, inode_summary_complete) =
            self.recorded_worktree_inode_summary(&closed_sessions, deadline)?;
        let closed_worktrees = crate::retention::ClosedWorktreeSummary::from_cleanup(
            &plan,
            &closed_sessions,
            &self.main_root,
        );
        let mut oldest_closed_at: Option<i64> = None;
        let mut oldest_closed_complete = true;
        for session in &closed_sessions {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                oldest_closed_complete = false;
                break;
            }
            let path = Path::new(&session.worktree_path);
            if let Some(closed_at) = session.closed_at
                && path.exists()
                && self.is_broker_owned_worktree(session, path)
            {
                oldest_closed_at =
                    Some(oldest_closed_at.map_or(closed_at, |oldest| oldest.min(closed_at)));
            }
        }
        let oldest_closed_age_days = oldest_closed_at
            .map(|closed_at| now_ms.saturating_sub(closed_at).max(0) as u64 / 86_400_000)
            .unwrap_or_else(|| {
                if oldest_closed_complete {
                    0
                } else {
                    u64::from(policy.closed_worktrees_days)
                }
            });
        // One reading, consumed by both the severity below and the evidence
        // line in the advice that reports it.
        let (gate_headroom, gate_headroom_deferred) = self.gate_headroom();
        let (host_volume_probe, host_available_bytes) = gate_headroom
            .bytes
            .map(|probe| (Some(probe.path), Some(probe.available)))
            .unwrap_or((None, None));
        let (host_inode_volume_probe, inodes_free) = gate_headroom
            .inodes
            .map(|probe| (Some(probe.path), Some(probe.available)))
            .unwrap_or((None, None));
        let reconciliation = self.reconcile_worktree_directories_until(false, deadline)?;
        let inventory_complete = deferred == 0
            && inode_summary_complete
            && oldest_closed_complete
            && reconciliation.complete;
        let severity = if inventory_complete {
            cleanup_retention_severity(
                plan.retained_worktree_count,
                plan.estimated_retained_bytes,
                oldest_closed_age_days,
                policy.closed_worktrees_days,
                policy.retained_bytes_budget,
            )
        } else {
            StatusAdviceSeverity::Warning
        };
        let estimated_blocked_bytes = plan
            .estimated_retained_bytes
            .saturating_sub(plan.estimated_reclaimable_bytes);
        let retained_total = crate::MeasuredTotal {
            bytes: plan.estimated_retained_bytes,
            measured: plan
                .worktrees
                .len()
                .saturating_sub(plan.unmeasured_worktree_count),
            unmeasured: plan.unmeasured_worktree_count,
            oldest_measured_at_ms: plan.sizes_measured_at_ms,
        };
        let mut retained_total = retained_total;
        retained_total.unmeasured = retained_total
            .unmeasured
            .saturating_add(deferred)
            .saturating_add(usize::from(
                !inode_summary_complete || !oldest_closed_complete || !reconciliation.complete,
            ));
        let budget_verdict = crate::budget_verdict(&retained_total, policy.retained_bytes_budget);
        // Only `Over` asserts that the budget is broken. A floor under the
        // budget is not a pass -- the bytes it skipped are exactly the ones
        // that would have decided it.
        let over_retained_bytes_budget = budget_verdict.exceeded();
        Ok(CleanupRetention {
            inventory_complete,
            inventory_deferred_sessions: deferred,
            gate_headroom_deferred,
            eligibility_checked: audit,
            broker_owned_worktree_count: plan.retained_worktree_count,
            retained_session_branch_count: plan.retained_branch_count,
            eligible_worktree_count: plan.eligible_worktree_count,
            estimated_retained_bytes: plan.estimated_retained_bytes,
            estimated_reclaimable_bytes: plan.estimated_reclaimable_bytes,
            estimated_blocked_bytes,
            retained_bytes_budget: policy.retained_bytes_budget,
            over_retained_bytes_budget,
            retained_bytes_deficit: crate::reclaim_order::deficit_bytes(
                plan.estimated_retained_bytes,
                policy.retained_bytes_budget,
            ),
            clears_retained_bytes_budget: inventory_complete
                && crate::reclaim_order::clears_budget(
                    plan.estimated_retained_bytes,
                    policy.retained_bytes_budget,
                    plan.estimated_reclaimable_bytes,
                ),
            budget_verdict,
            unmeasured_worktree_count: plan.unmeasured_worktree_count,
            sizes_measured_at_ms: plan.sizes_measured_at_ms,
            oldest_closed_age_days,
            closed_worktrees_policy_days: policy.closed_worktrees_days,
            host_available_bytes,
            host_volume_probe,
            host_inode_volume_probe,
            inodes_free,
            worktree_inodes,
            worktree_inode_unmeasured,
            severity,
            retention_config,
            reconciliation,
            closed_worktrees,
        })
    }

    /// Free space on volumes where gate work can write: repository state and
    /// logs, the broker database, the selected session-worktree root, the
    /// exact-tree verification slot, and managed build cache when configured.
    /// Each path is read at its nearest existing directory; this method does
    /// not create a missing root or cache.
    pub(super) fn gate_headroom_probes(&self) -> Vec<PathBuf> {
        let mut probes = std::collections::BTreeSet::from([
            self.main_root.join(".aethyme"),
            self.store
                .db_path()
                .parent()
                .unwrap_or(&self.main_root)
                .to_path_buf(),
        ]);
        let worktree_root_plan = self.worktree_root_plan().ok();
        if let Some(plan) = &worktree_root_plan
            && let Some((root, _)) = crate::broker::lifecycle::worktree_root_candidates_for(plan)
                .into_iter()
                .find(|(root, source)| {
                    *source != crate::WorktreeRootSource::RepositoryFallback
                        && crate::verification::removable_volume_unavailable_reason(root).is_none()
                })
        {
            probes.insert(root);
        }
        let verification = worktree_root_plan.as_ref().map_or_else(
            || crate::verification::plan_slot_placement(&self.main_root, "merge-sim"),
            |plan| {
                crate::verification::plan_slot_placement_with_worktree_root_plan(
                    &self.main_root,
                    "merge-sim",
                    plan,
                )
            },
        );
        if crate::verification::removable_volume_unavailable_reason(&verification.directory)
            .is_none()
        {
            probes.insert(verification.directory);
        }

        let (uses_managed_cache, uses_host_resource_db) = crate::gates::load_gates(&self.main_root)
            .map(|gates| {
                (
                    gates.iter().any(|gate| gate.managed_cache.is_some()),
                    gates
                        .iter()
                        .any(|gate| !gate.resources.is_empty() || gate.managed_cache.is_some()),
                )
            })
            .unwrap_or_default();
        let gate_storage = self.storage_context().gate_storage_locations();
        if uses_managed_cache && let Some(cache) = gate_storage.cache_dir() {
            probes.insert(cache);
        }
        if uses_host_resource_db && let Ok(database) = gate_storage.resource_db_path() {
            probes.insert(database.parent().unwrap_or(&self.main_root).to_path_buf());
        }
        probes.into_iter().collect()
    }

    fn gate_headroom(&self) -> (GateHeadroom, bool) {
        let deadline = crate::git::active_git_deadline();
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return (GateHeadroom::default(), true);
        }
        let probes = self.gate_headroom_probes();
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return (GateHeadroom::default(), true);
        }
        let headroom = lowest_headroom_with(&probes, |probe| {
            crate::disk_headroom::available_headroom_at_or_above_for(&self.main_root, probe)
        });
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            (GateHeadroom::default(), true)
        } else {
            (headroom, false)
        }
    }

    pub(super) fn recorded_worktree_inode_summary(
        &self,
        closed_sessions: &[Session],
        deadline: Option<std::time::Instant>,
    ) -> Result<(u64, usize, bool), BrokerOpError> {
        let mut paths = std::collections::BTreeSet::new();
        let live_sessions = self.store.live_sessions()?;
        let total_sessions = live_sessions.len().saturating_add(closed_sessions.len());
        let mut complete = true;
        let mut unmeasured = 0_usize;
        for (index, session) in live_sessions
            .iter()
            .chain(closed_sessions.iter())
            .enumerate()
        {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                complete = false;
                unmeasured = unmeasured.saturating_add(total_sessions.saturating_sub(index));
                break;
            }
            if session.origin != SessionOrigin::Spawned {
                continue;
            }
            let path = Path::new(&session.worktree_path);
            if is_real_directory(path) && self.is_broker_owned_worktree(session, path) {
                paths.insert(session.worktree_path.clone());
            }
        }

        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            complete = false;
            unmeasured = unmeasured.saturating_add(paths.len());
            return Ok((0, unmeasured, complete));
        }
        let records = crate::measurement::load_size_records(&self.main_root);
        let mut known = 0_u64;
        let path_count = paths.len();
        for (index, path) in paths.into_iter().enumerate() {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                complete = false;
                unmeasured = unmeasured.saturating_add(path_count.saturating_sub(index));
                break;
            }
            match records.get(&path).and_then(|record| record.inodes) {
                Some(inodes) => known = known.saturating_add(inodes),
                None => unmeasured = unmeasured.saturating_add(1),
            }
        }
        Ok((known, unmeasured, complete))
    }

    /// The exact durable proof captured before finish began removing this
    /// checkout. Older cleanup-started events remain readable, but their
    /// missing proof cannot authorize deleting an orphan whose contents Git
    /// can no longer inspect.
    fn finish_cleanup_recovery_intent(
        &self,
        session: &Session,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        let current = self.store.session(session.id).ok()?;
        if current.origin != SessionOrigin::Spawned
            || current.cleanup_state != crate::SessionCleanupState::Closed
            || current.worktree_path != session.worktree_path
            || current.branch != session.branch
        {
            return None;
        }
        let event = self
            .store
            .latest_session_finish_cleanup_started_event(session.id)
            .ok()??;
        let payload = event
            .payload_json
            .as_deref()
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())?;
        if payload
            .get("worktree_present")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
            || payload
                .get("branch_present")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
        {
            return None;
        }
        let proof = serde_json::from_value::<crate::events::FinishCleanupRecoveryProof>(
            payload.get("recovery_proof")?.clone(),
        )
        .ok()?;
        let schema_valid = proof.schema_version == 3
            && validate_disposable_prefixes(&proof.disposable_ignored_root_prefixes).is_ok()
            && proof
                .root_generation_sha256
                .as_deref()
                .is_some_and(crate::root_generation::valid_digest);
        if !schema_valid
            || proof.branch_ref != format!("refs/heads/{}", current.branch)
            || proof.head != proof.branch_tip
        {
            return None;
        }
        Some(proof)
    }

    /// Remove a session's worktree and mark it cleaned. Refuses when the
    /// worktree has uncommitted changes or commits not represented on a
    /// delivery target, unless `force`.
    pub(super) fn interrupted_finish_cleanup_proof(
        &self,
        session: &Session,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        self.interrupted_finish_cleanup_proof_with_status_limits(
            session,
            CleanupStatusLimits::new(
                self.landing_deadline.get(),
                crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
                crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            ),
        )
    }

    fn interrupted_finish_cleanup_proof_with_status_limits(
        &self,
        session: &Session,
        status_limits: CleanupStatusLimits,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        let current = self.store.session(session.id).ok()?;
        let proof = self.finish_cleanup_recovery_intent(&current)?;

        let worktree_path = Path::new(&current.worktree_path);
        let path_metadata = std::fs::symlink_metadata(worktree_path).ok()?;
        if path_metadata.file_type().is_symlink() || !path_metadata.is_dir() {
            return None;
        }
        let canonical_path = worktree_path.canonicalize().ok()?;
        if canonical_path.to_str()? != proof.worktree_path
            || !self
                .is_broker_owned_worktree_checked(&current, worktree_path)
                .ok()?
        {
            return None;
        }
        let checkout = GitRepo::discover(&canonical_path).ok()?;
        let mut checkout_proof = checkout.finish_cleanup_recovery_proof(&proof.branch_ref)?;
        // Git can reconstruct the checkout identity while its metadata still
        // exists, but only the event carries the captured disposable roots.
        checkout_proof.schema_version = proof.schema_version;
        checkout_proof.disposable_ignored_root_prefixes =
            proof.disposable_ignored_root_prefixes.clone();
        if checkout.root() != canonical_path
            || checkout_proof != proof
            || self.repo.resolve_ref(&proof.branch_ref).as_deref()
                != Some(proof.branch_tip.as_str())
        {
            return None;
        }
        let cleanup_policy = crate::AutoCleanupPolicy::load(self.main_root()).ok()?;
        if !checkout
            .has_only_unstaged_tracked_deletions_until_with_limits(
                &cleanup_policy,
                status_limits.deadline,
                status_limits.output_bytes,
                status_limits.path_fields,
            )
            .ok()?
            || !checkout
                .is_dirty_until_with_limits(
                    status_limits.deadline,
                    status_limits.output_bytes,
                    status_limits.path_fields,
                )
                .ok()?
        {
            return None;
        }

        let inventory = self.repo.worktree_inventory().ok()?;
        let matching_path = inventory
            .iter()
            .filter(|entry| entry.path.canonicalize().ok().as_ref() == Some(&canonical_path))
            .collect::<Vec<_>>();
        if matching_path.len() != 1 {
            return None;
        }
        let registration = matching_path[0];
        if registration.prunable
            || registration.bare
            || (registration.locked
                && registration.lock_reason.as_deref()
                    != Some(crate::git::BROKER_WORKTREE_LOCK_REASON))
            || registration.head.as_deref() != Some(proof.head.as_str())
            || registration.branch.as_deref() != Some(proof.branch_ref.as_str())
            || inventory
                .iter()
                .filter(|entry| entry.branch.as_deref() == Some(proof.branch_ref.as_str()))
                .count()
                != 1
        {
            return None;
        }

        let delivery_targets = self.cleanup_delivery_targets().ok()?;
        let (provenance, _) = self
            .cleanup_provenance(&current, &proof.head, &delivery_targets)
            .ok()?;
        if provenance.representation != CleanupRepresentation::Represented
            || self
                .refuse_live_checkout(current.id, worktree_path)
                .is_err()
        {
            return None;
        }
        Some(proof)
    }

    /// Validate a persisted finish removal after Git has already deregistered
    /// the checkout. The start event is written only after bounded cleanup
    /// eligibility has checked status under the lifecycle claim; its durable
    /// proof remains the authority when a partial removal has already deleted
    /// the worktree admin/index and fresh `git status` is no longer possible.
    /// The recorded root, pointer, admin directory, common directory, branch,
    /// and head identities bind that authority to this one checkout. A
    /// missing `.git` pointer is acceptable only under that exact proof,
    /// because a partial `remove_dir_all` may have unlinked it before dying.
    fn interrupted_finish_orphan_cleanup_proof(
        &self,
        _session: &Session,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        #[cfg(unix)]
        {
            self.interrupted_finish_orphan_cleanup_proof_unix(_session)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    #[cfg(unix)]
    fn interrupted_finish_orphan_cleanup_proof_unix(
        &self,
        session: &Session,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        use std::os::unix::fs::MetadataExt;

        let current = self.store.session(session.id).ok()?;
        let proof = self.finish_cleanup_recovery_intent(&current)?;
        if current.origin != SessionOrigin::Spawned
            || current.cleanup_state != crate::SessionCleanupState::Closed
            || current.worktree_path != session.worktree_path
            || current.branch != session.branch
        {
            return None;
        }

        let worktree_path = Path::new(&current.worktree_path);
        let path_metadata = std::fs::symlink_metadata(worktree_path).ok()?;
        if path_metadata.file_type().is_symlink()
            || !path_metadata.is_dir()
            || path_metadata.dev() != proof.worktree_device
            || path_metadata.ino() != proof.worktree_inode
        {
            return None;
        }
        let (generation_device, generation_inode, generation_sha256) =
            crate::root_generation::read_path_with_identity(worktree_path)?;
        if (generation_device, generation_inode) != (proof.worktree_device, proof.worktree_inode)
            || proof.root_generation_sha256.as_deref() != Some(generation_sha256.as_str())
        {
            return None;
        }
        let canonical_path = worktree_path.canonicalize().ok()?;
        if canonical_path.to_str()? != proof.worktree_path
            || !self
                .is_broker_owned_worktree_checked(&current, worktree_path)
                .ok()?
        {
            return None;
        }

        let common_path = self.main_git_common_dir().ok()?.canonicalize().ok()?;
        let common_metadata = std::fs::symlink_metadata(&common_path).ok()?;
        if common_metadata.file_type().is_symlink()
            || !common_metadata.is_dir()
            || common_path.to_str()? != proof.git_common_path
            || common_metadata.dev() != proof.git_common_device
            || common_metadata.ino() != proof.git_common_inode
        {
            return None;
        }
        let worktrees_path = common_path.join("worktrees");
        let worktrees_metadata = std::fs::symlink_metadata(&worktrees_path).ok()?;
        if worktrees_metadata.file_type().is_symlink() || !worktrees_metadata.is_dir() {
            return None;
        }
        let canonical_worktrees = worktrees_path.canonicalize().ok()?;
        let admin_path = Path::new(&proof.git_admin_path);
        let admin_name = admin_path.file_name()?;
        if !admin_path.is_absolute()
            || !matches!(admin_name.to_str(), Some(name) if !name.is_empty() && name != "." && name != "..")
            || canonical_worktrees.join(admin_name) != admin_path
        {
            return None;
        }
        match std::fs::symlink_metadata(admin_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) | Err(_) => return None,
        }

        let pointer_path = canonical_path.join(".git");
        match std::fs::symlink_metadata(&pointer_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
            Ok(_) => {
                let witness =
                    read_bounded_ownership_witness_with_metadata(&pointer_path).ok()??;
                if witness.metadata.dev() != proof.git_pointer_device
                    || witness.metadata.ino() != proof.git_pointer_inode
                {
                    return None;
                }
                let pointer_text = std::str::from_utf8(&witness.bytes).ok()?;
                let pointed_admin = pointer_text
                    .lines()
                    .find_map(|line| line.strip_prefix("gitdir:"))?
                    .trim();
                let pointed_admin = PathBuf::from(pointed_admin);
                let pointed_admin = if pointed_admin.is_absolute() {
                    pointed_admin
                } else {
                    canonical_path.join(pointed_admin)
                };
                let pointed_admin_parent = pointed_admin.parent()?.canonicalize().ok()?;
                let pointed_admin = pointed_admin_parent.join(pointed_admin.file_name()?);
                if pointed_admin != admin_path {
                    return None;
                }
            }
        }

        let expected_branch_ref = format!("refs/heads/{}", current.branch);
        if proof.branch_ref != expected_branch_ref
            || proof.head != proof.branch_tip
            || self.repo.resolve_ref(&expected_branch_ref).as_deref()
                != Some(proof.branch_tip.as_str())
        {
            return None;
        }
        let inventory = self.repo.worktree_inventory().ok()?;
        if inventory.iter().any(|entry| {
            entry.branch.as_deref() == Some(expected_branch_ref.as_str())
                || entry.path == canonical_path
                || entry.path.canonicalize().ok().as_ref() == Some(&canonical_path)
        }) {
            return None;
        }
        let delivery_targets = self.cleanup_delivery_targets().ok()?;
        let (provenance, _) = self
            .cleanup_provenance(&current, &proof.head, &delivery_targets)
            .ok()?;
        if provenance.representation != CleanupRepresentation::Represented
            || self
                .refuse_live_checkout(current.id, worktree_path)
                .is_err()
        {
            return None;
        }
        Some(proof)
    }

    fn resume_interrupted_finish_cleanup(
        &mut self,
        session: &Session,
        expected_proof: &crate::events::FinishCleanupRecoveryProof,
        status_limits: CleanupStatusLimits,
    ) -> Result<(), BrokerOpError> {
        let worktree_path = PathBuf::from(&session.worktree_path);
        self.refuse_live_checkout(session.id, &worktree_path)?;
        if self.interrupted_finish_cleanup_proof_with_status_limits(session, status_limits)
            != Some(expected_proof.clone())
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the interrupted finish cleanup identity or safety checks changed; inspect the worktree before retrying".into(),
            });
        }

        let policy = crate::AutoCleanupPolicy::load(self.main_root()).map_err(|reason| {
            BrokerOpError::DirtyWorktree {
                id: session.id,
                reason,
            }
        })?;
        if let Some(reason) = crate::auto_cleanup::ignored_content_reason(&worktree_path, &policy)
            .map_err(|reason| BrokerOpError::DirtyWorktree {
            id: session.id,
            reason,
        })? {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason,
            });
        }

        // `--force` is confined to this validated interrupted finish. The
        // exact registered checkout, represented head, and unstaged-deletion
        // only status were rechecked immediately above. Preserve Git's
        // refusal if its registration or the directory changed after the
        // proof; the generic orphan fallback is not authorized here.
        self.repo.worktree_remove_registered(&worktree_path, true)?;
        match std::fs::symlink_metadata(&worktree_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(BrokerOpError::DirtyWorktree {
                    id: session.id,
                    reason: "the interrupted finish worktree path still exists after Git removal"
                        .into(),
                });
            }
            Err(source) => {
                return Err(BrokerError::Io {
                    path: worktree_path.clone(),
                    source,
                }
                .into());
            }
        }

        self.refuse_live_checkout(session.id, &worktree_path)?;
        let current = self.store.session(session.id)?;
        if current.cleanup_state != crate::SessionCleanupState::Closed
            || current.origin != SessionOrigin::Spawned
            || current.worktree_path != session.worktree_path
            || current.branch != session.branch
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the session is no longer the same closed cleanup owner".into(),
            });
        }
        let inventory = self.repo.worktree_inventory()?;
        if inventory
            .iter()
            .any(|entry| entry.branch.as_deref() == Some(expected_proof.branch_ref.as_str()))
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the session branch is now checked out by a registered worktree".into(),
            });
        }
        let delivery_targets = self.cleanup_delivery_targets()?;
        let (provenance, _) =
            self.cleanup_provenance(&current, &expected_proof.head, &delivery_targets)?;
        if provenance.representation != CleanupRepresentation::Represented {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the session head is no longer represented on a delivery target".into(),
            });
        }
        self.repo
            .delete_branch_ref_checked(&session.branch, &expected_proof.branch_tip)?;
        self.mark_removed_checkout_sessions_cleaned(&[session.id], session.id)?;
        Ok(())
    }

    fn resume_interrupted_finish_orphan_cleanup(
        &mut self,
        session: &Session,
        expected_proof: &crate::events::FinishCleanupRecoveryProof,
    ) -> Result<(), BrokerOpError> {
        let worktree_path = PathBuf::from(&session.worktree_path);
        self.refuse_live_checkout(session.id, &worktree_path)?;
        if self.interrupted_finish_orphan_cleanup_proof(session) != Some(expected_proof.clone()) {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the interrupted orphan cleanup identity or safety checks changed; inspect the remaining files before retrying".into(),
            });
        }

        let policy = crate::AutoCleanupPolicy::load(self.main_root()).map_err(|reason| {
            BrokerOpError::DirtyWorktree {
                id: session.id,
                reason,
            }
        })?;
        if policy.strict {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "strict cleanup preserves interrupted orphan residue; Git metadata is unavailable to classify it, so remove it manually".into(),
            });
        }

        if let Err(reason) =
            remove_authorized_orphan_tree(&self.repo, &worktree_path, expected_proof)
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: format!(
                    "interrupted orphan removal preserved unverified residue and retained the branch and session for retry: {reason}"
                ),
            });
        }
        require_worktree_path_absent(&worktree_path, session.id)?;
        self.refuse_live_checkout(session.id, &worktree_path)?;
        let current = self.store.session(session.id)?;
        if current.cleanup_state != crate::SessionCleanupState::Closed
            || current.origin != SessionOrigin::Spawned
            || current.worktree_path != session.worktree_path
            || current.branch != session.branch
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the session is no longer the same closed orphan cleanup owner".into(),
            });
        }
        self.ensure_orphan_branch_not_checked_out(
            session.id,
            &expected_proof.branch_ref,
            &expected_proof.branch_tip,
            None,
        )?;
        let delivery_targets = self.cleanup_delivery_targets()?;
        let (provenance, _) =
            self.cleanup_provenance(&current, &expected_proof.head, &delivery_targets)?;
        if provenance.representation != CleanupRepresentation::Represented {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the session head is no longer represented on a delivery target".into(),
            });
        }
        self.repo
            .delete_branch_ref_checked(&session.branch, &expected_proof.branch_tip)?;
        self.mark_removed_checkout_sessions_cleaned(&[session.id], session.id)?;
        Ok(())
    }

    fn remove_proven_orphan_worktree(
        &self,
        session: &Session,
        item: Option<&CleanupWorktreePlan>,
        worktree_path: &Path,
        force: bool,
    ) -> Result<(String, String), BrokerOpError> {
        let current = self.store.session(session.id)?;
        let Some(item) = item else {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason:
                    "the orphaned worktree has no current cleanup plan; preserved it for inspection"
                        .into(),
            });
        };
        let expected_branch_ref = format!("refs/heads/{}", session.branch);
        if current.cleanup_state != crate::SessionCleanupState::Closed
            || current.origin != SessionOrigin::Spawned
            || current.worktree_path != session.worktree_path
            || current.branch != session.branch
            || item.session_id != session.id
            || item.origin != SessionOrigin::Spawned
            || item.worktree_path != session.worktree_path
            || !item.worktree_present
            || (!force && !item.eligible())
            || item.branch_ref != expected_branch_ref
            || (!force
                && item.provenance.as_ref().is_none_or(|provenance| {
                    provenance.representation != CleanupRepresentation::Represented
                }))
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the orphaned checkout lacks a current represented cleanup proof; preserved it for inspection".into(),
            });
        }
        let branch_tip =
            item.branch_tip
                .as_deref()
                .ok_or_else(|| {
                    BrokerOpError::DirtyWorktree {
                id: session.id,
                reason:
                    "the orphaned checkout has no branch tip to preserve; left it for inspection"
                        .into(),
            }
                })?;
        if self.repo.resolve_ref(&expected_branch_ref).as_deref() != Some(branch_tip) {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the orphaned checkout branch moved after its cleanup plan; preserved it for inspection".into(),
            });
        }
        if !force
            && self
                .interrupted_finish_orphan_cleanup_proof(session)
                .is_none()
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "non-force orphan removal requires the matching unfinished finish-cleanup proof; preserved the checkout for inspection".into(),
            });
        }
        if !self.is_broker_owned_worktree_checked(&current, worktree_path)?
            || !self.is_orphaned_worktree_of_this_repository_checked(worktree_path)?
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the worktree is not a positively identified orphan of this repository; preserved it for inspection".into(),
            });
        }
        let canonical_path = worktree_path
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: worktree_path.to_path_buf(),
                source,
            })?;
        self.ensure_orphan_branch_not_checked_out(
            session.id,
            &expected_branch_ref,
            branch_tip,
            Some(&canonical_path),
        )?;
        self.refuse_live_checkout(session.id, worktree_path)?;

        // This path is no longer registered, so Git cannot remove it. Reach
        // this filesystem removal only with the current represented branch,
        // a same-repository `.git` pointer whose admin entry is explicitly
        // absent, and the session/path ownership checks above. In particular,
        // a Git discovery error alone never authorizes recursive removal.
        if !self.is_orphaned_worktree_of_this_repository_checked(worktree_path)? {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the orphan identity changed before removal; preserved the checkout".into(),
            });
        }
        let outcome = crate::removal::remove_tree(worktree_path);
        if !outcome.removed {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: format!(
                    "orphan removal stopped after {} attempt(s); preserved the branch and session for retry: {}",
                    outcome.attempts,
                    outcome.error.as_deref().unwrap_or("the path remains")
                ),
            });
        }
        Ok((expected_branch_ref, branch_tip.to_owned()))
    }

    fn ensure_orphan_branch_not_checked_out(
        &self,
        session_id: i64,
        branch_ref: &str,
        expected_tip: &str,
        worktree_path: Option<&Path>,
    ) -> Result<(), BrokerOpError> {
        if self.repo.resolve_ref(branch_ref).as_deref() != Some(expected_tip) {
            return Err(BrokerOpError::DirtyWorktree {
                id: session_id,
                reason: "the orphan cleanup branch tip changed; preserved the branch and session for inspection".into(),
            });
        }
        let canonical_path =
            worktree_path.map(|path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
        let inventory = self.repo.worktree_inventory()?;
        if inventory.iter().any(|entry| {
            entry.branch.as_deref() == Some(branch_ref)
                || canonical_path.as_ref().is_some_and(|canonical| {
                    entry.path == *canonical
                        || entry.path.canonicalize().ok().as_ref() == Some(canonical)
                })
        }) {
            return Err(BrokerOpError::DirtyWorktree {
                id: session_id,
                reason: "the orphan cleanup branch or path is registered by another worktree; preserved it for inspection".into(),
            });
        }
        Ok(())
    }

    pub fn cleanup(&mut self, session_id: i64, force: bool) -> Result<(), BrokerOpError> {
        let initial_session = self.store.session(session_id)?;
        // Adoption and checkout creation take the same repository-wide claim
        // before changing ownership. Hold it from the final proof/live-session
        // checks through Git removal and the terminal database update.
        let _lifecycle_claim = self.claim_worktree_lifecycle()?;
        let session = self.store.session(session_id)?;
        ensure_lifecycle_session_unchanged(session_id, &initial_session, &session)?;
        // Preserve the idempotent success case, but never reinterpret a path
        // that reappeared after recorded cleanup as belonging to this row.
        if session.cleanup_state == crate::SessionCleanupState::Cleaned
            && session.status == SessionStatus::Cleaned
        {
            let worktree_path = Path::new(&session.worktree_path);
            match std::fs::symlink_metadata(worktree_path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.mark_removed_checkout_sessions_cleaned(&[session_id], session_id)?;
                }
                Ok(_) => {
                    return Err(BrokerOpError::CleanupSessionNotTerminal {
                        id: session_id,
                        state: "cleaned; the recorded worktree path has since been recreated"
                            .into(),
                    });
                }
                Err(source) => {
                    return Err(BrokerError::Io {
                        path: worktree_path.to_path_buf(),
                        source,
                    }
                    .into());
                }
            }
            return Ok(());
        }
        self.cleanup_under_lifecycle_claim(&session, force, None)
    }

    /// Remove a worktree only after an archive was written and verified.
    /// The exact reviewed plan, including protected ignored files and policy,
    /// is recomputed under the lifecycle claim immediately before removal.
    pub(crate) fn cleanup_after_recovery_archive(
        &mut self,
        session_id: i64,
        archived_plan: &crate::CleanupResolvePlan,
        archive_path: &Path,
    ) -> Result<(), BrokerOpError> {
        let initial_session = self.store.session(session_id)?;
        let _lifecycle_claim = self.claim_worktree_lifecycle()?;
        let session = self.store.session(session_id)?;
        ensure_lifecycle_session_unchanged(session_id, &initial_session, &session)?;
        self.cleanup_under_lifecycle_claim(
            &session,
            true,
            Some(CleanupArchiveGuard::LegacySpawned {
                plan: archived_plan,
                archive_path,
            }),
        )
    }

    /// Remove an explicitly reviewed retirement target only after its archive
    /// was written and verified. The opt-in is per call and must agree with
    /// the plan's captured origin; adopted session branches are retained.
    pub(crate) fn cleanup_after_recovery_archive_for_retirement(
        &mut self,
        session_id: i64,
        archived_plan: &crate::CleanupResolvePlan,
        archive_path: &Path,
        allow_adopted: bool,
        expected_cohort: Option<&crate::auto_cleanup::ClosedArtifactCheckoutCohort>,
    ) -> Result<(), BrokerOpError> {
        let initial_session = self.store.session(session_id)?;
        let _lifecycle_claim = self.claim_worktree_lifecycle()?;
        let session = self.store.session(session_id)?;
        ensure_lifecycle_session_unchanged(session_id, &initial_session, &session)?;
        if archived_plan.session_id != session_id
            || archived_plan.target_origin != session.origin
            || (session.origin == SessionOrigin::Adopted && !allow_adopted)
            || expected_cohort.is_some_and(|cohort| cohort.owner_session_id() != session_id)
        {
            return Err(BrokerOpError::CleanupResolveRefused {
                id: session_id,
                reason: "retirement archive does not match the exact closed session and explicit origin opt-in".into(),
            });
        }
        self.cleanup_under_lifecycle_claim(
            &session,
            true,
            Some(CleanupArchiveGuard::Retirement {
                plan: archived_plan,
                archive_path,
                allow_adopted,
                expected_cohort,
            }),
        )
    }

    /// Perform cleanup while the caller holds the repository lifecycle claim.
    /// Finish uses this after recording its durable cleanup intent under the
    /// same claim; reacquiring here would deadlock on the persistent lock.
    pub(super) fn cleanup_under_lifecycle_claim(
        &mut self,
        session: &Session,
        force: bool,
        archive_guard: Option<CleanupArchiveGuard<'_>>,
    ) -> Result<(), BrokerOpError> {
        self.cleanup_under_lifecycle_claim_with_status_limits(
            session,
            force,
            archive_guard,
            CleanupStatusLimits::new(
                self.landing_deadline.get(),
                crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
                crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            ),
        )
    }

    fn cleanup_under_lifecycle_claim_with_status_limits(
        &mut self,
        session: &Session,
        force: bool,
        archive_guard: Option<CleanupArchiveGuard<'_>>,
        status_limits: CleanupStatusLimits,
    ) -> Result<(), BrokerOpError> {
        let session_id = session.id;
        // Physical removal is only valid after an explicit terminal
        // transition. In particular, --force cannot turn an open session's
        // checkout into a cleanup candidate.
        if !session.status.is_closed()
            || session.cleanup_state != crate::SessionCleanupState::Closed
        {
            return Err(BrokerOpError::CleanupSessionNotTerminal {
                id: session_id,
                state: format!(
                    "status={}, cleanup_state={}",
                    session.status.as_str(),
                    session.cleanup_state.as_str()
                ),
            });
        }
        let _session_commit_claim =
            crate::file_lock::try_acquire_session_commit_lock(self.main_root(), session_id)
                .map_err(|source| BrokerOpError::DirtyWorktree {
                    id: session_id,
                    reason: format!(
                        "the session-local commit lane is busy or unavailable; preserved the worktree: {source}"
                    ),
                })?;
        let worktree_path = PathBuf::from(&session.worktree_path);
        // Report the live owner before an eligibility verdict can turn the
        // same protection into a generic dirty-content refusal. The lifecycle
        // claim remains held and destructive branches repeat this guard.
        self.refuse_live_checkout(session_id, &worktree_path)?;
        let artifact_use_reclaimer = if worktree_path_is_real_directory(&worktree_path, session_id)?
        {
            match crate::file_lock::artifact_use::try_artifact_use_reclaimer(
                self.main_root(),
                &worktree_path,
            ) {
                Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(reclaimer)) => {
                    Some(reclaimer)
                }
                Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(
                    crate::file_lock::artifact_use::ArtifactUseReclaimBlock::ProtocolUnavailable,
                )) => {
                    #[cfg(not(unix))]
                    {
                        if archive_guard.is_some_and(|guard| guard.expected_cohort().is_some()) {
                            return Err(BrokerOpError::DirtyWorktree {
                                id: session_id,
                                reason:
                                    "artifact-use protection is unavailable; preserved the worktree"
                                        .into(),
                            });
                        }
                        // Keep the pre-existing explicit cleanup contract on
                        // targets that cannot run the artifact-use protocol.
                        // Automatic compact cleanup remains fail-closed above.
                        None
                    }
                    #[cfg(unix)]
                    {
                        return Err(BrokerOpError::DirtyWorktree {
                            id: session_id,
                            reason:
                                "artifact-use protection is unavailable; preserved the worktree"
                                    .into(),
                        });
                    }
                }
                Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(block)) => {
                    let reason = match block {
                        crate::file_lock::artifact_use::ArtifactUseReclaimBlock::Contended => {
                            "an artifact producer is active; preserved the worktree"
                        }
                        crate::file_lock::artifact_use::ArtifactUseReclaimBlock::UnknownProducer {
                            ..
                        } => "artifact-use markers are unresolved; preserved the worktree",
                        crate::file_lock::artifact_use::ArtifactUseReclaimBlock::ProtocolUnavailable => {
                            unreachable!("unavailable protocol is handled above")
                        }
                    };
                    return Err(BrokerOpError::DirtyWorktree {
                        id: session_id,
                        reason: reason.into(),
                    });
                }
                Err(source) => {
                    return Err(BrokerOpError::DirtyWorktree {
                        id: session_id,
                        reason: format!(
                            "artifact-use state could not be verified; preserved the worktree: {source}"
                        ),
                    });
                }
            }
        } else {
            None
        };
        let _artifact_use_reclaimer = artifact_use_reclaimer;
        if archive_guard.is_some_and(|guard| guard.plan().target_origin != session.origin) {
            return Err(BrokerOpError::CleanupResolveRefused {
                id: session_id,
                reason: "the archived target origin no longer matches the registered session"
                    .into(),
            });
        }
        if archive_guard.is_some()
            && session.origin != SessionOrigin::Spawned
            && !archive_guard.is_some_and(|guard| guard.is_retirement() && guard.permits_adopted())
        {
            return Err(BrokerOpError::CleanupResolveRefused {
                id: session_id,
                reason:
                    "only an explicitly adopted retirement plan may archive an adopted checkout"
                        .into(),
            });
        }
        if !force
            && let Some(proof) =
                self.interrupted_finish_cleanup_proof_with_status_limits(session, status_limits)
        {
            return self.resume_interrupted_finish_cleanup(session, &proof, status_limits);
        }
        if !force && let Some(proof) = self.interrupted_finish_orphan_cleanup_proof(session) {
            return self.resume_interrupted_finish_orphan_cleanup(session, &proof);
        }
        let retirement_adopted =
            archive_guard.is_some_and(|guard| guard.is_retirement() && guard.permits_adopted());
        if session.origin == SessionOrigin::Adopted && !retirement_adopted {
            if worktree_path_is_real_directory(&worktree_path, session_id)? {
                if !force {
                    let targets = self.cleanup_delivery_targets()?;
                    let mut landing_cache = crate::representation::LandingVerdictCache::default();
                    let (disposition, reason, _) = self.cleanup_eligibility_with_status_limits(
                        session_id,
                        &worktree_path,
                        &targets,
                        &mut landing_cache,
                        status_limits,
                    )?;
                    if disposition != CleanupDisposition::Eligible {
                        return Err(BrokerOpError::DirtyWorktree {
                            id: session_id,
                            reason,
                        });
                    }
                }
                self.refuse_live_checkout(session_id, &worktree_path)?;
                self.repo
                    .worktree_remove_registered(&worktree_path, force)?;
            }
            require_worktree_path_absent(&worktree_path, session_id)?;
            self.mark_removed_checkout_sessions_cleaned(&[session_id], session_id)?;
            return Ok(());
        }
        // Recorded sizes, not a walk: removal needs the provenance verdict,
        // not the byte count, and a walk here only widens the window between
        // the live-checkout check below and the removal.
        let mut records = crate::measurement::SizeRecords::default();
        let item = if let Some(archive_guard) = archive_guard {
            self.cleanup_item_for_recovery_archive_with_limits(
                session,
                crate::SizeScan::Recorded,
                &mut records,
                status_limits,
                &archive_guard.plan().delivery_targets,
                archive_guard.permits_adopted(),
            )?
        } else {
            self.cleanup_item_scanned_with_limits(
                session,
                crate::SizeScan::Recorded,
                &mut records,
                status_limits,
            )?
        };
        if !force
            && let Some(item) = item.as_ref()
            && !item.eligible()
        {
            let inspection = if item.inspection_commands.is_empty() {
                String::new()
            } else {
                format!(
                    "; inspect first with: {}",
                    item.inspection_commands.join("; ")
                )
            };
            return Err(BrokerOpError::DirtyWorktree {
                id: session_id,
                reason: format!(
                    "{}{}; exact discard command: {}",
                    item.reason, inspection, item.force_cleanup_command
                ),
            });
        }

        let mut removed_orphan_branch = None;
        let mut removed_checkout_session_ids = vec![session_id];
        let retained_checkout_present =
            worktree_path_is_real_directory(&worktree_path, session_id)?;
        if archive_guard.is_some() && !retained_checkout_present {
            return Err(BrokerOpError::CleanupResolveRefused {
                id: session_id,
                reason: "the archived checkout is no longer present for content revalidation; preserved the session branch and archive".into(),
            });
        }
        if retained_checkout_present {
            self.refuse_live_checkout(session_id, &worktree_path)?;
            if let Some(archive_guard) = archive_guard {
                let archived_plan = archive_guard.plan();
                let archive_path = archive_guard.archive_path();
                let _archive_deadline = crate::git::limit_git_until(status_limits.deadline);
                let current = if archive_guard.is_retirement() {
                    self.cleanup_resolve_retirement_revalidation_plan(
                        session_id,
                        archive_guard.permits_adopted(),
                    )?
                } else {
                    self.cleanup_resolve_revalidation_plan(session_id)?
                };
                if current.digest != archived_plan.digest
                    || current.target_origin != session.origin
                    || archived_plan.target_origin != session.origin
                    || current.checkout_branch_ref != archived_plan.checkout_branch_ref
                    || current.checkout_branch_tip != archived_plan.checkout_branch_tip
                {
                    return Err(BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: "worktree contents or ignored cleanup policy changed after archive verification; the archive was kept and the worktree remains in place; review a new resolve plan before retrying".into(),
                    });
                }
                // A bundle omits commits reachable from its reviewed delivery
                // targets. Each prerequisite must remain rooted in current
                // delivery history; local object existence alone is insufficient.
                {
                    let _deadline = crate::git::limit_git_until(status_limits.deadline);
                    for prerequisite in &archived_plan.delivery_targets {
                        let mut retained = false;
                        let mut unavailable = None;
                        for target in &current.delivery_targets {
                            match self.repo_handle().is_ancestor_checked(prerequisite, target) {
                                Ok(true) => {
                                    retained = true;
                                    break;
                                }
                                Ok(false) => {}
                                Err(error) => unavailable = Some(error.to_string()),
                            }
                        }
                        if !retained {
                            return Err(BrokerOpError::CleanupResolveRefused {
                                id: session_id,
                                reason: match unavailable {
                                    Some(error) => format!("archive prerequisite reachability could not be verified; the archive and checkout were preserved: {error}"),
                                    None => "archive prerequisites are no longer retained by current delivery history; the archive and checkout were preserved".into(),
                                },
                            });
                        }
                    }
                }
                crate::cleanup_resolve::verify_recovery_archive_for_plan(
                    self.repo_handle(),
                    archive_path,
                    archived_plan,
                )
                .map_err(|reason| BrokerOpError::CleanupResolveRefused {
                    id: session_id,
                    reason: format!(
                        "the verified archive changed before worktree removal; the worktree remains in place: {reason}"
                    ),
                })?;
            }
            if let Some(archive_guard) = archive_guard
                && let Some(expected_cohort) = archive_guard.expected_cohort()
            {
                let archived_worktree = Path::new(&archive_guard.plan().worktree_path);
                if archived_worktree != worktree_path {
                    return Err(BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: "the cleanup cohort path no longer matches the archived checkout"
                            .into(),
                    });
                }
                let current_cohort = self.closed_artifact_checkout_cohort_for_session(
                    expected_cohort.owner_session_id(),
                    archived_worktree,
                    Some(status_limits.deadline),
                )?;
                if current_cohort.as_ref() != Some(expected_cohort) {
                    return Err(BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: "the closed checkout cohort changed after archive verification; the archive and checkout were preserved".into(),
                    });
                }
                if expected_cohort.owner_session_id() != session_id {
                    return Err(BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: "the archived session is no longer the owner of its closed checkout cohort; the archive and checkout were preserved".into(),
                    });
                }
                if !self.cleanup_cohort_filesystem_aliases_match(
                    expected_cohort.alias_refresh_generation(),
                    Some(status_limits.deadline),
                )? {
                    return Err(BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: "the registered checkout alias inventory changed or could not be rechecked after archive verification; the archive and checkout were preserved".into(),
                    });
                }
                let cleanup_policy = crate::AutoCleanupPolicy::load(self.main_root()).map_err(
                    |reason| BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: format!(
                            "automatic checkout cleanup policy could not be rechecked; the archive and checkout were preserved: {reason}"
                        ),
                    },
                )?;
                if !cleanup_policy.auto_remove || cleanup_policy.is_pinned(archived_worktree) {
                    return Err(BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: if cleanup_policy.auto_remove {
                            "the checkout is now pinned by cleanup policy; the archive and checkout were preserved".into()
                        } else {
                            "automatic checkout removal is now disabled; the archive and checkout were preserved".into()
                        },
                    });
                }
                let open_file_snapshot = crate::reclaim::open_file_snapshot_under(Path::new("/"));
                if let Some(reason) = crate::reclaim::open_file_snapshot_blocker_for_worktree(
                    &open_file_snapshot,
                    archived_worktree,
                ) {
                    return Err(BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: format!(
                            "the checkout has active or unverified process use; the archive and checkout were preserved: {reason}"
                        ),
                    });
                }
                removed_checkout_session_ids = expected_cohort.session_ids();
            }
            if self.is_orphaned_worktree_of_this_repository_checked(&worktree_path)? {
                if archive_guard.is_some_and(|guard| guard.preserves_branch(session)) {
                    return Err(BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: "the archived checkout became an orphan; preserving its registered branch and checkout for recovery".into(),
                    });
                }
                removed_orphan_branch = Some(self.remove_proven_orphan_worktree(
                    session,
                    item.as_ref(),
                    &worktree_path,
                    force,
                )?);
            } else {
                self.repo
                    .worktree_remove_registered(&worktree_path, force)?;
            }
        }
        require_worktree_path_absent(&worktree_path, session_id)?;
        if let Some((branch_ref, branch_tip)) = removed_orphan_branch.as_ref() {
            self.refuse_live_checkout(session_id, &worktree_path)?;
            self.ensure_orphan_branch_not_checked_out(session_id, branch_ref, branch_tip, None)?;
        }
        if let Some(branch_tip) = item.and_then(|item| item.branch_tip)
            && session.origin == SessionOrigin::Spawned
            && !archive_guard.is_some_and(|guard| guard.preserves_branch(session))
        {
            self.repo
                .delete_branch_ref_checked(&session.branch, &branch_tip)?;
        }
        self.mark_removed_checkout_sessions_cleaned(&removed_checkout_session_ids, session_id)?;
        Ok(())
    }

    /// Record a checkout removal only after the physical checkout was proven
    /// absent. Retirement callers pass the complete closed cohort; other
    /// callers settle only the row they just removed. Raw alias symlinks may
    /// remain dangling, and additional cohort branch refs are left intact.
    fn mark_removed_checkout_sessions_cleaned(
        &mut self,
        session_ids: &[i64],
        owner_id: i64,
    ) -> Result<(), BrokerOpError> {
        let mut ids = session_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        for id in &ids {
            let current = self.store.session(*id)?;
            if current.status == SessionStatus::Cleaned
                && current.cleanup_state == crate::SessionCleanupState::Cleaned
            {
                continue;
            }
            if !current.status.is_closed()
                || current.cleanup_state != crate::SessionCleanupState::Closed
            {
                return Err(BrokerOpError::DirtyWorktree {
                    id: owner_id,
                    reason: format!(
                        "closed checkout cohort member {id} changed before cleanup could be recorded"
                    ),
                });
            }
            self.store
                .set_session_status(*id, SessionStatus::Cleaned, None)?;
        }
        self.store
            .complete_checkout_cleanup_requests_after_removal(
                &ids,
                crate::collaboration_gc::now_ms(),
            )?;
        Ok(())
    }

    // Reuse one ownership observation across a full plan instead of opening
    // the session table and canonicalizing every live path per closed row.
    // Apply takes a fresh observation under the lifecycle claim.
    pub(crate) fn cleanup_live_checkout_snapshot(
        &self,
        deadline: Option<std::time::Instant>,
    ) -> Result<Vec<(i64, crate::auto_cleanup::ResolvedWorktreePath)>, BrokerOpError> {
        self.cleanup_live_checkout_snapshot_until(deadline)?
            .ok_or_else(|| BrokerOpError::RepresentationUnavailable {
                reason: "live checkout ownership observation exceeded its deadline".into(),
            })
    }

    fn cleanup_live_checkout_snapshot_until(
        &self,
        deadline: Option<std::time::Instant>,
    ) -> Result<Option<Vec<(i64, crate::auto_cleanup::ResolvedWorktreePath)>>, BrokerOpError> {
        let deadline = CleanupStatusLimits::new(
            deadline,
            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
        )
        .deadline;
        let sessions = self.store.live_sessions()?;
        let Some(paths) =
            crate::auto_cleanup::resolve_live_worktree_paths(&sessions, Some(deadline))
        else {
            return Ok(None);
        };
        Ok(Some(
            sessions
                .into_iter()
                .map(|session| session.id)
                .zip(paths)
                .collect(),
        ))
    }

    /// Refuse to remove a checkout any other live session works in.
    ///
    /// A closed session's directory can be adopted again (`start --adopt`,
    /// `--replace-stale`), leaving the closed row naming a live checkout.
    /// Its cleanliness and provenance then describe the live session's work,
    /// so no proof about the closed row authorizes removing it -- and no
    /// `--force` does either. Checked immediately before removal, because
    /// adoption takes no GC lock; paths are compared canonically, and a live
    /// checkout nested inside the directory or containing it counts too.
    pub(crate) fn refuse_live_checkout(
        &self,
        session_id: i64,
        worktree: &Path,
    ) -> Result<(), BrokerOpError> {
        self.refuse_live_checkout_with_deadline(session_id, worktree, self.landing_deadline.get())
    }

    /// Refuse a live checkout using the caller's remaining maintenance budget.
    ///
    /// Artifact reclamation calls this while holding its GC and artifact-use
    /// claims, so a live-path observation cannot extend an inline sweep past
    /// its smaller deadline.
    pub(crate) fn refuse_live_checkout_until(
        &self,
        session_id: i64,
        worktree: &Path,
        deadline: std::time::Instant,
    ) -> Result<(), BrokerOpError> {
        self.refuse_live_checkout_with_deadline(session_id, worktree, Some(deadline))
    }

    fn refuse_live_checkout_with_deadline(
        &self,
        session_id: i64,
        worktree: &Path,
        deadline: Option<std::time::Instant>,
    ) -> Result<(), BrokerOpError> {
        let live_checkouts = self.cleanup_live_checkout_snapshot(deadline)?;
        let live_owner =
            overlapping_live_checkout(session_id, worktree, &live_checkouts).map_err(|reason| {
                BrokerOpError::DirtyWorktree {
                    id: session_id,
                    reason,
                }
            })?;
        if let Some(live_id) = live_owner {
            return Err(BrokerOpError::WorktreeInUseByLiveSession {
                id: session_id,
                live_id,
                path: worktree.to_string_lossy().into_owned(),
            });
        }
        Ok(())
    }
}

fn overlapping_live_checkout(
    session_id: i64,
    worktree: &Path,
    live_checkouts: &[(i64, crate::auto_cleanup::ResolvedWorktreePath)],
) -> Result<Option<i64>, String> {
    use crate::auto_cleanup::{LiveOverlapEvidence, ResolvedWorktreePath};

    let candidate = ResolvedWorktreePath::resolve(worktree);
    for (live_id, path) in live_checkouts {
        if *live_id == session_id {
            continue;
        }
        match crate::auto_cleanup::live_worktree_overlap(&candidate, path) {
            LiveOverlapEvidence::Disjoint => {}
            LiveOverlapEvidence::Overlap => return Ok(Some(*live_id)),
            LiveOverlapEvidence::Unknown => {
                return Err(crate::auto_cleanup::overlap_resolution_error(
                    &candidate, path,
                ));
            }
        }
    }
    Ok(None)
}

fn worktree_path_is_real_directory(path: &Path, session_id: i64) -> Result<bool, BrokerOpError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(BrokerOpError::DirtyWorktree {
                id: session_id,
                reason: format!(
                    "worktree path {} is not a real directory; preserved the path and session, and did not delete the branch",
                    path.display()
                ),
            })
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(BrokerError::Io {
            path: path.to_path_buf(),
            source,
        }
        .into()),
    }
}

fn require_worktree_path_absent(path: &Path, session_id: i64) -> Result<(), BrokerOpError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(BrokerOpError::DirtyWorktree {
            id: session_id,
            reason: format!(
                "Git reported worktree removal but {} still exists; preserved the path, branch, and session for inspection",
                path.display()
            ),
        }),
        Err(source) => Err(BrokerError::Io {
            path: path.to_path_buf(),
            source,
        }
        .into()),
    }
}

pub(super) fn ensure_lifecycle_session_unchanged(
    session_id: i64,
    before: &Session,
    after: &Session,
) -> Result<(), BrokerOpError> {
    if before.worktree_path != after.worktree_path
        || before.branch != after.branch
        || before.origin != after.origin
        || before.status != after.status
        || before.cleanup_state != after.cleanup_state
        || before.closed_at != after.closed_at
        || before.task != after.task
        || before.diff_base != after.diff_base
        || before.adoption_base != after.adoption_base
        || before.adopted_head != after.adopted_head
        || before.pid != after.pid
        || before.command != after.command
        || before.log_path != after.log_path
        || before.agent_identity != after.agent_identity
        || before.updated_at != after.updated_at
        || before.last_activity_at != after.last_activity_at
    {
        return Err(BrokerOpError::DirtyWorktree {
            id: session_id,
            reason: "the session owner, activity, or cleanup state changed while waiting for the worktree lifecycle claim".into(),
        });
    }
    Ok(())
}

fn size_warm_candidates(plan: &CleanupPlan) -> Vec<String> {
    plan.worktrees
        .iter()
        .filter(|item| item.worktree_present && item.disposition != CleanupDisposition::UnsafePath)
        .map(|item| item.worktree_path.clone())
        .collect()
}

#[cfg(test)]
mod cleanup_size_ownership_tests {
    use super::{size_warm_candidates, worktree_size_estimate};
    use crate::measurement::{SizeRecords, SizeScan};
    use crate::{CleanupDisposition, CleanupPlan, CleanupWorktreePlan, SessionOrigin};

    fn worktree_plan(
        path: &str,
        present: bool,
        disposition: CleanupDisposition,
    ) -> CleanupWorktreePlan {
        CleanupWorktreePlan {
            session_id: 1,
            worktree_path: path.to_owned(),
            worktree_present: present,
            branch_ref: "refs/heads/test".to_owned(),
            branch_tip: None,
            delete_branch: false,
            origin: SessionOrigin::Spawned,
            disposition,
            provenance: None,
            estimated_bytes: None,
            estimated_inodes: None,
            reason: String::new(),
            inspection_commands: Vec::new(),
            force_cleanup_command: String::new(),
        }
    }

    #[test]
    fn warm_candidates_skip_unsafe_and_absent_worktrees_but_keep_owned_dirty_entries() {
        let plan = CleanupPlan {
            worktrees: vec![
                worktree_plan("/unsafe", true, CleanupDisposition::UnsafePath),
                worktree_plan("/owned-dirty", true, CleanupDisposition::Dirty),
                worktree_plan(
                    "/owned-unproven",
                    true,
                    CleanupDisposition::UnprovenProvenance,
                ),
                worktree_plan("/absent", false, CleanupDisposition::Eligible),
            ],
            ..CleanupPlan::default()
        };

        assert_eq!(
            size_warm_candidates(&plan),
            vec!["/owned-dirty", "/owned-unproven"]
        );
    }

    #[test]
    fn unowned_replacement_is_not_measured_or_reported_from_cache() {
        let temp = tempfile::tempdir().unwrap();
        let broker_root = temp.path().join("broker-worktrees");
        let replacement = temp.path().join("replaced-session-path");
        std::fs::create_dir_all(&broker_root).unwrap();
        std::fs::create_dir_all(&replacement).unwrap();
        assert!(!replacement.starts_with(&broker_root));

        let key = replacement.to_string_lossy().into_owned();
        let mut records = SizeRecords::default();
        records.record_usage(&key, 4096, Some(3), 1);
        let mut measurement_calls = 0;
        let measured = worktree_size_estimate(
            &replacement,
            &key,
            true,
            false,
            SizeScan::Measure,
            &mut records,
            |_| {
                measurement_calls += 1;
                panic!("an unowned path must not be traversed for size")
            },
        );
        assert_eq!(measured, (None, None));
        assert_eq!(measurement_calls, 0);

        let recorded = worktree_size_estimate(
            &replacement,
            &key,
            true,
            false,
            SizeScan::Recorded,
            &mut records,
            |_| {
                measurement_calls += 1;
                None
            },
        );
        assert_eq!(recorded, (None, None));
        assert_eq!(measurement_calls, 0);
    }

    #[test]
    fn owned_existing_path_is_measured_once_and_recorded() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owned-worktree");
        std::fs::create_dir(&path).unwrap();
        let key = path.to_string_lossy().into_owned();
        let mut records = SizeRecords::default();
        let mut measurement_calls = 0;

        let estimate = worktree_size_estimate(
            &path,
            &key,
            true,
            true,
            SizeScan::Measure,
            &mut records,
            |measured_path| {
                measurement_calls += 1;
                assert_eq!(measured_path, path.as_path());
                Some(crate::disk_headroom::DirectoryUsage {
                    bytes: 8192,
                    inodes: 5,
                })
            },
        );

        assert_eq!(estimate, (Some(8192), Some(5)));
        assert_eq!(measurement_calls, 1);
        assert_eq!(
            records
                .get(&key)
                .map(|record| (record.bytes, record.inodes)),
            Some((8192, Some(5)))
        );
    }

    #[test]
    fn absent_path_is_zero_without_measuring() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("absent-worktree");
        let key = path.to_string_lossy().into_owned();
        let mut records = SizeRecords::default();
        let mut measurement_calls = 0;

        let estimate = worktree_size_estimate(
            &path,
            &key,
            false,
            false,
            SizeScan::Measure,
            &mut records,
            |_| {
                measurement_calls += 1;
                panic!("an absent path must not be measured")
            },
        );

        assert_eq!(estimate, (Some(0), Some(0)));
        assert_eq!(measurement_calls, 0);
        assert_eq!(records.get(&key), None);
    }
}

#[cfg(test)]
mod ownership_witness_tests {
    use super::{WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES, read_bounded_ownership_witness};
    #[cfg(unix)]
    use std::time::Duration;

    #[test]
    fn ownership_witness_reads_are_size_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("marker");
        std::fs::write(&path, b"small marker").unwrap();
        assert_eq!(
            read_bounded_ownership_witness(&path).unwrap().as_deref(),
            Some(&b"small marker"[..])
        );

        std::fs::write(
            &path,
            vec![b'x'; WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES as usize + 1],
        )
        .unwrap();
        assert!(read_bounded_ownership_witness(&path).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn ownership_witness_open_rejects_symlinks_and_does_not_block_on_fifos() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let linked = temp.path().join("linked-marker");
        std::fs::write(&target, b"target marker").unwrap();
        symlink(&target, &linked).unwrap();
        assert!(read_bounded_ownership_witness(&linked).unwrap().is_none());

        let fifo = temp.path().join("fifo-marker");
        let output = std::process::Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .output()
            .unwrap();
        assert!(output.status.success());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            assert!(sender.send(read_bounded_ownership_witness(&fifo)).is_ok());
        });
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(Ok(None))
        ));
    }
}

#[cfg(test)]
mod registered_removal_failure_tests {
    use super::CleanupStatusLimits;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    use super::{
        MAX_ORPHAN_RESIDUE_ENUMERATED_ENTRIES, MAX_ORPHAN_RESIDUE_ENUMERATED_NAME_BYTES,
        ORPHAN_RESIDUE_SCAN_BUDGET, OrphanRemovePass, OrphanResidueBudget, PinnedOrphanRoot,
        collect_orphan_residue, open_directory_at, remove_orphan_residue_pass,
        tracked_tree_contains_directory,
    };
    use crate::{SessionCleanupState, SessionOrigin, SessionStatus, git::with_git_deadline};
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    use std::ffi::CString;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    use std::os::fd::AsRawFd;
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    fn initialized_repository(root: &Path) -> std::path::PathBuf {
        let repository = root.join("repository");
        std::fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        git(&repository, &["config", "user.name", "Cleanup test"]);
        git(
            &repository,
            &["config", "user.email", "cleanup@example.invalid"],
        );
        git(&repository, &["commit", "--allow-empty", "-qm", "initial"]);
        repository
    }

    fn git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn remove_worktree_admin_directory(worktree: &Path) {
        let pointer = std::fs::read_to_string(worktree.join(".git")).unwrap();
        let gitdir = pointer
            .trim()
            .strip_prefix("gitdir:")
            .expect("linked worktree has a gitdir pointer")
            .trim();
        let gitdir = Path::new(gitdir);
        let gitdir = if gitdir.is_absolute() {
            gitdir.to_path_buf()
        } else {
            worktree.join(gitdir)
        };
        std::fs::remove_dir_all(gitdir).unwrap();
    }

    #[test]
    fn force_cleanup_refuses_a_nonterminal_session_without_changing_its_checkout() {
        let owner = tempfile::tempdir().unwrap();
        let repository = initialized_repository(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker.start_worktree("continuing work", None).unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        let evidence = worktree.join("must-survive.txt");
        std::fs::write(&evidence, b"still in use").unwrap();
        let branch_ref = format!("refs/heads/{}", session.branch);

        let error = broker.cleanup(session.id, true).unwrap_err();

        assert!(matches!(
            error,
            crate::BrokerOpError::CleanupSessionNotTerminal { id, .. } if id == session.id
        ));
        assert!(worktree.is_dir());
        assert_eq!(std::fs::read(evidence).unwrap(), b"still in use");
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Open
        );
        assert!(broker.repo.resolve_ref(&branch_ref).is_some());
    }

    #[test]
    fn cleaned_session_does_not_authorize_removing_a_recreated_path() {
        let owner = tempfile::tempdir().unwrap();
        let repository = initialized_repository(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("already-cleaned path reuse", None)
            .unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        broker.close(session.id).unwrap();

        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(closed.status, SessionStatus::Cleaned);
        assert_eq!(closed.cleanup_state, SessionCleanupState::Closed);

        broker.cleanup(session.id, false).unwrap();
        broker.cleanup(session.id, false).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let replacement = worktree.join("new-owner-data.txt");
        std::fs::write(&replacement, b"not this session's checkout").unwrap();

        let error = broker.cleanup(session.id, true).unwrap_err();

        assert!(matches!(
            error,
            crate::BrokerOpError::CleanupSessionNotTerminal { id, state }
                if id == session.id && state.contains("recreated")
        ));
        assert_eq!(
            std::fs::read(replacement).unwrap(),
            b"not this session's checkout"
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Cleaned
        );
    }

    #[test]
    fn adopted_archive_path_requires_an_exact_nonprimary_linked_worktree() {
        let owner = tempfile::tempdir().unwrap();
        let repository = initialized_repository(owner.path());
        let linked = owner.path().join("adopted-linked-worktree");
        let linked_arg = linked.to_string_lossy().into_owned();
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "adopted-cleanup-fixture",
                &linked_arg,
            ],
        );
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let adopted = broker.adopt(&linked, Some("exact adopted path")).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);

        assert_eq!(adopted.origin, SessionOrigin::Adopted);
        assert!(
            broker
                .registered_adopted_worktree_matches(
                    &adopted,
                    Path::new(&adopted.worktree_path),
                    deadline,
                )
                .unwrap()
        );

        let mut primary_target = adopted.clone();
        primary_target.worktree_path = repository.to_string_lossy().into_owned();
        assert!(
            !broker
                .registered_adopted_worktree_matches(&primary_target, &repository, deadline,)
                .unwrap()
        );

        let foreign_parent = owner.path().join("foreign");
        let foreign_repository = initialized_repository(&foreign_parent);
        let mut foreign_target = adopted.clone();
        foreign_target.worktree_path = foreign_repository.to_string_lossy().into_owned();
        assert!(!broker
            .registered_adopted_worktree_matches(
                &foreign_target,
                &foreign_repository,
                deadline,
            )
            .unwrap());

        #[cfg(unix)]
        {
            let alias = owner.path().join("adopted-path-alias");
            std::os::unix::fs::symlink(&linked, &alias).unwrap();
            let mut symlink_target = adopted.clone();
            symlink_target.worktree_path = alias.to_string_lossy().into_owned();
            assert!(
                !broker
                    .registered_adopted_worktree_matches(&symlink_target, &alias, deadline)
                    .unwrap()
            );
        }
    }

    fn repository_with_ignored_cleanup_fixture(root: &Path) -> std::path::PathBuf {
        let repository = initialized_repository(root);
        std::fs::write(
            repository.join(".gitignore"),
            "/.aethyme/\n/target/\n/local-dataset/\n",
        )
        .unwrap();
        std::fs::write(repository.join("README.md"), "tracked fixture\n").unwrap();
        git(&repository, &["add", ".gitignore", "README.md"]);
        git(&repository, &["commit", "-qm", "cleanup fixture"]);
        std::fs::create_dir_all(repository.join(".aethyme")).unwrap();
        std::fs::write(
            repository.join(".aethyme/config.toml"),
            "[cleanup]\nstrict = false\n",
        )
        .unwrap();
        repository
    }

    #[test]
    fn unavailable_landing_proof_does_not_mask_checkout_inspection_failure() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("unavailable proof preflight", None)
            .unwrap();
        let worktree = Path::new(&session.worktree_path);
        let limits = || {
            CleanupStatusLimits::new(
                None,
                crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
                crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            )
        };
        let (disposition, reason, provenance) = broker
            .cleanup_eligibility_with_provenance(worktree, limits(), |_| {
                Err(crate::BrokerOpError::RepresentationUnavailable {
                    reason: "deterministic exhausted landing budget".into(),
                })
            })
            .unwrap();
        assert_eq!(disposition, crate::CleanupDisposition::UnprovenProvenance);
        assert!(reason.contains("deterministic exhausted landing budget"));
        assert!(provenance.is_none());
        assert!(worktree.exists());

        let error = broker.cleanup_eligibility_with_provenance(worktree, limits(), |_| {
            Err(crate::BrokerOpError::CleanupResolveRefused {
                id: session.id,
                reason: "non-representation failure".into(),
            })
        });
        assert!(matches!(
            error,
            Err(crate::BrokerOpError::CleanupResolveRefused { .. })
        ));

        std::fs::write(worktree.join("retained-untracked.txt"), "unique work").unwrap();
        let (disposition, _, _) = broker
            .cleanup_eligibility_with_provenance(worktree, limits(), |_| {
                panic!("dirty preflight must precede landing inspection")
            })
            .unwrap();
        assert_eq!(disposition, crate::CleanupDisposition::Dirty);

        let (disposition, _, _) = broker
            .cleanup_eligibility_with_provenance(
                worktree,
                CleanupStatusLimits::new(None, 1, 1),
                |_| panic!("incomplete status must not become unavailable landing proof"),
            )
            .unwrap();
        assert_eq!(disposition, crate::CleanupDisposition::InspectionFailed);
        assert_eq!(
            std::fs::read_to_string(worktree.join("retained-untracked.txt")).unwrap(),
            "unique work"
        );
    }

    fn start_represented_cleanup_session(
        broker: &mut crate::Broker,
        label: &str,
    ) -> (crate::Session, std::path::PathBuf) {
        let session = broker.start_worktree(label, None).unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        std::fs::write(worktree.join("session-change.txt"), "represented\n").unwrap();
        git(&worktree, &["add", "session-change.txt"]);
        git(&worktree, &["commit", "-qm", "represented cleanup head"]);
        assert!(
            broker.submit(session.id).unwrap().promoted,
            "recovery fixture must represent the session head before cleanup starts"
        );
        (session, worktree)
    }

    fn persist_finish_cleanup_intent(
        broker: &mut crate::Broker,
        session: &crate::Session,
        worktree: &Path,
    ) -> crate::events::FinishCleanupRecoveryProof {
        persist_finish_cleanup_intent_with_schema(broker, session, worktree, 3)
    }

    fn persist_finish_cleanup_intent_with_schema(
        broker: &mut crate::Broker,
        session: &crate::Session,
        worktree: &Path,
        schema_version: u8,
    ) -> crate::events::FinishCleanupRecoveryProof {
        let branch_ref = format!("refs/heads/{}", session.branch);
        let checkout = crate::git::GitRepo::discover(worktree).unwrap();
        let mut proof = checkout
            .finish_cleanup_recovery_proof(&branch_ref)
            .expect("the represented linked checkout must yield a durable recovery proof");
        proof.schema_version = schema_version;
        if schema_version < 3 {
            proof.root_generation_sha256 = None;
        }
        if schema_version >= 2 {
            let policy = crate::AutoCleanupPolicy::load(broker.main_root()).unwrap();
            proof.disposable_ignored_root_prefixes =
                crate::auto_cleanup::disposable_ignored_directory_prefixes(worktree, &policy)
                    .unwrap();
        }
        let payload = crate::events::session_finish_cleanup_started_payload_with_proof(
            true,
            true,
            Some(&proof),
        );
        broker
            .store()
            .begin_finish_cleanup(session.id, &payload)
            .unwrap();
        proof
    }

    fn advance_schema_floor(database: &Path) {
        let mut conn = rusqlite::Connection::open(database).unwrap();
        let tx = conn.transaction().unwrap();
        let newer = crate::SCHEMA_VERSION + 1;
        for key in ["schema_version", "min_compatible_schema"] {
            assert_eq!(
                tx.execute(
                    "UPDATE meta SET value=?2 WHERE key=?1",
                    rusqlite::params![key, newer.to_string()]
                )
                .unwrap(),
                1
            );
        }
        tx.commit().unwrap();
    }

    fn assert_new_schema_refusal(error: &crate::BrokerOpError) {
        assert!(
            matches!(error, crate::BrokerOpError::Store(crate::BrokerError::SchemaTooNew { found, supported })
            if *found == crate::SCHEMA_VERSION + 1 && *supported == crate::SCHEMA_VERSION),
            "{error:?}"
        );
    }

    #[test]
    fn incomplete_bounded_status_keeps_a_closed_cleanup_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = repository_with_ignored_cleanup_fixture(tmp.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "bounded status refusal");
        broker.close(session.id).unwrap();

        let first = worktree.join("untracked-one.txt");
        let second = worktree.join("untracked-two.txt");
        std::fs::write(&first, "keep one\n").unwrap();
        std::fs::write(&second, "keep two\n").unwrap();
        let closed_session = broker.store().session(session.id).unwrap();
        let _claim = broker.claim_worktree_lifecycle().unwrap();

        let error = broker
            .cleanup_under_lifecycle_claim_with_status_limits(
                &closed_session,
                false,
                None,
                CleanupStatusLimits::new(None, 1024 * 1024, 1),
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("cleanup inspection failed"),
            "an incomplete status inventory must become an explicit cleanup refusal: {error}"
        );
        assert!(
            worktree.is_dir(),
            "incomplete inspection removed the checkout"
        );
        assert_eq!(std::fs::read(first).unwrap(), b"keep one\n");
        assert_eq!(std::fs::read(second).unwrap(), b"keep two\n");
    }

    #[test]
    fn incomplete_status_refuses_interrupted_finish_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = repository_with_ignored_cleanup_fixture(tmp.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "bounded finish recovery status");
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        let closed = broker.store().session(session.id).unwrap();
        let limits = CleanupStatusLimits::new(None, 1024 * 1024, 0);

        assert!(
            broker
                .interrupted_finish_cleanup_proof_with_status_limits(&closed, limits)
                .is_none(),
            "an incomplete --ignored=matching status inventory must not authorize recovery"
        );

        let _claim = broker.claim_worktree_lifecycle().unwrap();
        let error = broker
            .cleanup_under_lifecycle_claim_with_status_limits(
                &closed,
                false,
                None,
                CleanupStatusLimits::new(
                    Some(std::time::Instant::now() - std::time::Duration::from_millis(1)),
                    1024 * 1024,
                    crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
                ),
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("cleanup inspection failed"),
            "bounded status failure must become an explicit cleanup refusal: {error}"
        );
        assert!(worktree.is_dir(), "incomplete proof removed the worktree");
        assert!(worktree.join("session-change.txt").is_file());
        assert!(!worktree.join("README.md").exists());
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(
            broker.repo.resolve_ref(&proof.branch_ref).is_some(),
            "incomplete status proof must retain the represented branch"
        );
    }

    #[test]
    fn lifecycle_claim_refuses_a_floor_raised_after_controller_open() {
        let tmp = tempfile::tempdir().unwrap();
        let root = initialized_repository(tmp.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let database = broker.store().db_path().to_path_buf();
        let reached_claim = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = std::sync::Arc::clone(&reached_claim);
        broker.worktree_lifecycle_test_observer = Some(std::sync::Arc::new(move |_| {
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        advance_schema_floor(&database);
        let error = broker.claim_worktree_lifecycle().unwrap_err();
        assert_new_schema_refusal(&error);
        assert!(!reached_claim.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!storage.worktree_root().exists());
    }

    #[test]
    fn start_rechecks_the_floor_after_waiting_for_the_lifecycle_claim() {
        let tmp = tempfile::tempdir().unwrap();
        let root = initialized_repository(tmp.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let database = broker.store().db_path().to_path_buf();
        let claim = crate::file_lock::acquire_repository_lifecycle_lock(&root).unwrap();
        let held = std::sync::Arc::new(std::sync::Mutex::new(Some(claim)));
        let held_at_boundary = std::sync::Arc::clone(&held);
        let reached = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reached_at_boundary = std::sync::Arc::clone(&reached);
        broker.worktree_lifecycle_test_observer = Some(std::sync::Arc::new(move |_| {
            reached_at_boundary.store(true, std::sync::atomic::Ordering::SeqCst);
            advance_schema_floor(&database);
            drop(held_at_boundary.lock().unwrap().take());
        }));
        let before = git_head(&root);
        let error = broker
            .start_worktree("must not allocate", None)
            .unwrap_err();
        assert_new_schema_refusal(&error);
        assert!(reached.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            !storage.worktree_root().exists(),
            "refusal must precede root preparation"
        );
        assert!(broker.store().live_sessions().unwrap().is_empty());
        assert_eq!(git_head(&root), before);
        // The failed post-acquisition check must release the claim.
        drop(crate::file_lock::acquire_repository_lifecycle_lock(&root).unwrap());
    }

    #[test]
    fn stale_controller_force_cleanup_preserves_registered_and_orphan_checkouts() {
        for orphan in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let root = initialized_repository(tmp.path());
            let storage = crate::test_storage::TestStorage::new();
            let mut broker = storage.open(&root).unwrap();
            let session = broker
                .start_worktree("preserve while incompatible", None)
                .unwrap();
            broker.close(session.id).unwrap();
            let path = Path::new(&session.worktree_path);
            let evidence = path.join("service-data.txt");
            std::fs::write(&evidence, b"must survive").unwrap();
            if orphan {
                remove_worktree_admin_directory(path);
            }
            let database = broker.store().db_path().to_path_buf();
            let before = broker.store().session(session.id).unwrap();
            advance_schema_floor(&database);
            let error = broker.cleanup(session.id, true).unwrap_err();
            assert_new_schema_refusal(&error);
            assert_eq!(std::fs::read(&evidence).unwrap(), b"must survive");
            assert_eq!(
                broker.store().session(session.id).unwrap().cleanup_state,
                before.cleanup_state
            );
            let branch = format!("refs/heads/{}", session.branch);
            assert!(broker.repo.resolve_ref(&branch).is_some());
        }
    }

    fn git_head(root: &Path) -> Vec<u8> {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command
            .current_dir(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        output.stdout
    }

    #[test]
    fn non_force_orphan_cleanup_without_finish_intent_preserves_user_data() {
        for remove_pointer in [false, true] {
            let owner = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
            let repository = initialized_repository(owner.path());
            let storage = crate::test_storage::TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let session = broker
                .start_worktree("manual orphan without finish intent", None)
                .unwrap();
            let worktree = Path::new(&session.worktree_path).to_path_buf();
            broker.close(session.id).unwrap();
            let evidence = worktree.join("unique-user-data.txt");
            std::fs::write(&evidence, b"keep this unreviewed data").unwrap();
            remove_worktree_admin_directory(&worktree);
            if remove_pointer {
                std::fs::remove_file(worktree.join(".git")).unwrap();
            }

            let result = broker.cleanup(session.id, false);

            assert!(
                result.is_err(),
                "a manual orphan without finish intent must remain inspection-only"
            );
            assert_eq!(
                std::fs::read(&evidence).unwrap(),
                b"keep this unreviewed data",
                "manual admin loss must not authorize discarding user data"
            );
            assert_eq!(
                broker.store().session(session.id).unwrap().cleanup_state,
                SessionCleanupState::Closed
            );
            assert!(
                crate::git::GitRepo::discover(&repository)
                    .unwrap()
                    .resolve_ref(&format!("refs/heads/{}", session.branch))
                    .is_some(),
                "a proofless orphan must retain its branch"
            );
        }
    }

    #[test]
    fn explicit_force_can_remove_a_positively_identified_same_repository_orphan() {
        let owner = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let repository = initialized_repository(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("explicitly forced same repository orphan", None)
            .unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        broker.close(session.id).unwrap();
        let evidence = worktree.join("discarded-by-explicit-force.txt");
        std::fs::write(&evidence, b"explicit force is a discard request").unwrap();
        remove_worktree_admin_directory(&worktree);

        broker.cleanup(session.id, true).unwrap();

        assert!(!worktree.exists());
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Cleaned
        );
        assert!(
            crate::git::GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref(&format!("refs/heads/{}", session.branch))
                .is_none(),
            "explicit force should remove the owned session branch"
        );
    }

    #[test]
    fn registered_finish_recovery_obeys_strict_policy_flip_and_manual_residue_removal() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "registered recovery strict flip");

        let cache = worktree.join("target");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
        std::fs::write(cache.join("payload"), b"known ignored output").unwrap();
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(
            broker.interrupted_finish_cleanup_proof(&closed),
            Some(proof.clone()),
            "the represented interrupted finish must be recoverable before policy changes"
        );
        assert!(broker.store().live_sessions().unwrap().is_empty());

        std::fs::write(
            repository.join(".aethyme/config.toml"),
            "[cleanup]\nstrict = true\n",
        )
        .unwrap();
        let error = broker.cleanup(session.id, false).unwrap_err();
        assert!(
            worktree.exists(),
            "strict policy unexpectedly removed the checkout with ignored residue: {error}"
        );
        assert!(cache.join("payload").is_file());
        assert!(worktree.join("session-change.txt").is_file());
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());

        // Strict mode permits finish recovery once the operator has removed
        // every ignored leftover by hand.
        std::fs::remove_dir_all(&cache).unwrap();
        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(
            broker.interrupted_finish_cleanup_proof(&closed),
            Some(proof.clone())
        );
        broker.cleanup(session.id, false).unwrap();
        assert!(!worktree.exists());
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_none());
    }

    #[test]
    fn orphan_finish_recovery_requires_strict_policy_to_be_disabled() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan recovery strict flip");

        let residue = worktree.join("local-dataset/saved-session.json");
        std::fs::create_dir_all(residue.parent().unwrap()).unwrap();
        std::fs::write(&residue, b"saved session data").unwrap();
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        remove_worktree_admin_directory(&worktree);
        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(
            broker.interrupted_finish_orphan_cleanup_proof(&closed),
            Some(proof.clone()),
            "the same-repository orphan must have a durable recovery proof before policy changes"
        );
        assert!(broker.store().live_sessions().unwrap().is_empty());

        std::fs::write(
            repository.join(".aethyme/config.toml"),
            "[cleanup]\nstrict = true\n",
        )
        .unwrap();
        assert_eq!(
            broker.interrupted_finish_orphan_cleanup_proof(&closed),
            Some(proof.clone()),
            "the policy flip must not alter the identity proof"
        );
        let error = broker.cleanup(session.id, false).unwrap_err();
        assert!(
            error.to_string().contains("strict cleanup"),
            "strict policy should retain orphan residue: {error}"
        );
        assert!(residue.is_file());
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());

        std::fs::write(
            repository.join(".aethyme/config.toml"),
            "[cleanup]\nstrict = false\n",
        )
        .unwrap();
        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(
            broker.interrupted_finish_orphan_cleanup_proof(&closed),
            Some(proof.clone())
        );
        broker.cleanup(session.id, false).unwrap();
        assert!(!worktree.exists());
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_none());
    }

    #[test]
    fn orphan_finish_recovery_preserves_a_new_standalone_file() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan recovery late file");
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        remove_worktree_admin_directory(&worktree);

        let late_file = worktree.join("late-after-git-removal.txt");
        std::fs::write(&late_file, b"preserve this file").unwrap();
        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(
            broker.interrupted_finish_orphan_cleanup_proof(&closed),
            Some(proof.clone())
        );

        let error = broker.cleanup(session.id, false).unwrap_err();
        assert!(error.to_string().contains("unverified path"), "{error}");
        assert_eq!(std::fs::read(&late_file).unwrap(), b"preserve this file");
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());
    }

    #[test]
    fn orphan_finish_recovery_preserves_tracked_content_changed_after_verification() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan recovery changed tracked");
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        remove_worktree_admin_directory(&worktree);

        let changed = worktree.join("README.md");
        std::fs::write(&changed, b"new content after authorization").unwrap();
        let error = broker.cleanup(session.id, false).unwrap_err();
        assert!(
            error.to_string().contains("changed after cleanup"),
            "{error}"
        );
        assert_eq!(
            std::fs::read(&changed).unwrap(),
            b"new content after authorization"
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn orphan_finish_recovery_preserves_tracked_executable_mode_changes() {
        use std::os::unix::fs::PermissionsExt;

        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan recovery changed mode");
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        remove_worktree_admin_directory(&worktree);

        let changed = worktree.join("README.md");
        std::fs::write(&changed, b"tracked fixture\n").unwrap();
        std::fs::set_permissions(&changed, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = broker.cleanup(session.id, false).unwrap_err();
        assert!(
            error.to_string().contains("changed executable mode"),
            "{error}"
        );
        assert_eq!(std::fs::read(&changed).unwrap(), b"tracked fixture\n");
        assert_eq!(
            std::fs::metadata(&changed).unwrap().permissions().mode() & 0o111,
            0o111
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn orphan_finish_recovery_preserves_a_symlink_replacing_tracked_content() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan recovery symlink");
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        remove_worktree_admin_directory(&worktree);

        let outside = owner.path().join("outside.txt");
        std::fs::write(&outside, b"outside content").unwrap();
        let tracked_path = worktree.join("README.md");
        std::os::unix::fs::symlink(&outside, &tracked_path).unwrap();
        let error = broker.cleanup(session.id, false).unwrap_err();
        assert!(error.to_string().contains("changed type"), "{error}");
        assert!(
            std::fs::symlink_metadata(&tracked_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside content");
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());
    }

    #[test]
    fn legacy_orphan_proof_cannot_authorize_an_ignored_directory() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "legacy orphan proof");
        let residue = worktree.join("local-dataset/saved-session.json");
        std::fs::create_dir_all(residue.parent().unwrap()).unwrap();
        std::fs::write(&residue, b"saved session data").unwrap();
        let proof = persist_finish_cleanup_intent_with_schema(&mut broker, &session, &worktree, 1);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        remove_worktree_admin_directory(&worktree);
        let closed = broker.store().session(session.id).unwrap();
        assert!(
            broker
                .interrupted_finish_orphan_cleanup_proof(&closed)
                .is_none()
        );

        let error = broker.cleanup(session.id, false).unwrap_err();
        assert!(error.to_string().contains("preserved"), "{error}");
        assert_eq!(std::fs::read(&residue).unwrap(), b"saved session data");
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());
    }

    #[test]
    fn legacy_v2_orphan_proof_without_root_generation_cannot_authorize_residue() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "legacy v2 orphan proof");
        let residue = worktree.join("local-dataset/saved-session.json");
        std::fs::create_dir_all(residue.parent().unwrap()).unwrap();
        std::fs::write(&residue, b"saved session data").unwrap();
        let proof = persist_finish_cleanup_intent_with_schema(&mut broker, &session, &worktree, 2);
        assert!(!proof.disposable_ignored_root_prefixes.is_empty());
        assert!(proof.root_generation_sha256.is_none());
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        remove_worktree_admin_directory(&worktree);
        let closed = broker.store().session(session.id).unwrap();
        assert!(
            broker
                .interrupted_finish_orphan_cleanup_proof(&closed)
                .is_none()
        );

        let error = broker.cleanup(session.id, false).unwrap_err();
        assert!(error.to_string().contains("preserved"), "{error}");
        assert_eq!(std::fs::read(&residue).unwrap(), b"saved session data");
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn orphan_residue_inventory_refuses_a_volume_identity_mismatch() {
        use std::os::unix::fs::MetadataExt;

        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan recovery volume check");
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        let metadata = std::fs::symlink_metadata(&worktree).unwrap();
        let expected_device = metadata.dev().saturating_add(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut budget = OrphanResidueBudget::new(deadline);
        let mut entries = std::collections::BTreeMap::new();
        let tracked = std::collections::BTreeMap::new();
        let mut root = PinnedOrphanRoot::open(&worktree, &proof).unwrap().unwrap();
        root.identity.0 = expected_device;

        let error = collect_orphan_residue(
            &root,
            &root.root,
            "",
            &proof,
            &tracked,
            &mut budget,
            &mut entries,
        )
        .unwrap_err();
        assert!(error.contains("crosses a volume or mount"), "{error}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn pinned_orphan_root_rechecks_its_generation_before_each_removal_pass() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan root generation recheck");
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        let root = PinnedOrphanRoot::open(&worktree, &proof).unwrap().unwrap();
        let replacement = [0x5a_u8; 32];

        #[cfg(target_os = "linux")]
        let name = c"user.aethyme.root-generation";
        #[cfg(target_os = "macos")]
        let name = c"com.aethyme.root-generation";

        #[cfg(target_os = "linux")]
        // SAFETY: the descriptor is the pinned root directory and the xattr
        // name/value buffers remain live for the duration of the call.
        let result = unsafe {
            libc::fsetxattr(
                root.root.as_raw_fd(),
                name.as_ptr(),
                replacement.as_ptr().cast(),
                replacement.len(),
                libc::XATTR_REPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        // SAFETY: the descriptor is the pinned root directory and the xattr
        // name/value buffers remain live for the duration of the call.
        let result = unsafe {
            libc::fsetxattr(
                root.root.as_raw_fd(),
                name.as_ptr(),
                replacement.as_ptr().cast(),
                replacement.len(),
                0,
                libc::XATTR_REPLACE,
            )
        };
        assert_eq!(result, 0);

        let error = root.ensure_binding().unwrap_err();
        assert!(error.contains("generation changed"), "{error}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn orphan_removal_does_not_follow_a_directory_replaced_by_a_symlink() {
        use std::os::unix::fs::symlink;

        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        std::fs::create_dir_all(repository.join("nested")).unwrap();
        std::fs::write(repository.join("nested/tracked.txt"), b"tracked data").unwrap();
        git(&repository, &["add", "nested/tracked.txt"]);
        git(&repository, &["commit", "-qm", "nested tracked fixture"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan ancestor swap protection");
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        let deadline = std::time::Instant::now() + ORPHAN_RESIDUE_SCAN_BUDGET;
        let root = PinnedOrphanRoot::open(&worktree, &proof).unwrap().unwrap();
        let tracked = crate::git::GitRepo::discover(&repository)
            .unwrap()
            .tracked_tree_files_at(&proof.head, deadline)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut entries = std::collections::BTreeMap::new();
        let mut budget = OrphanResidueBudget::new(deadline);
        collect_orphan_residue(
            &root,
            &root.root,
            "",
            &proof,
            &tracked,
            &mut budget,
            &mut entries,
        )
        .unwrap();

        let outside = owner.path().join("moved-nested");
        std::fs::rename(worktree.join("nested"), &outside).unwrap();
        symlink(&outside, worktree.join("nested")).unwrap();

        let error = remove_orphan_residue_pass(&root, &entries, &proof, &mut budget).unwrap_err();
        assert!(error.contains("changed after verification"), "{error}");
        assert_eq!(
            std::fs::read(outside.join("tracked.txt")).unwrap(),
            b"tracked data"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn orphan_removal_preserves_a_new_path_added_after_inventory() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan late path protection");
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        let deadline = std::time::Instant::now() + ORPHAN_RESIDUE_SCAN_BUDGET;
        let root = PinnedOrphanRoot::open(&worktree, &proof).unwrap().unwrap();
        let tracked = crate::git::GitRepo::discover(&repository)
            .unwrap()
            .tracked_tree_files_at(&proof.head, deadline)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut entries = std::collections::BTreeMap::new();
        let mut budget = OrphanResidueBudget::new(deadline);
        collect_orphan_residue(
            &root,
            &root.root,
            "",
            &proof,
            &tracked,
            &mut budget,
            &mut entries,
        )
        .unwrap();

        let late_path = worktree.join("added-after-inventory.txt");
        std::fs::write(&late_path, b"preserve late path").unwrap();
        assert_eq!(
            remove_orphan_residue_pass(&root, &entries, &proof, &mut budget).unwrap(),
            OrphanRemovePass::Retry
        );
        assert_eq!(std::fs::read(&late_path).unwrap(), b"preserve late path");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn orphan_residue_inventory_enforces_a_seeded_enumeration_limit() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan recovery inventory cap");
        let nested = worktree.join("local-dataset/outer");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("one"), b"1").unwrap();
        std::fs::write(nested.join("two"), b"2").unwrap();
        std::fs::write(nested.join("three"), b"3").unwrap();
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        let root = PinnedOrphanRoot::open(&worktree, &proof).unwrap().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut budget = OrphanResidueBudget::with_limits(deadline, 2, 1024, 1024);
        let mut entries = std::collections::BTreeMap::new();
        let tracked = std::collections::BTreeMap::new();
        let nested_directory = open_directory_at(
            root.root.as_raw_fd(),
            &CString::new("local-dataset").unwrap(),
        )
        .unwrap();

        let error = collect_orphan_residue(
            &root,
            &nested_directory,
            "local-dataset",
            &proof,
            &tracked,
            &mut budget,
            &mut entries,
        )
        .unwrap_err();
        assert!(error.contains("total enumeration safety limit"), "{error}");
        assert!(nested.join("one").is_file());

        let mut name_budget = OrphanResidueBudget::with_limits(deadline, 10, 1, 1024);
        let mut name_entries = std::collections::BTreeMap::new();
        let error = collect_orphan_residue(
            &root,
            &nested_directory,
            "local-dataset",
            &proof,
            &tracked,
            &mut name_budget,
            &mut name_entries,
        )
        .unwrap_err();
        assert!(error.contains("total name-byte safety limit"), "{error}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn orphan_residue_inventory_enforces_a_seeded_retained_path_limit() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan recovery path byte cap");
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        let root = PinnedOrphanRoot::open(&worktree, &proof).unwrap().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut budget = OrphanResidueBudget::with_limits(
            deadline,
            MAX_ORPHAN_RESIDUE_ENUMERATED_ENTRIES,
            MAX_ORPHAN_RESIDUE_ENUMERATED_NAME_BYTES,
            1,
        );
        let mut entries = std::collections::BTreeMap::new();
        let tracked = std::collections::BTreeMap::new();

        let error = collect_orphan_residue(
            &root,
            &root.root,
            "",
            &proof,
            &tracked,
            &mut budget,
            &mut entries,
        )
        .unwrap_err();
        assert!(error.contains("retained path safety limit"), "{error}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn tracked_tree_directory_lookup_matches_complete_path_components() {
        fn file(path: &str) -> crate::git::GitTreeFile {
            crate::git::GitTreeFile {
                path: path.to_owned(),
                mode: "100644".into(),
                object_id: "0".repeat(40),
            }
        }

        let files = [file("nested/tracked.txt"), file("nestedish/other.txt")]
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<std::collections::BTreeMap<_, _>>();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        assert!(tracked_tree_contains_directory(&files, "nested", deadline).unwrap());
        assert!(!tracked_tree_contains_directory(&files, "nestedi", deadline).unwrap());
        assert!(!tracked_tree_contains_directory(&files, "nestedish/other.txt", deadline).unwrap());
    }

    #[test]
    fn interrupted_finish_recovery_still_refuses_an_unrepresented_session_head() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("unrepresented recovery protection", None)
            .unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        std::fs::write(worktree.join("unpublished.txt"), "not submitted\n").unwrap();
        git(&worktree, &["add", "unpublished.txt"]);
        git(&worktree, &["commit", "-qm", "unrepresented session head"]);

        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        let targets = broker.cleanup_delivery_targets().unwrap();
        let (provenance, _) = broker
            .cleanup_provenance(&session, &proof.head, &targets)
            .unwrap();
        assert_ne!(
            provenance.representation,
            crate::CleanupRepresentation::Represented,
            "this fixture intentionally leaves its session commit unsubmitted"
        );
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        let closed = broker.store().session(session.id).unwrap();
        assert!(
            broker.interrupted_finish_cleanup_proof(&closed).is_none(),
            "a durable finish intent must not substitute for delivery representation"
        );

        assert!(broker.cleanup(session.id, false).is_err());
        assert!(worktree.join("unpublished.txt").is_file());
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
    }

    #[test]
    fn registered_finish_recovery_discards_opaque_unknown_ignored_directory_by_default() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "opaque ignored recovery");

        let residue = worktree.join("local-dataset/saved-session.json");
        std::fs::create_dir_all(residue.parent().unwrap()).unwrap();
        std::fs::write(&residue, b"opaque ignored directory payload").unwrap();
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(
            broker.interrupted_finish_cleanup_proof(&closed),
            Some(proof.clone()),
            "unknown ignored directory contents are opaque in disposable mode"
        );

        broker.cleanup(session.id, false).unwrap();

        assert!(!worktree.exists());
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_none());
    }

    #[cfg(not(unix))]
    #[test]
    fn explicit_cleanup_keeps_legacy_behavior_when_artifact_protocol_is_unavailable() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "unsupported protocol cleanup");
        broker.close(session.id).unwrap();

        assert!(matches!(
            crate::file_lock::artifact_use::try_artifact_use_reclaimer(&repository, &worktree)
                .unwrap(),
            crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(
                crate::file_lock::artifact_use::ArtifactUseReclaimBlock::ProtocolUnavailable
            )
        ));

        broker.cleanup(session.id, false).unwrap();
        assert!(!worktree.exists());
    }

    #[test]
    fn registered_cleanup_preserves_checkout_after_git_removal_timeout() {
        let owner = tempfile::tempdir().unwrap();
        let repository = initialized_repository(owner.path());

        let worktree = owner.path().join("registered-worktree");
        let worktree_text = worktree.to_string_lossy().into_owned();
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "fixture-adopted",
                &worktree_text,
                "HEAD",
            ],
        );
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let adopted = broker
            .adopt(&worktree, Some("closed checkout cleanup"))
            .unwrap();
        broker.close(adopted.id).unwrap();
        let evidence = worktree.join("keep-for-retry.txt");
        std::fs::write(&evidence, b"preserve this checkout").unwrap();

        let result = with_git_deadline(Duration::ZERO, || broker.cleanup(adopted.id, true));
        assert!(result.is_err(), "expired Git budget must refuse removal");
        assert_eq!(std::fs::read(&evidence).unwrap(), b"preserve this checkout");
        assert_eq!(
            broker.store().session(adopted.id).unwrap().cleanup_state,
            SessionCleanupState::Closed,
            "failed removal must leave the session available for retry"
        );
        assert!(
            crate::git::GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref("refs/heads/fixture-adopted")
                .is_some(),
            "failed removal must retain its branch"
        );
    }

    #[test]
    fn adopted_cleanup_preserves_non_directory_replacement_and_branch() {
        let owner = tempfile::tempdir().unwrap();
        let repository = initialized_repository(owner.path());
        let parent = owner.path().join("adopted-parent");
        std::fs::create_dir(&parent).unwrap();
        let worktree = parent.join("checkout");
        let worktree_text = worktree.to_string_lossy().into_owned();
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "fixture-adopted-replacement",
                &worktree_text,
                "HEAD",
            ],
        );
        let evidence = worktree.join("keep-for-retry.txt");
        std::fs::write(&evidence, b"preserve adopted checkout").unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let adopted = broker
            .adopt(&worktree, Some("replacement cleanup"))
            .unwrap();
        broker.close(adopted.id).unwrap();

        let preserved = parent.join("preserved-checkout");
        std::fs::rename(&worktree, &preserved).unwrap();
        std::fs::write(&worktree, b"replacement data").unwrap();

        let result = broker.cleanup(adopted.id, true);

        assert!(
            result.is_err(),
            "a non-directory replacement must be retained"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not a real directory")
        );
        assert_eq!(
            broker.store().session(adopted.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert_eq!(std::fs::read(&worktree).unwrap(), b"replacement data");
        assert_eq!(
            std::fs::read(preserved.join("keep-for-retry.txt")).unwrap(),
            b"preserve adopted checkout"
        );
        assert!(
            crate::git::GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref("refs/heads/fixture-adopted-replacement")
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn adopted_cleanup_preserves_session_and_branch_when_path_metadata_fails() {
        let owner = tempfile::tempdir().unwrap();
        let repository = initialized_repository(owner.path());
        let parent = owner.path().join("adopted-parent");
        std::fs::create_dir(&parent).unwrap();
        let worktree = parent.join("checkout");
        let worktree_text = worktree.to_string_lossy().into_owned();
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "fixture-adopted",
                &worktree_text,
                "HEAD",
            ],
        );
        let evidence = worktree.join("keep-for-retry.txt");
        std::fs::write(&evidence, b"preserve adopted checkout").unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let adopted = broker
            .adopt(&worktree, Some("metadata-error cleanup"))
            .unwrap();
        broker.close(adopted.id).unwrap();

        let preserved_parent = owner.path().join("preserved-parent");
        std::fs::rename(&parent, &preserved_parent).unwrap();
        std::fs::write(&parent, b"not a directory").unwrap();

        let result = broker.cleanup(adopted.id, true);

        assert!(
            result.is_err(),
            "metadata uncertainty must retain the session"
        );
        assert_eq!(
            broker.store().session(adopted.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert_eq!(
            std::fs::read(preserved_parent.join("checkout/keep-for-retry.txt")).unwrap(),
            b"preserve adopted checkout"
        );
        assert!(
            crate::git::GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref("refs/heads/fixture-adopted")
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn spawned_cleanup_preserves_dangling_replacement_and_branch() {
        use std::os::unix::fs::symlink;

        let owner = tempfile::tempdir().unwrap();
        let repository = initialized_repository(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("dangling cleanup replacement", None)
            .unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        broker.close(session.id).unwrap();
        let evidence = worktree.join("keep-for-retry.txt");
        std::fs::write(&evidence, b"preserve spawned checkout").unwrap();

        let preserved = owner.path().join("preserved-spawned-checkout");
        std::fs::rename(&worktree, &preserved).unwrap();
        let missing_target = owner.path().join("missing-target");
        symlink(&missing_target, &worktree).unwrap();

        let result = broker.cleanup(session.id, true);

        assert!(
            result.is_err(),
            "a dangling symlink is not an absent worktree"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not a real directory")
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(
            std::fs::symlink_metadata(&worktree)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(preserved.join("keep-for-retry.txt")).unwrap(),
            b"preserve spawned checkout"
        );
        assert!(
            crate::git::GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref(&format!("refs/heads/{}", session.branch))
                .is_some()
        );
    }

    #[test]
    fn spawned_cleanup_refuses_orphan_paths_without_a_valid_same_repository_pointer() {
        for invalid_pointer in ["missing", "malformed", "wrong-repository", "metadata-error"] {
            let owner = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
            let repository = initialized_repository(owner.path());
            let foreign_repository = if invalid_pointer == "wrong-repository" {
                Some(initialized_repository(&owner.path().join("foreign")))
            } else {
                None
            };
            let storage = crate::test_storage::TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let session = broker
                .start_worktree("invalid orphan pointer", None)
                .unwrap();
            let worktree = Path::new(&session.worktree_path).to_path_buf();
            broker.close(session.id).unwrap();
            let evidence = worktree.join("keep-for-retry.txt");
            std::fs::write(&evidence, b"preserve this checkout").unwrap();
            remove_worktree_admin_directory(&worktree);

            match invalid_pointer {
                "missing" => std::fs::remove_file(worktree.join(".git")).unwrap(),
                "malformed" => {
                    std::fs::write(worktree.join(".git"), b"not a gitdir pointer\n").unwrap()
                }
                "wrong-repository" => {
                    let foreign = foreign_repository.as_ref().unwrap();
                    let gitdir = foreign.join(".git/worktrees/foreign-checkout");
                    std::fs::write(
                        worktree.join(".git"),
                        format!("gitdir: {}\n", gitdir.display()),
                    )
                    .unwrap();
                }
                "metadata-error" => {
                    let non_directory = repository.join(".git/not-a-directory");
                    std::fs::write(&non_directory, b"file where a directory is required").unwrap();
                    let gitdir = non_directory.join("worktrees/missing-admin");
                    std::fs::write(
                        worktree.join(".git"),
                        format!("gitdir: {}\n", gitdir.display()),
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }

            let result = broker.cleanup(session.id, true);

            assert!(
                result.is_err(),
                "{invalid_pointer} orphan evidence must not authorize removal"
            );
            assert_eq!(
                std::fs::read(&evidence).unwrap(),
                b"preserve this checkout",
                "{invalid_pointer} pointer must leave the checkout contents intact"
            );
            assert_eq!(
                broker.store().session(session.id).unwrap().cleanup_state,
                SessionCleanupState::Closed,
                "{invalid_pointer} pointer must leave the session retryable"
            );
            assert!(
                crate::git::GitRepo::discover(&repository)
                    .unwrap()
                    .resolve_ref(&format!("refs/heads/{}", session.branch))
                    .is_some(),
                "{invalid_pointer} pointer must retain the branch"
            );
        }
    }
}

#[cfg(test)]
mod tracked_submodule_cleanup_tests {
    use super::CleanupDisposition;
    use crate::git::GitRepo;
    use crate::test_storage::TestStorage;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        let status = command
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "submodule cleanup test")
            .env("GIT_AUTHOR_EMAIL", "submodule-cleanup@example.invalid")
            .env("GIT_COMMITTER_NAME", "submodule cleanup test")
            .env("GIT_COMMITTER_EMAIL", "submodule-cleanup@example.invalid")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn cleanup_retains_dirty_submodule_files_under_ignore_all_configuration() {
        for child_state in ["modified", "untracked"] {
            let owner = tempfile::tempdir().unwrap();
            let repository = owner.path().join("repository");
            let source = owner.path().join("submodule-source");
            std::fs::create_dir(&repository).unwrap();
            std::fs::create_dir(&source).unwrap();
            git(&source, &["init", "-q", "-b", "main"]);
            git(&source, &["config", "user.name", "submodule cleanup test"]);
            git(
                &source,
                &["config", "user.email", "submodule-cleanup@example.invalid"],
            );
            std::fs::write(source.join("tracked.txt"), "initial child content\n").unwrap();
            git(&source, &["add", "tracked.txt"]);
            git(&source, &["commit", "-qm", "initial child commit"]);

            git(&repository, &["init", "-q", "-b", "main"]);
            git(
                &repository,
                &["config", "user.name", "submodule cleanup test"],
            );
            git(
                &repository,
                &["config", "user.email", "submodule-cleanup@example.invalid"],
            );
            std::fs::write(repository.join("README.md"), "base\n").unwrap();
            git(&repository, &["add", "README.md"]);
            git(&repository, &["commit", "-qm", "superproject base"]);
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
            git(&repository, &["commit", "-qm", "add child submodule"]);
            git(&repository, &["config", "diff.ignoreSubmodules", "all"]);
            git(&repository, &["config", "submodule.sub.ignore", "all"]);

            let storage = TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let session = broker
                .start_worktree("dirty child submodule", None)
                .unwrap();
            let worktree = Path::new(&session.worktree_path);
            assert_eq!(
                GitRepo::discover(worktree).unwrap().head_commit().unwrap(),
                GitRepo::discover(&repository)
                    .unwrap()
                    .head_commit()
                    .unwrap(),
                "the superproject commit is already the parent checkout HEAD"
            );
            git(
                worktree,
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
            let child = worktree.join("sub");
            let dirty_path = match child_state {
                "modified" => {
                    let path = child.join("tracked.txt");
                    std::fs::write(&path, "changed child content\n").unwrap();
                    path
                }
                "untracked" => {
                    let path = child.join("new.txt");
                    std::fs::write(&path, "new child content\n").unwrap();
                    path
                }
                _ => unreachable!(),
            };
            broker.close(session.id).unwrap();

            let closed_session = broker.store().session(session.id).unwrap();
            let item = broker
                .cleanup_item(&closed_session)
                .unwrap()
                .expect("the closed broker worktree remains in cleanup planning");
            assert_eq!(item.disposition, CleanupDisposition::Dirty, "{child_state}");
            assert!(
                item.reason
                    .contains("worktree has uncommitted or untracked changes"),
                "{child_state}: {}",
                item.reason
            );
            let error = broker.cleanup(session.id, false).unwrap_err().to_string();
            assert!(
                error.contains("uncommitted or untracked changes"),
                "{error}"
            );
            assert!(
                worktree.is_dir(),
                "{child_state} submodule worktree was removed"
            );
            assert!(dirty_path.is_file(), "{child_state} child data was removed");
        }
    }
}

#[cfg(test)]
mod cleanup_target_snapshot_tests {
    use super::CleanupDisposition;
    use crate::BrokerOpError;
    use crate::test_storage::TestStorage;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    const ROOT_ENV: &str = "AETHYME_CLEANUP_TARGET_TRACE_ROOT";
    const CHILD_TEST: &str = "broker::cleanup::cleanup_target_snapshot_tests::cleanup_plan_reuses_one_delivery_target_snapshot";
    const TRANSIENT_ROOT_ENV: &str = "AETHYME_CLEANUP_TARGET_FAILURE_ROOT";
    const FAIL_HEAD_ROOT_ENV: &str = "AETHYME_CLEANUP_TARGET_FAIL_HEAD_ROOT";
    const FAIL_HEAD_MARKER_ENV: &str = "AETHYME_CLEANUP_TARGET_FAIL_HEAD_MARKER";
    const REAL_GIT_ENV: &str = "AETHYME_CLEANUP_TARGET_REAL_GIT";
    const TRANSIENT_CHILD_TEST: &str = "broker::cleanup::cleanup_target_snapshot_tests::cleanup_plan_fails_closed_after_transient_primary_head_failure";

    fn clear_git_environment(command: &mut Command) {
        let keys = std::env::vars_os()
            .map(|(key, _)| key)
            .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in keys {
            command.env_remove(key);
        }
    }

    fn git(root: &Path, args: &[&str]) -> Output {
        let mut command = Command::new("git");
        clear_git_environment(&mut command);
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn git_text(root: &Path, args: &[&str]) -> String {
        String::from_utf8(git(root, args).stdout)
            .unwrap()
            .trim()
            .to_owned()
    }

    fn initialized_repository(parent: &Path) -> PathBuf {
        let repository = parent.join("repository");
        std::fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        git(&repository, &["config", "user.name", "Cleanup target test"]);
        git(
            &repository,
            &["config", "user.email", "cleanup-target@example.invalid"],
        );
        std::fs::write(repository.join("README.md"), "base\n").unwrap();
        git(&repository, &["add", "README.md"]);
        git(&repository, &["commit", "-qm", "initial"]);
        repository
    }

    struct UpstreamLookupTrace {
        starts: Vec<String>,
    }

    impl UpstreamLookupTrace {
        fn count(&self) -> usize {
            self.starts.len()
        }

        fn diagnostic(&self) -> String {
            self.starts.join("\n")
        }
    }

    fn upstream_lookup_trace(trace: &Path) -> UpstreamLookupTrace {
        let contents = std::fs::read_to_string(trace)
            .unwrap_or_else(|error| panic!("read Git Trace2 file {}: {error}", trace.display()));
        let mut starts = Vec::new();
        for (index, line) in contents.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let event = serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|error| {
                panic!(
                    "invalid Git Trace2 JSON at {}:{}: {error}; record={line:?}",
                    trace.display(),
                    index + 1
                )
            });
            if event["event"] != "start" {
                continue;
            }
            let argv = event["argv"].as_array().unwrap_or_else(|| {
                panic!(
                    "Git Trace2 start event at {}:{} has no argv array: {line}",
                    trace.display(),
                    index + 1
                )
            });
            if argv.iter().any(|argument| argument == "@{upstream}") {
                // Trace2 may omit cwd when the event uses Git's default repo;
                // retain every valid upstream start record, including raw argv.
                starts.push(line.to_owned());
            }
        }
        UpstreamLookupTrace { starts }
    }

    #[test]
    fn cleanup_live_ownership_snapshot_honors_its_effective_deadline() {
        let parent = tempfile::tempdir().unwrap();
        let repository = initialized_repository(parent.path());
        let storage = TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker.start_worktree("live ownership", None).unwrap();
        let expired = broker.cleanup_live_checkout_snapshot(Some(std::time::Instant::now()));
        assert!(matches!(
            expired,
            Err(BrokerOpError::RepresentationUnavailable { .. })
        ));
        let observed = broker.cleanup_live_checkout_snapshot(None).unwrap();
        assert!(observed.iter().any(|(id, _)| *id == session.id));
        assert!(Path::new(&session.worktree_path).is_dir());
    }

    #[test]
    fn cleanup_plan_reuses_one_delivery_target_snapshot() {
        // `cleanup_plan_scanned_within` captures targets once, then passes
        // that snapshot through `cleanup_item_scanned_with_tips`. Trace2
        // catches a regression that resolves `@{upstream}` per worktree.
        if let Some(parent) = std::env::var_os(ROOT_ENV) {
            let parent = PathBuf::from(parent);
            let repository = initialized_repository(&parent);
            let storage = TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let represented_one = broker.start_worktree("represented one", None).unwrap();
            let represented_one_worktree = PathBuf::from(&represented_one.worktree_path);
            std::fs::write(
                represented_one_worktree.join("represented-one.txt"),
                "delivered\n",
            )
            .unwrap();
            git(&represented_one_worktree, &["add", "represented-one.txt"]);
            git(
                &represented_one_worktree,
                &["commit", "-qm", "first delivered commit"],
            );
            git(
                &repository,
                &["merge", "--ff-only", represented_one.branch.as_str()],
            );
            let one_worktree_main_head = git_text(&repository, &["rev-parse", "HEAD"]);
            broker.close(represented_one.id).unwrap();

            let trace = std::env::var_os("GIT_TRACE2_EVENT")
                .map(PathBuf::from)
                .expect("parent supplies a private Git trace path");
            std::fs::write(&trace, "").unwrap();
            let one_worktree_plan = broker.cleanup_plan_recorded().unwrap();
            assert!(
                one_worktree_plan
                    .target_snapshot
                    .contains(&format!("HEAD={one_worktree_main_head}"))
            );
            assert_eq!(one_worktree_plan.worktrees.len(), 1);
            assert_eq!(
                one_worktree_plan.worktrees[0].disposition,
                CleanupDisposition::Eligible,
                "the worktree represented on the captured primary target remains eligible"
            );
            let one_worktree_trace = upstream_lookup_trace(&trace);
            assert!(
                one_worktree_trace.count() > 0,
                "the trace observes the target-capture lookup; Trace2 starts:\n{}",
                one_worktree_trace.diagnostic()
            );

            let represented_two = broker.start_worktree("represented two", None).unwrap();
            let pending = broker.start_worktree("pending", None).unwrap();
            let represented_two_worktree = PathBuf::from(&represented_two.worktree_path);
            let pending_worktree = PathBuf::from(&pending.worktree_path);
            std::fs::write(
                represented_two_worktree.join("represented-two.txt"),
                "delivered\n",
            )
            .unwrap();
            git(&represented_two_worktree, &["add", "represented-two.txt"]);
            git(
                &represented_two_worktree,
                &["commit", "-qm", "second delivered commit"],
            );
            git(
                &repository,
                &["merge", "--ff-only", represented_two.branch.as_str()],
            );
            let main_head = git_text(&repository, &["rev-parse", "HEAD"]);
            std::fs::write(pending_worktree.join("pending.txt"), "not delivered\n").unwrap();
            git(&pending_worktree, &["add", "pending.txt"]);
            git(&pending_worktree, &["commit", "-qm", "session-only commit"]);
            let pending_head = git_text(&pending_worktree, &["rev-parse", "HEAD"]);
            for session in [&represented_two, &pending] {
                broker.close(session.id).unwrap();
            }

            std::fs::write(&trace, "").unwrap();
            let plan = broker.cleanup_plan_recorded().unwrap();

            assert!(
                plan.target_snapshot.contains(&format!("HEAD={main_head}")),
                "the digest records the primary checkout target"
            );
            assert!(
                !plan
                    .target_snapshot
                    .contains(&format!("HEAD={pending_head}")),
                "a session checkout head must not become the plan target"
            );
            assert_eq!(plan.worktrees.len(), 3);
            assert_eq!(
                plan.worktrees
                    .iter()
                    .filter(|item| item.disposition == CleanupDisposition::Eligible)
                    .count(),
                2,
                "the two worktrees represented by a plan target remain eligible"
            );
            assert_eq!(
                plan.worktrees
                    .iter()
                    .find(|item| item.session_id == pending.id)
                    .map(|item| item.disposition),
                Some(CleanupDisposition::PendingCommits),
                "the unique session commit stays pending against the primary targets"
            );
            let three_worktree_trace = upstream_lookup_trace(&trace);
            assert_eq!(
                three_worktree_trace.count(),
                one_worktree_trace.count(),
                "a cleanup plan's upstream lookup count must not grow when worktrees grow from one to three; one-worktree Trace2 starts:\n{}\nthree-worktree Trace2 starts:\n{}",
                one_worktree_trace.diagnostic(),
                three_worktree_trace.diagnostic()
            );
            return;
        }

        let parent = tempfile::tempdir().unwrap();
        let parent_path = parent.path().canonicalize().unwrap();
        let trace = parent_path.join("git-trace.json");
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", CHILD_TEST, "--nocapture"]);
        clear_git_environment(&mut child);
        let output = child
            .env(ROOT_ENV, &parent_path)
            .env("GIT_TRACE2_EVENT", &trace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_plan_fails_closed_after_transient_primary_head_failure() {
        if let Some(parent) = std::env::var_os(TRANSIENT_ROOT_ENV) {
            let parent = PathBuf::from(parent);
            let repository = initialized_repository(&parent);
            let expected_primary = std::env::var_os(FAIL_HEAD_ROOT_ENV)
                .map(PathBuf::from)
                .expect("parent supplies the canonical primary checkout path");
            assert_eq!(repository.canonicalize().unwrap(), expected_primary);

            let storage = TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let session = broker.start_worktree("delivered", None).unwrap();
            let worktree = PathBuf::from(&session.worktree_path);
            std::fs::write(worktree.join("delivered.txt"), "represented on main\n").unwrap();
            git(&worktree, &["add", "delivered.txt"]);
            git(&worktree, &["commit", "-qm", "commit for main"]);
            let session_head = git_text(&worktree, &["rev-parse", "HEAD"]);
            git(
                &repository,
                &["merge", "--ff-only", session.branch.as_str()],
            );
            let main_head = git_text(&repository, &["rev-parse", "HEAD"]);
            assert_eq!(main_head, session_head);
            broker.close(session.id).unwrap();

            let marker = PathBuf::from(
                std::env::var_os(FAIL_HEAD_MARKER_ENV)
                    .expect("parent supplies one-shot failure marker"),
            );
            std::fs::write(&marker, "fail primary HEAD once").unwrap();
            let plan = broker.cleanup_plan_recorded().unwrap();
            assert!(
                !marker.exists(),
                "the Git shim must consume the one-shot primary HEAD failure"
            );
            assert!(
                !plan.target_snapshot.contains(&format!("HEAD={main_head}")),
                "a later successful lookup must not silently change the plan's target digest"
            );
            let item = plan
                .worktrees
                .iter()
                .find(|item| item.session_id == session.id)
                .expect("the closed session remains visible in the plan");
            assert_eq!(
                item.disposition,
                CleanupDisposition::InspectionFailed,
                "an unbound target must not make a session eligible"
            );
            assert!(item.reason.contains("could not resolve primary HEAD"));

            let (disposition, _, _) = broker
                .cleanup_eligibility(session.id, &worktree)
                .expect("the fresh direct inspection can now resolve primary HEAD");
            assert_eq!(
                disposition,
                CleanupDisposition::Eligible,
                "the same session is eligible once a fresh target set is resolved"
            );
            return;
        }

        let real_git = match crate::git::git_output_trust() {
            crate::git::GitOutputTrust::Undecorated { resolved, .. } => resolved
                .canonicalize()
                .expect("trusted Git executable should have a canonical path"),
            other => panic!("transient-failure fixture requires trusted Git: {other:?}"),
        };
        let parent = tempfile::tempdir().unwrap();
        let parent_path = parent.path().canonicalize().unwrap();
        let shim_dir = parent_path.join("bin");
        std::fs::create_dir(&shim_dir).unwrap();
        let marker = parent_path.join("fail-first-primary-head");
        let primary = parent_path.join("repository");
        let wrapper = shim_dir.join("git");
        std::fs::write(
            &wrapper,
            r#"#!/bin/sh
if [ "$1" = "rev-parse" ] &&
   [ "$2" = "HEAD" ] &&
   [ "$(pwd -P)" = "$AETHYME_CLEANUP_TARGET_FAIL_HEAD_ROOT" ] &&
   [ -f "$AETHYME_CLEANUP_TARGET_FAIL_HEAD_MARKER" ]; then
  rm -f "$AETHYME_CLEANUP_TARGET_FAIL_HEAD_MARKER"
  echo "one-shot injected primary HEAD failure" >&2
  exit 128
fi
exec "$AETHYME_CLEANUP_TARGET_REAL_GIT" "$@"
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let original_path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(shim_dir).chain(std::env::split_paths(&original_path)),
        )
        .unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", TRANSIENT_CHILD_TEST, "--nocapture"]);
        clear_git_environment(&mut child);
        let output = child
            .env(TRANSIENT_ROOT_ENV, &parent_path)
            .env(FAIL_HEAD_ROOT_ENV, &primary)
            .env(FAIL_HEAD_MARKER_ENV, &marker)
            .env(REAL_GIT_ENV, &real_git)
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
