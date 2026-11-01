//! Bounded descriptor-relative inventory, logging, and removal for environment
//! cache entries. The selected host-state root is canonicalized once; every
//! descendant is then opened without following links or crossing the cache
//! root's mount identity.

#[cfg(target_os = "macos")]
use std::ffi::CStr;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::ffi::{CString, OsStr, OsString};
use std::fmt;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::fs::File;
use std::io;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::Instant;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use super::managed_storage_inventory as fs;

#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_ENTRIES: usize = 100_000;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_DEPTH: usize = 64;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_DURATION: Duration = Duration::from_secs(3);
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_KEY_BYTES: usize = 4096;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_TEMPLATE_MARKER_BYTES: usize = 1024 * 1024;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_TEMPLATE_MARKER_TOTAL_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CacheGcFsError {
    pub code: &'static str,
    pub message: String,
}

impl CacheGcFsError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for CacheGcFsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CacheFsIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct BuildEntry {
    pub repository: String,
    pub component: String,
    pub digest: String,
    pub bytes: u64,
    pub inodes: u64,
    pub last_used_at_ms: Option<i64>,
    pub key: Option<String>,
    pub identity: CacheFsIdentity,
    pub path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct TemplateEntry {
    pub repository: String,
    pub component: String,
    pub marker_name: String,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub parent_identity: CacheFsIdentity,
    pub identity: CacheFsIdentity,
}

#[derive(Debug)]
pub(crate) struct CacheInventory {
    pub cache_root: PathBuf,
    pub cache_identity: CacheFsIdentity,
    pub host_total_bytes: u64,
    pub builds: Vec<BuildEntry>,
    pub templates: Vec<TemplateEntry>,
    pub templates_by_repository: std::collections::BTreeMap<String, usize>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
const CACHE_PRUNE_TAIL_WINDOW_BYTES: u64 = 256 * 1024;

#[derive(Debug)]
pub(crate) struct CachePruneLog {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    cache_fd: OwnedFd,
    environment_root: PathBuf,
    cache_identity: CacheFsIdentity,
    cache_path: PathBuf,
    cap: u64,
}

impl CachePruneLog {
    pub(crate) fn append(&mut self, _text: &str) -> io::Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            validate_cache_root_path(
                &self.environment_root,
                &self.cache_path,
                self.cache_identity,
            )
            .map_err(cache_io_error)?;
            append_prune_log(
                self.cache_fd.as_raw_fd(),
                &self.environment_root,
                &self.cache_path,
                self.cache_identity,
                self.cap,
                _text,
            )
            .map_err(cache_io_error)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "safe environment-cache logs are supported only on macOS and Linux",
            ))
        }
    }

    pub(crate) fn tail(&mut self, _lines: usize) -> io::Result<Vec<String>> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            validate_cache_root_path(
                &self.environment_root,
                &self.cache_path,
                self.cache_identity,
            )
            .map_err(cache_io_error)?;
            tail_prune_log(
                self.cache_fd.as_raw_fd(),
                &self.environment_root,
                &self.cache_path,
                self.cache_identity,
                _lines,
            )
            .map_err(cache_io_error)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "safe environment-cache logs are supported only on macOS and Linux",
            ))
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn cache_io_error(error: CacheGcFsError) -> io::Error {
    io::Error::other(error.to_string())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn read_log_tail(file: &mut File, lines: usize) -> io::Result<Vec<String>> {
    if lines == 0 {
        return Ok(Vec::new());
    }
    let length = file.metadata()?.len();
    let start = length.saturating_sub(CACHE_PRUNE_TAIL_WINDOW_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    (&mut *file)
        .take(CACHE_PRUNE_TAIL_WINDOW_BYTES)
        .read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut all: Vec<&str> = text.lines().collect();
    if start > 0 && !all.is_empty() {
        all.remove(0);
    }
    let skip = all.len().saturating_sub(lines);
    Ok(all[skip..].iter().map(|line| (*line).to_string()).collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteOutcome {
    Removed,
    Gone,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct CacheRoot {
    path: PathBuf,
    fd: OwnedFd,
    identity: CacheFsIdentity,
    device: u64,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct MountIdentity(Vec<u8>);

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn mount_identity(fd: libc::c_int, path: &Path) -> Result<MountIdentity, CacheGcFsError> {
    #[cfg(target_os = "linux")]
    {
        let empty = CString::new("").expect("constant has no NUL");
        let mut stat = std::mem::MaybeUninit::<libc::statx>::zeroed();
        // SAFETY: `fd` is an open descriptor and AT_EMPTY_PATH asks statx to
        // inspect that descriptor; `stat` points to writable storage.
        if unsafe {
            libc::statx(
                fd,
                empty.as_ptr(),
                libc::AT_EMPTY_PATH,
                libc::STATX_MNT_ID,
                stat.as_mut_ptr(),
            )
        } < 0
        {
            return Err(io_error(
                "mount_identity_unavailable",
                path,
                std::io::Error::last_os_error(),
            ));
        }
        // SAFETY: successful statx initialized the output structure.
        let stat = unsafe { stat.assume_init() };
        if stat.stx_mask & libc::STATX_MNT_ID == 0 {
            return Err(CacheGcFsError::new(
                "mount_identity_unavailable",
                format!("{} has no kernel mount identity", path.display()),
            ));
        }
        Ok(MountIdentity(stat.stx_mnt_id.to_ne_bytes().to_vec()))
    }

    #[cfg(target_os = "macos")]
    {
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
        // SAFETY: `fd` is an open descriptor and `stat` points to writable
        // storage for the fstatfs result.
        if unsafe { libc::fstatfs(fd, stat.as_mut_ptr()) } < 0 {
            return Err(io_error(
                "mount_identity_unavailable",
                path,
                std::io::Error::last_os_error(),
            ));
        }
        // SAFETY: successful fstatfs initialized the output structure.
        let stat = unsafe { stat.assume_init() };
        // SAFETY: fstatfs returns NUL-terminated mount and filesystem names.
        let mount_on = unsafe { CStr::from_ptr(stat.f_mntonname.as_ptr()) }.to_bytes();
        // SAFETY: fstatfs returns NUL-terminated mount and filesystem names.
        let mount_from = unsafe { CStr::from_ptr(stat.f_mntfromname.as_ptr()) }.to_bytes();
        // SAFETY: fstatfs returns a NUL-terminated filesystem type name.
        let filesystem_type = unsafe { CStr::from_ptr(stat.f_fstypename.as_ptr()) }.to_bytes();
        if mount_on.is_empty() || filesystem_type.is_empty() {
            return Err(CacheGcFsError::new(
                "mount_identity_unavailable",
                format!(
                    "{} has incomplete filesystem mount metadata",
                    path.display()
                ),
            ));
        }
        // SAFETY: the fsid is an initialized fixed-size C value. Reading its
        // object representation as bytes is valid for identity comparison.
        let fsid = unsafe {
            std::slice::from_raw_parts(
                (&stat.f_fsid as *const libc::fsid_t).cast::<u8>(),
                std::mem::size_of_val(&stat.f_fsid),
            )
        };
        let mut identity = Vec::new();
        for part in [fsid, mount_on, mount_from, filesystem_type] {
            identity.extend_from_slice(&(part.len() as u64).to_ne_bytes());
            identity.extend_from_slice(part);
        }
        return Ok(MountIdentity(identity));
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn verify_same_mount(
    parent_fd: libc::c_int,
    parent_path: &Path,
    child_fd: libc::c_int,
    child_path: &Path,
) -> Result<(), CacheGcFsError> {
    if mount_identity(parent_fd, parent_path)? != mount_identity(child_fd, child_path)? {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            format!(
                "{} crosses onto another mounted filesystem",
                child_path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Budget {
    deadline: Instant,
    entries: usize,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Budget {
    fn new(deadline: Option<Instant>) -> Self {
        let ceiling = Instant::now() + MAX_DURATION;
        Self {
            deadline: deadline.map_or(ceiling, |deadline| deadline.min(ceiling)),
            entries: 0,
        }
    }

    fn check(&self) -> Result<(), CacheGcFsError> {
        if Instant::now() >= self.deadline {
            Err(CacheGcFsError::new(
                "deadline_exceeded",
                "environment cache filesystem operation exceeded its bounded deadline",
            ))
        } else {
            Ok(())
        }
    }

    fn visit(&mut self) -> Result<(), CacheGcFsError> {
        self.check()?;
        self.entries = self
            .entries
            .checked_add(1)
            .filter(|count| *count <= MAX_ENTRIES)
            .ok_or_else(|| {
                CacheGcFsError::new(
                    "entry_limit",
                    "environment cache filesystem operation exceeded its entry limit",
                )
            })?;
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Entry {
    name: OsString,
    c_name: CString,
    stat: libc::stat,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct TreeMeasure {
    bytes: u64,
    inodes: u64,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn io_error(code: &'static str, path: &Path, error: impl fmt::Display) -> CacheGcFsError {
    CacheGcFsError::new(code, format!("{}: {error}", path.display()))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn identity(stat: &libc::stat) -> CacheFsIdentity {
    CacheFsIdentity {
        device: fs::device_id(stat),
        inode: stat.st_ino,
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn has_bounded_file_size(size: libc::off_t, max_bytes: usize) -> bool {
    usize::try_from(size).is_ok_and(|size| size <= max_bytes)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn verify_same(
    expected: &libc::stat,
    actual: &libc::stat,
    path: &Path,
) -> Result<(), CacheGcFsError> {
    if fs::same_identity(expected, actual) {
        Ok(())
    } else {
        Err(CacheGcFsError::new(
            "identity_changed",
            format!(
                "{} changed during cache inspection or removal",
                path.display()
            ),
        ))
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn verify_same_object(
    expected: &libc::stat,
    actual: &libc::stat,
    path: &Path,
) -> Result<(), CacheGcFsError> {
    if identity(expected) == identity(actual) && fs::file_type(expected) == fs::file_type(actual) {
        Ok(())
    } else {
        Err(CacheGcFsError::new(
            "identity_changed",
            format!("{} was replaced during cache removal", path.display()),
        ))
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_cache_root(environment_root: &Path) -> Result<Option<CacheRoot>, CacheGcFsError> {
    match std::fs::symlink_metadata(environment_root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("root_unreadable", environment_root, error)),
        Ok(_) => {}
    }

    // Resolve the user-selected host-state spelling once, so configured
    // aliases (including macOS /var -> /private/var) are accepted. From this
    // boundary onward, every component is opened with O_NOFOLLOW.
    let canonical_environment_root = std::fs::canonicalize(environment_root)
        .map_err(|error| io_error("root_unreadable", environment_root, error))?;
    let root_fd = fs::open_absolute_directory_nofollow(&canonical_environment_root)
        .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
    let root_stat = fs::fstat(root_fd.as_raw_fd(), &canonical_environment_root)
        .map_err(|error| CacheGcFsError::new("root_unreadable", error.to_string()))?;
    if fs::file_type(&root_stat) != libc::S_IFDIR {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            "selected environment state root is not a directory",
        ));
    }
    let cache_name = CString::new("cache").expect("constant has no NUL");
    let cache_path = canonical_environment_root.join("cache");
    let cache_stat = match fs::stat_at(root_fd.as_raw_fd(), &cache_name, libc::AT_SYMLINK_NOFOLLOW)
    {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("root_unreadable", &cache_path, error)),
    };
    if fs::file_type(&cache_stat) != libc::S_IFDIR {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            format!("{} is not a non-symlink directory", cache_path.display()),
        ));
    }
    let cache_fd = fs::openat_directory(root_fd.as_raw_fd(), &cache_name, &cache_path)
        .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
    let opened_cache_stat = fs::fstat(cache_fd.as_raw_fd(), &cache_path)
        .map_err(|error| CacheGcFsError::new("root_unreadable", error.to_string()))?;
    verify_same(&cache_stat, &opened_cache_stat, &cache_path)?;
    let device = fs::device_id(&cache_stat);
    Ok(Some(CacheRoot {
        path: cache_path,
        fd: cache_fd,
        identity: identity(&cache_stat),
        device,
    }))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_log_file(
    cache_fd: libc::c_int,
    cache_path: &Path,
    name: &std::ffi::CStr,
    path: &Path,
    create: bool,
) -> Result<File, CacheGcFsError> {
    let before = match fs::stat_at(cache_fd, name, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => Some(stat),
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => None,
        Err(error) => return Err(io_error("log_unreadable", path, error)),
    };
    if let Some(before) = before.as_ref()
        && (fs::file_type(before) != libc::S_IFREG || before.st_nlink != 1 || before.st_size < 0)
    {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            format!(
                "{} is not a singly-linked regular prune log",
                path.display()
            ),
        ));
    }

    let mut flags =
        libc::O_RDWR | libc::O_APPEND | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    if create {
        flags |= libc::O_CREAT;
    }
    // SAFETY: `cache_fd` is the pinned cache directory and `name` is a single
    // NUL-terminated component. O_NOFOLLOW prevents opening a symlink target.
    let raw = unsafe { libc::openat(cache_fd, name.as_ptr(), flags, 0o600 as libc::mode_t) };
    if raw < 0 {
        return Err(io_error("log_unreadable", path, io::Error::last_os_error()));
    }
    // SAFETY: openat returned a new owned descriptor, transferred exactly
    // once into File.
    let file = unsafe { File::from_raw_fd(raw) };
    let opened = fs::fstat(file.as_raw_fd(), path)
        .map_err(|error| io_error("log_unreadable", path, error))?;
    if fs::file_type(&opened) != libc::S_IFREG || opened.st_nlink != 1 || opened.st_size < 0 {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            format!(
                "{} is not a singly-linked regular prune log",
                path.display()
            ),
        ));
    }
    if let Some(before) = before.as_ref() {
        verify_same(before, &opened, path)?;
    }
    verify_same_mount(cache_fd, cache_path, file.as_raw_fd(), path)?;
    let named = fs::stat_at(cache_fd, name, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("log_unreadable", path, error))?;
    verify_same(&opened, &named, path)?;
    Ok(file)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn open_prune_log(
    environment_root: &Path,
    expected_cache_path: &Path,
    expected_cache_identity: CacheFsIdentity,
    cap: u64,
) -> Result<CachePruneLog, CacheGcFsError> {
    let cache = open_cache_root(environment_root)?.ok_or_else(|| {
        CacheGcFsError::new("identity_changed", "environment cache root disappeared")
    })?;
    if cache.path != expected_cache_path || cache.identity != expected_cache_identity {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "environment cache root changed before opening the prune log",
        ));
    }
    let log_name = CString::new("prune.log").expect("constant has no NUL");
    let rotated_name = CString::new("prune.log.1").expect("constant has no NUL");
    let log_path = cache.path.join("prune.log");
    let rotated_path = cache.path.join("prune.log.1");

    let current_before =
        match fs::stat_at(cache.fd.as_raw_fd(), &log_name, libc::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => Some(stat),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error("log_unreadable", &log_path, error)),
        };
    if let Some(current_before) = current_before {
        if fs::file_type(&current_before) != libc::S_IFREG
            || current_before.st_nlink != 1
            || current_before.st_size < 0
        {
            return Err(CacheGcFsError::new(
                "unsafe_path",
                format!(
                    "{} is not a singly-linked regular prune log",
                    log_path.display()
                ),
            ));
        }
        if current_before.st_size as u64 > cap {
            let current_file = open_log_file(
                cache.fd.as_raw_fd(),
                &cache.path,
                &log_name,
                &log_path,
                false,
            )?;
            let opened_current = fs::fstat(current_file.as_raw_fd(), &log_path)
                .map_err(|error| io_error("log_unreadable", &log_path, error))?;
            verify_same(&current_before, &opened_current, &log_path)?;
            // SAFETY: fchmod changes the already-opened regular log, not a
            // path that could be redirected through a symlink.
            if unsafe { libc::fchmod(current_file.as_raw_fd(), 0o600) } < 0 {
                return Err(io_error(
                    "log_unreadable",
                    &log_path,
                    io::Error::last_os_error(),
                ));
            }
            let rotated_before = match fs::stat_at(
                cache.fd.as_raw_fd(),
                &rotated_name,
                libc::AT_SYMLINK_NOFOLLOW,
            ) {
                Ok(stat) => Some(stat),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(io_error("log_unreadable", &rotated_path, error)),
            };
            if let Some(rotated_before) = rotated_before {
                if fs::file_type(&rotated_before) != libc::S_IFREG || rotated_before.st_nlink != 1 {
                    return Err(CacheGcFsError::new(
                        "unsafe_path",
                        format!(
                            "{} is not a singly-linked regular prune log",
                            rotated_path.display()
                        ),
                    ));
                }
                let rotated_file =
                    fs::openat_regular(cache.fd.as_raw_fd(), &rotated_name, &rotated_path)
                        .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
                let opened_rotated = fs::fstat(rotated_file.as_raw_fd(), &rotated_path)
                    .map_err(|error| io_error("log_unreadable", &rotated_path, error))?;
                verify_same(&rotated_before, &opened_rotated, &rotated_path)?;
                verify_same_mount(
                    cache.fd.as_raw_fd(),
                    &cache.path,
                    rotated_file.as_raw_fd(),
                    &rotated_path,
                )?;
                let rotated_named = fs::stat_at(
                    cache.fd.as_raw_fd(),
                    &rotated_name,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
                .map_err(|error| io_error("log_unreadable", &rotated_path, error))?;
                verify_same(&rotated_before, &rotated_named, &rotated_path)?;
                // SAFETY: the name is one component in the pinned cache root;
                // unlinkat removes the old log entry without following links.
                if unsafe { libc::unlinkat(cache.fd.as_raw_fd(), rotated_name.as_ptr(), 0) } < 0 {
                    return Err(io_error(
                        "log_rotation_failed",
                        &rotated_path,
                        io::Error::last_os_error(),
                    ));
                }
            }
            let current_named =
                fs::stat_at(cache.fd.as_raw_fd(), &log_name, libc::AT_SYMLINK_NOFOLLOW)
                    .map_err(|error| io_error("log_unreadable", &log_path, error))?;
            verify_same(&current_before, &current_named, &log_path)?;
            // SAFETY: both names are single components relative to the pinned
            // cache root; renameat moves the log entry and never follows it.
            if unsafe {
                libc::renameat(
                    cache.fd.as_raw_fd(),
                    log_name.as_ptr(),
                    cache.fd.as_raw_fd(),
                    rotated_name.as_ptr(),
                )
            } < 0
            {
                return Err(io_error(
                    "log_rotation_failed",
                    &log_path,
                    io::Error::last_os_error(),
                ));
            }
            // SAFETY: the cache root descriptor is open and owns the renamed
            // directory entries whose rotation is being made durable.
            if unsafe { libc::fsync(cache.fd.as_raw_fd()) } < 0 {
                return Err(io_error(
                    "log_rotation_failed",
                    &rotated_path,
                    io::Error::last_os_error(),
                ));
            }
        }
    }

    let current = open_log_file(
        cache.fd.as_raw_fd(),
        &cache.path,
        &log_name,
        &log_path,
        true,
    )?;
    // SAFETY: fchmod changes the mode of the already-opened regular log file,
    // not a path that could be redirected through a symlink.
    if unsafe { libc::fchmod(current.as_raw_fd(), 0o600) } < 0 {
        return Err(io_error(
            "log_unreadable",
            &log_path,
            io::Error::last_os_error(),
        ));
    }

    let rotated = match fs::stat_at(
        cache.fd.as_raw_fd(),
        &rotated_name,
        libc::AT_SYMLINK_NOFOLLOW,
    ) {
        Ok(_) => {
            let file = open_log_file(
                cache.fd.as_raw_fd(),
                &cache.path,
                &rotated_name,
                &rotated_path,
                false,
            )?;
            // SAFETY: fchmod changes the already-opened regular log, not a
            // path that could be redirected through a symlink.
            if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } < 0 {
                return Err(io_error(
                    "log_unreadable",
                    &rotated_path,
                    io::Error::last_os_error(),
                ));
            }
            Some(file)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_error("log_unreadable", &rotated_path, error)),
    };
    drop(current);
    drop(rotated);
    Ok(CachePruneLog {
        cache_fd: cache.fd,
        environment_root: environment_root.to_path_buf(),
        cache_identity: cache.identity,
        cache_path: cache.path,
        cap,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_cache_root_path(
    environment_root: &Path,
    expected_cache_path: &Path,
    expected_cache_identity: CacheFsIdentity,
) -> Result<(), CacheGcFsError> {
    let cache = open_cache_root(environment_root)?.ok_or_else(|| {
        CacheGcFsError::new("identity_changed", "environment cache root disappeared")
    })?;
    if cache.path != expected_cache_path || cache.identity != expected_cache_identity {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "environment cache root changed while the prune log was open",
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn verify_cache_root_fd(
    cache_fd: libc::c_int,
    cache_path: &Path,
    expected_cache_identity: CacheFsIdentity,
) -> Result<(), CacheGcFsError> {
    let stat = fs::fstat(cache_fd, cache_path)
        .map_err(|error| io_error("root_unreadable", cache_path, error))?;
    if fs::file_type(&stat) != libc::S_IFDIR || identity(&stat) != expected_cache_identity {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "pinned environment cache root changed while pruning",
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn append_prune_log(
    cache_fd: libc::c_int,
    environment_root: &Path,
    cache_path: &Path,
    cache_identity: CacheFsIdentity,
    cap: u64,
    text: &str,
) -> Result<(), CacheGcFsError> {
    // The root check also applies the normal cap rotation through the selected
    // cache path. Actual writes remain anchored to the cache fd retained when
    // the prune sink was created.
    let _validated = open_prune_log(environment_root, cache_path, cache_identity, cap)?;
    verify_cache_root_fd(cache_fd, cache_path, cache_identity)?;
    let name = CString::new("prune.log").expect("constant has no NUL");
    let path = cache_path.join("prune.log");
    let mut file = open_log_file(cache_fd, cache_path, &name, &path, true)?;
    let before = fs::fstat(file.as_raw_fd(), &path)
        .map_err(|error| io_error("log_unreadable", &path, error))?;
    verify_same_mount(cache_fd, cache_path, file.as_raw_fd(), &path)?;
    file.write_all(text.as_bytes())
        .map_err(|error| io_error("log_write_failed", &path, error))?;
    let opened_after = fs::fstat(file.as_raw_fd(), &path)
        .map_err(|error| io_error("log_write_failed", &path, error))?;
    verify_same_object(&before, &opened_after, &path)?;
    let named_after = fs::stat_at(cache_fd, &name, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("log_write_failed", &path, error))?;
    verify_same(&opened_after, &named_after, &path)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_existing_log(
    cache_fd: libc::c_int,
    cache_path: &Path,
    name: &std::ffi::CStr,
) -> Result<Option<File>, CacheGcFsError> {
    let name_text = name.to_string_lossy();
    let path = cache_path.join(name_text.as_ref());
    let before = match fs::stat_at(cache_fd, name, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("log_unreadable", &path, error)),
    };
    if fs::file_type(&before) != libc::S_IFREG || before.st_nlink != 1 {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            format!(
                "{} is not a singly-linked regular prune log",
                path.display()
            ),
        ));
    }
    let file = fs::openat_regular(cache_fd, name, &path)
        .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
    let opened = fs::fstat(file.as_raw_fd(), &path)
        .map_err(|error| io_error("log_unreadable", &path, error))?;
    verify_same(&before, &opened, &path)?;
    verify_same_mount(cache_fd, cache_path, file.as_raw_fd(), &path)?;
    let named = fs::stat_at(cache_fd, name, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("log_unreadable", &path, error))?;
    verify_same(&opened, &named, &path)?;
    Ok(Some(file))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn tail_prune_log(
    cache_fd: libc::c_int,
    environment_root: &Path,
    cache_path: &Path,
    cache_identity: CacheFsIdentity,
    lines: usize,
) -> Result<Vec<String>, CacheGcFsError> {
    validate_cache_root_path(environment_root, cache_path, cache_identity)?;
    verify_cache_root_fd(cache_fd, cache_path, cache_identity)?;
    let current_name = CString::new("prune.log").expect("constant has no NUL");
    let rotated_name = CString::new("prune.log.1").expect("constant has no NUL");
    let mut current = open_existing_log(cache_fd, cache_path, &current_name)?;
    let mut rotated = open_existing_log(cache_fd, cache_path, &rotated_name)?;
    let mut collected = match current.as_mut() {
        Some(file) => read_log_tail(file, lines)
            .map_err(|error| io_error("log_read_failed", &cache_path.join("prune.log"), error))?,
        None => Vec::new(),
    };
    if collected.len() < lines
        && let Some(file) = rotated.as_mut()
    {
        let mut older = read_log_tail(file, lines - collected.len())
            .map_err(|error| io_error("log_read_failed", &cache_path.join("prune.log.1"), error))?;
        older.append(&mut collected);
        collected = older;
    }
    Ok(collected)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn open_prune_log(
    _environment_root: &Path,
    _expected_cache_path: &Path,
    _expected_cache_identity: CacheFsIdentity,
    _cap: u64,
) -> Result<CachePruneLog, CacheGcFsError> {
    Err(CacheGcFsError::new(
        "unsupported_platform",
        "safe environment-cache logs are supported only on macOS and Linux",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn c_name(name: &OsStr, path: &Path) -> Result<CString, CacheGcFsError> {
    CString::new(name.as_bytes()).map_err(|_| {
        CacheGcFsError::new(
            "unsafe_path",
            format!(
                "{} contains a NUL byte in a directory entry",
                path.display()
            ),
        )
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn read_names(
    fd: libc::c_int,
    path: &Path,
    budget: &Budget,
) -> Result<Vec<OsString>, CacheGcFsError> {
    budget.check()?;
    // SAFETY: `fd` is an open directory. `dup` returns a separate descriptor
    // for `fdopendir`, leaving the caller's OwnedFd untouched.
    let duplicate = unsafe { libc::dup(fd) };
    if duplicate < 0 {
        return Err(io_error(
            "directory_unreadable",
            path,
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: `duplicate` is a newly owned descriptor referring to a
    // directory; `fdopendir` takes ownership on success.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        // SAFETY: fdopendir failed and did not take ownership of `duplicate`.
        unsafe { libc::close(duplicate) };
        return Err(io_error(
            "directory_unreadable",
            path,
            std::io::Error::last_os_error(),
        ));
    }
    let mut names = Vec::new();
    let result = loop {
        if let Err(error) = budget.check() {
            break Err(error);
        }
        fs::set_errno(0);
        // SAFETY: `stream` is live and uniquely owned by this function until
        // closed below; readdir returns either null or a borrowed dirent.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let errno = fs::get_errno();
            if errno == 0 {
                break Ok(());
            }
            break Err(io_error(
                "directory_unreadable",
                path,
                std::io::Error::from_raw_os_error(errno),
            ));
        }
        // SAFETY: POSIX dirent names are NUL terminated and valid while the
        // stream remains open; the bytes are copied before the next readdir.
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if budget.entries.saturating_add(names.len()) >= MAX_ENTRIES {
            break Err(CacheGcFsError::new(
                "entry_limit",
                "environment cache filesystem operation exceeded its entry limit",
            ));
        }
        names.push(OsString::from_vec(name.to_vec()));
    };
    // SAFETY: this function owns the DIR stream and closes it exactly once.
    let close_result = unsafe { libc::closedir(stream) };
    if close_result < 0 && result.is_ok() {
        return Err(io_error(
            "directory_unreadable",
            path,
            std::io::Error::last_os_error(),
        ));
    }
    result?;
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    Ok(names)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn list_entries(
    fd: libc::c_int,
    path: &Path,
    device: u64,
    budget: &mut Budget,
) -> Result<Vec<Entry>, CacheGcFsError> {
    let names = read_names(fd, path, budget)?;
    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        budget.visit()?;
        let child_path = path.join(&name);
        let c_name = c_name(&name, &child_path)?;
        let stat = fs::stat_at(fd, &c_name, libc::AT_SYMLINK_NOFOLLOW)
            .map_err(|error| io_error("entry_unreadable", &child_path, error))?;
        if fs::device_id(&stat) != device {
            return Err(CacheGcFsError::new(
                "unsafe_path",
                format!("{} crosses onto another filesystem", child_path.display()),
            ));
        }
        entries.push(Entry { name, c_name, stat });
    }
    Ok(entries)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_child_directory(
    parent: libc::c_int,
    entry: &Entry,
    parent_path: &Path,
    device: u64,
) -> Result<OwnedFd, CacheGcFsError> {
    let path = parent_path.join(&entry.name);
    if fs::file_type(&entry.stat) != libc::S_IFDIR {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            format!("{} is not a directory", path.display()),
        ));
    }
    let child = fs::openat_directory(parent, &entry.c_name, &path)
        .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
    let opened = fs::fstat(child.as_raw_fd(), &path)
        .map_err(|error| CacheGcFsError::new("entry_unreadable", error.to_string()))?;
    verify_same(&entry.stat, &opened, &path)?;
    if fs::device_id(&opened) != device {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            format!("{} crosses onto another filesystem", path.display()),
        ));
    }
    verify_same_mount(parent, parent_path, child.as_raw_fd(), &path)?;
    Ok(child)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn optional_directory(
    parent: libc::c_int,
    parent_path: &Path,
    name: &str,
    device: u64,
) -> Result<Option<OwnedFd>, CacheGcFsError> {
    let c_name = CString::new(name).expect("constant has no NUL");
    let path = parent_path.join(name);
    let before = match fs::stat_at(parent, &c_name, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("entry_unreadable", &path, error)),
    };
    let entry = Entry {
        name: OsString::from(name),
        c_name,
        stat: before,
    };
    Ok(Some(open_child_directory(
        parent,
        &entry,
        parent_path,
        device,
    )?))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn utf8_name(entry: &Entry, parent: &Path) -> Result<String, CacheGcFsError> {
    entry.name.clone().into_string().map_err(|_| {
        CacheGcFsError::new(
            "unsafe_path",
            format!(
                "{} has a non-UTF-8 cache name",
                parent.join(&entry.name).display()
            ),
        )
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn is_staging(name: &OsStr) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension == "staging")
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompletionMarker {
    identity: CacheFsIdentity,
    modified: SystemTime,
    last_used_at_ms: Option<i64>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn completion_marker(
    fd: libc::c_int,
    path: &Path,
) -> Result<Option<CompletionMarker>, CacheGcFsError> {
    let name = CString::new(".complete").expect("constant has no NUL");
    let before = match fs::stat_at(fd, &name, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(io_error(
                "marker_unreadable",
                &path.join(".complete"),
                error,
            ));
        }
    };
    if fs::file_type(&before) != libc::S_IFREG {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            format!("{} is not a regular file", path.join(".complete").display()),
        ));
    }
    let marker = fs::openat_regular(fd, &name, &path.join(".complete"))
        .map_err(|error| CacheGcFsError::new("marker_unreadable", error.to_string()))?;
    let opened = fs::fstat(marker.as_raw_fd(), &path.join(".complete"))
        .map_err(|error| CacheGcFsError::new("marker_unreadable", error.to_string()))?;
    verify_same(&before, &opened, &path.join(".complete"))?;
    verify_same_mount(fd, path, marker.as_raw_fd(), &path.join(".complete"))?;
    let modified = marker
        .metadata()
        .and_then(|metadata| metadata.modified())
        .map_err(|error| io_error("marker_unreadable", &path.join(".complete"), error))?;
    let after = fs::stat_at(fd, &name, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("marker_unreadable", &path.join(".complete"), error))?;
    verify_same(&before, &after, &path.join(".complete"))?;
    Ok(Some(CompletionMarker {
        identity: identity(&before),
        modified,
        last_used_at_ms: modified
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_millis() as i64),
    }))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn last_used_at_ms(fd: libc::c_int, path: &Path) -> Result<Option<i64>, CacheGcFsError> {
    Ok(completion_marker(fd, path)?.and_then(|marker| marker.last_used_at_ms))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn invalidate_completion_marker(
    fd: libc::c_int,
    path: &Path,
    expected: Option<CompletionMarker>,
    budget: &mut Budget,
) -> Result<(), CacheGcFsError> {
    let marker_path = path.join(".complete");
    let marker = CString::new(".complete").expect("constant has no NUL");
    let before = match fs::stat_at(fd, &marker, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error("marker_unreadable", &marker_path, error)),
    };
    let Some(expected) = expected else {
        return Err(CacheGcFsError::new(
            "used_since_plan",
            "build became complete after deletion was planned",
        ));
    };
    if fs::file_type(&before) != libc::S_IFREG || identity(&before) != expected.identity {
        return Err(CacheGcFsError::new(
            "used_since_plan",
            "completion marker changed after deletion was planned",
        ));
    }
    budget.visit()?;
    let file = fs::openat_regular(fd, &marker, &marker_path)
        .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
    let opened = fs::fstat(file.as_raw_fd(), &marker_path)
        .map_err(|error| io_error("entry_unreadable", &marker_path, error))?;
    verify_same(&before, &opened, &marker_path)?;
    verify_same_mount(fd, path, file.as_raw_fd(), &marker_path)?;
    let modified = file
        .metadata()
        .and_then(|metadata| metadata.modified())
        .map_err(|error| io_error("marker_unreadable", &marker_path, error))?;
    if modified != expected.modified {
        return Err(CacheGcFsError::new(
            "used_since_plan",
            "completion marker was updated after deletion was planned",
        ));
    }
    let named = fs::stat_at(fd, &marker, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("entry_unreadable", &marker_path, error))?;
    verify_same(&before, &named, &marker_path)?;
    budget.check()?;
    // SAFETY: the marker name is one component relative to the pinned build
    // descriptor; unlinkat removes the marker itself without following it.
    if unsafe { libc::unlinkat(fd, marker.as_ptr(), 0) } < 0 {
        return Err(io_error(
            "delete_failed",
            &marker_path,
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: `fd` is the pinned build directory whose marker entry was just
    // removed; syncing it makes marker invalidation durable before payload
    // deletion starts.
    if unsafe { libc::fsync(fd) } < 0 {
        return Err(io_error(
            "marker_sync_failed",
            &marker_path,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn read_key(fd: libc::c_int, path: &Path) -> Result<Option<String>, CacheGcFsError> {
    let name = CString::new(".key").expect("constant has no NUL");
    let before = match fs::stat_at(fd, &name, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("metadata_unreadable", &path.join(".key"), error)),
    };
    if fs::file_type(&before) != libc::S_IFREG
        || before.st_size < 0
        || before.st_size as usize > MAX_KEY_BYTES
    {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            format!(
                "{} is not a bounded regular file",
                path.join(".key").display()
            ),
        ));
    }
    let file = fs::openat_regular(fd, &name, &path.join(".key"))
        .map_err(|error| CacheGcFsError::new("metadata_unreadable", error.to_string()))?;
    let opened = fs::fstat(file.as_raw_fd(), &path.join(".key"))
        .map_err(|error| CacheGcFsError::new("metadata_unreadable", error.to_string()))?;
    verify_same(&before, &opened, &path.join(".key"))?;
    verify_same_mount(fd, path, file.as_raw_fd(), &path.join(".key"))?;
    let mut bytes = Vec::with_capacity(before.st_size as usize);
    file.take((MAX_KEY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("metadata_unreadable", &path.join(".key"), error))?;
    if bytes.len() > MAX_KEY_BYTES {
        return Err(CacheGcFsError::new(
            "metadata_limit",
            format!(
                "{} exceeds the metadata size limit",
                path.join(".key").display()
            ),
        ));
    }
    let after = fs::stat_at(fd, &name, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("metadata_unreadable", &path.join(".key"), error))?;
    verify_same(&before, &after, &path.join(".key"))?;
    let key = String::from_utf8(bytes).map_err(|_| {
        CacheGcFsError::new(
            "metadata_invalid",
            format!("{} is not UTF-8", path.join(".key").display()),
        )
    })?;
    if key.is_empty() {
        return Err(CacheGcFsError::new(
            "metadata_invalid",
            format!("{} is empty", path.join(".key").display()),
        ));
    }
    Ok(Some(key))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn read_regular_file(
    directory_path: &Path,
    name: &str,
    max_bytes: usize,
    deadline: Option<Instant>,
) -> Result<Option<Vec<u8>>, CacheGcFsError> {
    let budget = Budget::new(deadline);
    budget.check()?;
    let directory = fs::open_absolute_directory_nofollow(directory_path)
        .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
    let name_c = CString::new(name)
        .map_err(|_| CacheGcFsError::new("unsafe_path", "file name contains NUL"))?;
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            "file name must be one normal path component",
        ));
    }
    let path = directory_path.join(name);
    let before = match fs::stat_at(directory.as_raw_fd(), &name_c, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("metadata_unreadable", &path, error)),
    };
    if fs::file_type(&before) != libc::S_IFREG
        || before.st_size < 0
        || before.st_size as usize > max_bytes
    {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            format!("{} is not a bounded regular file", path.display()),
        ));
    }
    let file = fs::openat_regular(directory.as_raw_fd(), &name_c, &path)
        .map_err(|error| CacheGcFsError::new("metadata_unreadable", error.to_string()))?;
    let opened = fs::fstat(file.as_raw_fd(), &path)
        .map_err(|error| CacheGcFsError::new("metadata_unreadable", error.to_string()))?;
    verify_same(&before, &opened, &path)?;
    verify_same_mount(
        directory.as_raw_fd(),
        directory_path,
        file.as_raw_fd(),
        &path,
    )?;
    let mut bytes = Vec::with_capacity(before.st_size as usize);
    file.take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("metadata_unreadable", &path, error))?;
    if bytes.len() > max_bytes {
        return Err(CacheGcFsError::new(
            "metadata_limit",
            format!("{} exceeds the metadata size limit", path.display()),
        ));
    }
    budget.check()?;
    let after = fs::stat_at(directory.as_raw_fd(), &name_c, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("metadata_unreadable", &path, error))?;
    verify_same(&before, &after, &path)?;
    Ok(Some(bytes))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn tree_measure(
    root_fd: &OwnedFd,
    root_path: &Path,
    root_stat: &libc::stat,
    device: u64,
    budget: &mut Budget,
) -> Result<TreeMeasure, CacheGcFsError> {
    fn walk(
        fd: libc::c_int,
        path: &Path,
        initial_stat: &libc::stat,
        depth: usize,
        device: u64,
        budget: &mut Budget,
        total: &mut TreeMeasure,
    ) -> Result<(), CacheGcFsError> {
        budget.check()?;
        if depth > MAX_DEPTH {
            return Err(CacheGcFsError::new(
                "depth_limit",
                format!("{} exceeds the cache scan depth limit", path.display()),
            ));
        }
        let entries = list_entries(fd, path, device, budget)?;
        for entry in entries {
            budget.check()?;
            total.inodes = total.inodes.checked_add(1).ok_or_else(|| {
                CacheGcFsError::new("size_overflow", "cache inode count overflowed")
            })?;
            let child_path = path.join(&entry.name);
            match fs::file_type(&entry.stat) {
                libc::S_IFDIR => {
                    let child = open_child_directory(fd, &entry, path, device)?;
                    walk(
                        child.as_raw_fd(),
                        &child_path,
                        &entry.stat,
                        depth + 1,
                        device,
                        budget,
                        total,
                    )?;
                    let after = fs::fstat(child.as_raw_fd(), &child_path).map_err(|error| {
                        CacheGcFsError::new("entry_unreadable", error.to_string())
                    })?;
                    verify_same(&entry.stat, &after, &child_path)?;
                    let named_after = fs::stat_at(fd, &entry.c_name, libc::AT_SYMLINK_NOFOLLOW)
                        .map_err(|error| io_error("entry_unreadable", &child_path, error))?;
                    verify_same(&entry.stat, &named_after, &child_path)?;
                }
                libc::S_IFREG => {
                    let file =
                        fs::openat_regular(fd, &entry.c_name, &child_path).map_err(|error| {
                            CacheGcFsError::new("entry_unreadable", error.to_string())
                        })?;
                    let opened = fs::fstat(file.as_raw_fd(), &child_path).map_err(|error| {
                        CacheGcFsError::new("entry_unreadable", error.to_string())
                    })?;
                    verify_same(&entry.stat, &opened, &child_path)?;
                    verify_same_mount(fd, path, file.as_raw_fd(), &child_path)?;
                    let named_after = fs::stat_at(fd, &entry.c_name, libc::AT_SYMLINK_NOFOLLOW)
                        .map_err(|error| io_error("entry_unreadable", &child_path, error))?;
                    verify_same(&entry.stat, &named_after, &child_path)?;
                    let size = u64::try_from(entry.stat.st_size).map_err(|_| {
                        CacheGcFsError::new("size_invalid", "cache entry has a negative size")
                    })?;
                    total.bytes = total.bytes.checked_add(size).ok_or_else(|| {
                        CacheGcFsError::new("size_overflow", "cache byte total overflowed")
                    })?;
                }
                // Symlinks and other leaf objects are counted by their own
                // metadata and are never opened or followed.
                _ => {
                    let named_after = fs::stat_at(fd, &entry.c_name, libc::AT_SYMLINK_NOFOLLOW)
                        .map_err(|error| io_error("entry_unreadable", &child_path, error))?;
                    verify_same(&entry.stat, &named_after, &child_path)?;
                    let size = u64::try_from(entry.stat.st_size).map_err(|_| {
                        CacheGcFsError::new("size_invalid", "cache entry has a negative size")
                    })?;
                    total.bytes = total.bytes.checked_add(size).ok_or_else(|| {
                        CacheGcFsError::new("size_overflow", "cache byte total overflowed")
                    })?;
                }
            }
        }
        let after = fs::fstat(fd, path)
            .map_err(|error| CacheGcFsError::new("entry_unreadable", error.to_string()))?;
        verify_same(initial_stat, &after, path)
    }

    if fs::file_type(root_stat) != libc::S_IFDIR {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            format!("{} is not a build directory", root_path.display()),
        ));
    }
    let mut total = TreeMeasure {
        bytes: 0,
        inodes: 0,
    };
    walk(
        root_fd.as_raw_fd(),
        root_path,
        root_stat,
        0,
        device,
        budget,
        &mut total,
    )?;
    Ok(total)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn list_builds(
    builds_fd: libc::c_int,
    builds_path: &Path,
    device: u64,
    budget: &mut Budget,
) -> Result<(Vec<BuildEntry>, u64), CacheGcFsError> {
    let mut result = Vec::new();
    let mut total_bytes = 0_u64;
    for repository in list_entries(builds_fd, builds_path, device, budget)? {
        let repository_name = utf8_name(&repository, builds_path)?;
        let repository_fd = open_child_directory(builds_fd, &repository, builds_path, device)?;
        let repository_path = builds_path.join(&repository_name);
        for component in list_entries(repository_fd.as_raw_fd(), &repository_path, device, budget)?
        {
            let component_name = utf8_name(&component, &repository_path)?;
            let component_fd = open_child_directory(
                repository_fd.as_raw_fd(),
                &component,
                &repository_path,
                device,
            )?;
            let component_path = repository_path.join(&component_name);
            for build in list_entries(component_fd.as_raw_fd(), &component_path, device, budget)? {
                if is_staging(&build.name) {
                    continue;
                }
                let digest = utf8_name(&build, &component_path)?;
                let build_fd = open_child_directory(
                    component_fd.as_raw_fd(),
                    &build,
                    &component_path,
                    device,
                )?;
                let build_path = component_path.join(&digest);
                let measure = tree_measure(&build_fd, &build_path, &build.stat, device, budget)?;
                let after = fs::stat_at(
                    component_fd.as_raw_fd(),
                    &build.c_name,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
                .map_err(|error| io_error("entry_unreadable", &build_path, error))?;
                verify_same(&build.stat, &after, &build_path)?;
                let last_used_at_ms = last_used_at_ms(build_fd.as_raw_fd(), &build_path)?;
                let key = read_key(build_fd.as_raw_fd(), &build_path)?;
                total_bytes = total_bytes.checked_add(measure.bytes).ok_or_else(|| {
                    CacheGcFsError::new("size_overflow", "host cache byte total overflowed")
                })?;
                result.push(BuildEntry {
                    repository: repository_name.clone(),
                    component: component_name.clone(),
                    digest,
                    bytes: measure.bytes,
                    inodes: measure.inodes,
                    last_used_at_ms,
                    key,
                    identity: identity(&build.stat),
                    path: build_path,
                });
            }
        }
    }
    Ok((result, total_bytes))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn count_templates(
    templates_fd: libc::c_int,
    templates_path: &Path,
    device: u64,
    budget: &mut Budget,
) -> Result<std::collections::BTreeMap<String, usize>, CacheGcFsError> {
    let mut counts = std::collections::BTreeMap::new();
    for repository in list_entries(templates_fd, templates_path, device, budget)? {
        let repository_name = utf8_name(&repository, templates_path)?;
        let repository_fd =
            open_child_directory(templates_fd, &repository, templates_path, device)?;
        let repository_path = templates_path.join(&repository_name);
        let mut count = 0_usize;
        for component in list_entries(repository_fd.as_raw_fd(), &repository_path, device, budget)?
        {
            let component_fd = open_child_directory(
                repository_fd.as_raw_fd(),
                &component,
                &repository_path,
                device,
            )?;
            let component_path = repository_path.join(&component.name);
            for marker in list_entries(component_fd.as_raw_fd(), &component_path, device, budget)? {
                let marker_path = component_path.join(&marker.name);
                let after = fs::stat_at(
                    component_fd.as_raw_fd(),
                    &marker.c_name,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
                .map_err(|error| io_error("entry_unreadable", &marker_path, error))?;
                verify_same(&marker.stat, &after, &marker_path)?;
                count = count.checked_add(1).ok_or_else(|| {
                    CacheGcFsError::new("size_overflow", "template entry count overflowed")
                })?;
            }
        }
        if count > 0 {
            counts.insert(repository_name, count);
        }
    }
    Ok(counts)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn list_templates(
    templates_fd: libc::c_int,
    templates_path: &Path,
    device: u64,
    budget: &mut Budget,
) -> Result<
    (
        Vec<TemplateEntry>,
        std::collections::BTreeMap<String, usize>,
    ),
    CacheGcFsError,
> {
    let mut entries = Vec::new();
    let mut counts = std::collections::BTreeMap::new();
    let mut total_marker_bytes = 0_usize;
    for repository in list_entries(templates_fd, templates_path, device, budget)? {
        let repository_name = utf8_name(&repository, templates_path)?;
        let repository_fd =
            open_child_directory(templates_fd, &repository, templates_path, device)?;
        let repository_path = templates_path.join(&repository_name);
        let mut count = 0_usize;
        for component in list_entries(repository_fd.as_raw_fd(), &repository_path, device, budget)?
        {
            let component_name = utf8_name(&component, &repository_path)?;
            let component_fd = open_child_directory(
                repository_fd.as_raw_fd(),
                &component,
                &repository_path,
                device,
            )?;
            let component_path = repository_path.join(&component_name);
            let component_stat = fs::fstat(component_fd.as_raw_fd(), &component_path)
                .map_err(|error| io_error("entry_unreadable", &component_path, error))?;
            for marker in list_entries(component_fd.as_raw_fd(), &component_path, device, budget)? {
                let marker_name = utf8_name(&marker, &component_path)?;
                let marker_path = component_path.join(&marker_name);
                if fs::file_type(&marker.stat) != libc::S_IFREG
                    || !has_bounded_file_size(marker.stat.st_size, MAX_TEMPLATE_MARKER_BYTES)
                {
                    return Err(CacheGcFsError::new(
                        "unsafe_path",
                        format!(
                            "{} is not a bounded regular template marker",
                            marker_path.display()
                        ),
                    ));
                }
                let file =
                    fs::openat_regular(component_fd.as_raw_fd(), &marker.c_name, &marker_path)
                        .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
                let opened = fs::fstat(file.as_raw_fd(), &marker_path)
                    .map_err(|error| io_error("entry_unreadable", &marker_path, error))?;
                verify_same(&marker.stat, &opened, &marker_path)?;
                verify_same_mount(
                    component_fd.as_raw_fd(),
                    &component_path,
                    file.as_raw_fd(),
                    &marker_path,
                )?;
                let mut bytes = Vec::with_capacity(marker.stat.st_size as usize);
                file.take((MAX_TEMPLATE_MARKER_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|error| io_error("metadata_unreadable", &marker_path, error))?;
                if bytes.len() > MAX_TEMPLATE_MARKER_BYTES {
                    return Err(CacheGcFsError::new(
                        "metadata_limit",
                        format!(
                            "{} exceeds the template marker size limit",
                            marker_path.display()
                        ),
                    ));
                }
                total_marker_bytes = total_marker_bytes
                    .checked_add(bytes.len())
                    .filter(|total| *total <= MAX_TEMPLATE_MARKER_TOTAL_BYTES)
                    .ok_or_else(|| {
                        CacheGcFsError::new(
                            "metadata_limit",
                            "template markers exceed the total inventory byte limit",
                        )
                    })?;
                let named_after = fs::stat_at(
                    component_fd.as_raw_fd(),
                    &marker.c_name,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
                .map_err(|error| io_error("metadata_unreadable", &marker_path, error))?;
                verify_same(&marker.stat, &named_after, &marker_path)?;
                budget.check()?;
                count = count.checked_add(1).ok_or_else(|| {
                    CacheGcFsError::new("size_overflow", "template entry count overflowed")
                })?;
                entries.push(TemplateEntry {
                    repository: repository_name.clone(),
                    component: component_name.clone(),
                    marker_name,
                    path: marker_path,
                    bytes,
                    parent_identity: identity(&component_stat),
                    identity: identity(&marker.stat),
                });
            }
        }
        if count > 0 {
            counts.insert(repository_name, count);
        }
    }
    Ok((entries, counts))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn inventory(
    environment_root: &Path,
    deadline: Option<Instant>,
) -> Result<Option<CacheInventory>, CacheGcFsError> {
    inventory_impl(environment_root, deadline, false, true)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn inventory_for_prune(
    environment_root: &Path,
    deadline: Option<Instant>,
    include_templates: bool,
) -> Result<Option<CacheInventory>, CacheGcFsError> {
    inventory_impl(
        environment_root,
        deadline,
        include_templates,
        include_templates,
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn inventory_impl(
    environment_root: &Path,
    deadline: Option<Instant>,
    include_templates: bool,
    collect_template_counts: bool,
) -> Result<Option<CacheInventory>, CacheGcFsError> {
    let mut budget = Budget::new(deadline);
    budget.check()?;
    let Some(cache) = open_cache_root(environment_root)? else {
        return Ok(None);
    };
    budget.check()?;
    let cache_entries = list_entries(cache.fd.as_raw_fd(), &cache.path, cache.device, &mut budget)?;
    for entry in &cache_entries {
        let name = entry.name.to_string_lossy();
        if name == "builds" || name == "templates" {
            continue;
        }
        if fs::file_type(&entry.stat) != libc::S_IFREG {
            return Err(CacheGcFsError::new(
                "unsafe_path",
                format!(
                    "unexpected cache root entry {}",
                    cache.path.join(&entry.name).display()
                ),
            ));
        }
    }
    let builds_fd = optional_directory(cache.fd.as_raw_fd(), &cache.path, "builds", cache.device)?;
    let templates_fd = if include_templates || collect_template_counts {
        optional_directory(cache.fd.as_raw_fd(), &cache.path, "templates", cache.device)?
    } else {
        None
    };
    let (builds, host_total_bytes) = if let Some(fd) = builds_fd {
        list_builds(
            fd.as_raw_fd(),
            &cache.path.join("builds"),
            cache.device,
            &mut budget,
        )?
    } else {
        (Vec::new(), 0)
    };
    let (templates, templates_by_repository) = if let Some(fd) = templates_fd {
        if include_templates {
            list_templates(
                fd.as_raw_fd(),
                &cache.path.join("templates"),
                cache.device,
                &mut budget,
            )?
        } else if collect_template_counts {
            (
                Vec::new(),
                count_templates(
                    fd.as_raw_fd(),
                    &cache.path.join("templates"),
                    cache.device,
                    &mut budget,
                )?,
            )
        } else {
            (Vec::new(), std::collections::BTreeMap::new())
        }
    } else {
        (Vec::new(), std::collections::BTreeMap::new())
    };
    budget.check()?;
    Ok(Some(CacheInventory {
        cache_root: cache.path,
        cache_identity: cache.identity,
        host_total_bytes,
        builds,
        templates,
        templates_by_repository,
    }))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
type OpenedBuildDirectory = (OwnedFd, libc::stat, OwnedFd, CString, PathBuf);

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_relative_directory(
    cache: &CacheRoot,
    relative: [&str; 3],
) -> Result<Option<OpenedBuildDirectory>, CacheGcFsError> {
    let builds_name = CString::new("builds").expect("constant has no NUL");
    let builds_path = cache.path.join("builds");
    let builds_stat = match fs::stat_at(
        cache.fd.as_raw_fd(),
        &builds_name,
        libc::AT_SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("entry_unreadable", &builds_path, error)),
    };
    let builds_entry = Entry {
        name: OsString::from("builds"),
        c_name: builds_name,
        stat: builds_stat,
    };
    let mut current = open_child_directory(
        cache.fd.as_raw_fd(),
        &builds_entry,
        &cache.path,
        cache.device,
    )?;
    let mut current_path = builds_path;
    let mut last_parent = None;
    let mut last_name = CString::new("").expect("constant has no NUL");
    let mut last_stat = None;
    for component in relative {
        let name = CString::new(component).map_err(|_| {
            CacheGcFsError::new("unsafe_path", "cache candidate component contains NUL")
        })?;
        let child_path = current_path.join(component);
        let stat = match fs::stat_at(current.as_raw_fd(), &name, libc::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error("entry_unreadable", &child_path, error)),
        };
        if fs::file_type(&stat) != libc::S_IFDIR {
            return Err(CacheGcFsError::new(
                "unsafe_path",
                format!("{} is not a directory", child_path.display()),
            ));
        }
        let entry = Entry {
            name: OsString::from(component),
            c_name: name.clone(),
            stat,
        };
        let child = open_child_directory(current.as_raw_fd(), &entry, &current_path, cache.device)?;
        last_parent = Some(current);
        last_name = name;
        last_stat = Some(stat);
        current = child;
        current_path = child_path;
    }
    let parent = last_parent.expect("three relative components include a parent");
    Ok(Some((
        current,
        last_stat.expect("three relative components include a final stat"),
        parent,
        last_name,
        current_path,
    )))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn remove_directory_contents(
    directory_fd: libc::c_int,
    path: &Path,
    initial_stat: &libc::stat,
    device: u64,
    budget: &mut Budget,
    preserve_gc_metadata: bool,
    depth: usize,
) -> Result<(), CacheGcFsError> {
    let initial = fs::fstat(directory_fd, path)
        .map_err(|error| CacheGcFsError::new("entry_unreadable", error.to_string()))?;
    verify_same_object(initial_stat, &initial, path)?;
    let names = read_names(directory_fd, path, budget)?;
    for name in names {
        if preserve_gc_metadata && name == ".key" {
            continue;
        }
        budget.visit()?;
        let child_path = path.join(&name);
        let c_name = c_name(&name, &child_path)?;
        let before = fs::stat_at(directory_fd, &c_name, libc::AT_SYMLINK_NOFOLLOW)
            .map_err(|error| io_error("entry_unreadable", &child_path, error))?;
        if fs::device_id(&before) != device {
            return Err(CacheGcFsError::new(
                "unsafe_path",
                format!("{} crosses onto another filesystem", child_path.display()),
            ));
        }
        if fs::file_type(&before) == libc::S_IFDIR {
            if depth >= MAX_DEPTH {
                return Err(CacheGcFsError::new(
                    "depth_limit",
                    format!(
                        "{} exceeds the cache removal depth limit",
                        child_path.display()
                    ),
                ));
            }
            let child = fs::openat_directory(directory_fd, &c_name, &child_path)
                .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
            let opened = fs::fstat(child.as_raw_fd(), &child_path)
                .map_err(|error| CacheGcFsError::new("entry_unreadable", error.to_string()))?;
            verify_same(&before, &opened, &child_path)?;
            verify_same_mount(directory_fd, path, child.as_raw_fd(), &child_path)?;
            remove_directory_contents(
                child.as_raw_fd(),
                &child_path,
                &before,
                device,
                budget,
                false,
                depth + 1,
            )?;
            let named_after = fs::stat_at(directory_fd, &c_name, libc::AT_SYMLINK_NOFOLLOW)
                .map_err(|error| io_error("entry_unreadable", &child_path, error))?;
            verify_same_object(&before, &named_after, &child_path)?;
            // SAFETY: parent is an open directory, `c_name` is a single
            // NUL-terminated component, and AT_REMOVEDIR never follows links.
            if unsafe { libc::unlinkat(directory_fd, c_name.as_ptr(), libc::AT_REMOVEDIR) } < 0 {
                return Err(io_error(
                    "delete_failed",
                    &child_path,
                    std::io::Error::last_os_error(),
                ));
            }
        } else {
            if fs::file_type(&before) == libc::S_IFREG {
                let file = fs::openat_regular(directory_fd, &c_name, &child_path)
                    .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
                let opened = fs::fstat(file.as_raw_fd(), &child_path)
                    .map_err(|error| CacheGcFsError::new("entry_unreadable", error.to_string()))?;
                verify_same(&before, &opened, &child_path)?;
                verify_same_mount(directory_fd, path, file.as_raw_fd(), &child_path)?;
            }
            let named_after = fs::stat_at(directory_fd, &c_name, libc::AT_SYMLINK_NOFOLLOW)
                .map_err(|error| io_error("entry_unreadable", &child_path, error))?;
            verify_same(&before, &named_after, &child_path)?;
            // SAFETY: unlinkat operates on one name relative to an open
            // directory and removes a symlink itself instead of following it.
            if unsafe { libc::unlinkat(directory_fd, c_name.as_ptr(), 0) } < 0 {
                return Err(io_error(
                    "delete_failed",
                    &child_path,
                    std::io::Error::last_os_error(),
                ));
            }
        }
    }
    let after = fs::fstat(directory_fd, path)
        .map_err(|error| CacheGcFsError::new("entry_unreadable", error.to_string()))?;
    verify_same_object(initial_stat, &after, path)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_template_parent(
    cache: &CacheRoot,
    repository: &str,
    component: &str,
) -> Result<Option<(OwnedFd, libc::stat, PathBuf)>, CacheGcFsError> {
    for name in [repository, component] {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\0')
        {
            return Err(CacheGcFsError::new(
                "unsafe_path",
                "template marker path is not component-safe",
            ));
        }
    }
    let Some(templates_fd) =
        optional_directory(cache.fd.as_raw_fd(), &cache.path, "templates", cache.device)?
    else {
        return Ok(None);
    };
    let templates_path = cache.path.join("templates");
    let repository_name = CString::new(repository)
        .map_err(|_| CacheGcFsError::new("unsafe_path", "template repository contains NUL"))?;
    let repository_path = templates_path.join(repository);
    let repository_stat = match fs::stat_at(
        templates_fd.as_raw_fd(),
        &repository_name,
        libc::AT_SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("entry_unreadable", &repository_path, error)),
    };
    let repository_entry = Entry {
        name: OsString::from(repository),
        c_name: repository_name,
        stat: repository_stat,
    };
    let repository_fd = open_child_directory(
        templates_fd.as_raw_fd(),
        &repository_entry,
        &templates_path,
        cache.device,
    )?;
    let component_name = CString::new(component)
        .map_err(|_| CacheGcFsError::new("unsafe_path", "template component contains NUL"))?;
    let component_path = repository_path.join(component);
    let component_stat = match fs::stat_at(
        repository_fd.as_raw_fd(),
        &component_name,
        libc::AT_SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("entry_unreadable", &component_path, error)),
    };
    let component_entry = Entry {
        name: OsString::from(component),
        c_name: component_name,
        stat: component_stat,
    };
    let component_fd = open_child_directory(
        repository_fd.as_raw_fd(),
        &component_entry,
        &repository_path,
        cache.device,
    )?;
    let opened = fs::fstat(component_fd.as_raw_fd(), &component_path)
        .map_err(|error| io_error("entry_unreadable", &component_path, error))?;
    verify_same(&component_stat, &opened, &component_path)?;
    Ok(Some((component_fd, component_stat, component_path)))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[allow(clippy::too_many_arguments)] // Keep each deletion-proof identity and hook explicit.
pub(crate) fn remove_template_marker(
    environment_root: &Path,
    expected_cache_path: &Path,
    expected_cache_identity: CacheFsIdentity,
    repository: &str,
    component: &str,
    marker_name: &str,
    expected_parent_identity: CacheFsIdentity,
    expected_marker_identity: CacheFsIdentity,
    expected_contents: &[u8],
    before_unlink: impl FnOnce(libc::c_int, &[u8]) -> Result<(), String>,
    deadline: Option<Instant>,
) -> Result<DeleteOutcome, CacheGcFsError> {
    let budget = Budget::new(deadline);
    budget.check()?;
    let cache = open_cache_root(environment_root)?.ok_or_else(|| {
        CacheGcFsError::new("identity_changed", "environment state root disappeared")
    })?;
    if cache.path != expected_cache_path || cache.identity != expected_cache_identity {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "environment cache root changed since template inventory",
        ));
    }
    let Some((parent_fd, parent_stat, parent_path)) =
        open_template_parent(&cache, repository, component)?
    else {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "template marker parent disappeared since inventory",
        ));
    };
    if identity(&parent_stat) != expected_parent_identity {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "template marker parent changed since inventory",
        ));
    }
    if marker_name.is_empty()
        || marker_name == "."
        || marker_name == ".."
        || marker_name.contains('/')
        || marker_name.contains('\0')
    {
        return Err(CacheGcFsError::new(
            "unsafe_path",
            "template marker name is not one component",
        ));
    }
    let name = CString::new(marker_name)
        .map_err(|_| CacheGcFsError::new("unsafe_path", "template marker name contains NUL"))?;
    let marker_path = parent_path.join(marker_name);
    let before = fs::stat_at(parent_fd.as_raw_fd(), &name, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("entry_unreadable", &marker_path, error))?;
    if fs::file_type(&before) != libc::S_IFREG
        || identity(&before) != expected_marker_identity
        || !has_bounded_file_size(before.st_size, MAX_TEMPLATE_MARKER_BYTES)
    {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "template marker identity or type changed since inventory",
        ));
    }
    let file = fs::openat_regular(parent_fd.as_raw_fd(), &name, &marker_path)
        .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
    let opened = fs::fstat(file.as_raw_fd(), &marker_path)
        .map_err(|error| io_error("entry_unreadable", &marker_path, error))?;
    verify_same(&before, &opened, &marker_path)?;
    verify_same_mount(
        parent_fd.as_raw_fd(),
        &parent_path,
        file.as_raw_fd(),
        &marker_path,
    )?;
    let mut bytes = Vec::with_capacity(before.st_size as usize);
    file.take((MAX_TEMPLATE_MARKER_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("metadata_unreadable", &marker_path, error))?;
    if bytes.len() > MAX_TEMPLATE_MARKER_BYTES || bytes != expected_contents {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "template marker contents changed since inventory",
        ));
    }
    let named = fs::stat_at(parent_fd.as_raw_fd(), &name, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("entry_unreadable", &marker_path, error))?;
    verify_same(&before, &named, &marker_path)?;
    budget.check()?;
    before_unlink(parent_fd.as_raw_fd(), &bytes)
        .map_err(|error| CacheGcFsError::new("template_destroy_failed", error))?;
    // The hook has its own configured timeout. Start a fresh bounded
    // filesystem budget for the identity recheck and marker unlink.
    let budget = Budget::new(None);
    budget.check()?;

    // The hook has succeeded. If it removed the marker itself, sync its pinned
    // parent and report completion; otherwise remove only the original file.
    let after_hook = match fs::stat_at(parent_fd.as_raw_fd(), &name, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // SAFETY: `parent_fd` is the pinned component directory whose
            // marker entry the successful hook removed.
            if unsafe { libc::fsync(parent_fd.as_raw_fd()) } < 0 {
                return Err(io_error(
                    "marker_sync_failed",
                    &marker_path,
                    io::Error::last_os_error(),
                ));
            }
            return Ok(DeleteOutcome::Removed);
        }
        Err(error) => return Err(io_error("entry_unreadable", &marker_path, error)),
    };
    if fs::file_type(&after_hook) != libc::S_IFREG
        || identity(&after_hook) != expected_marker_identity
        || !has_bounded_file_size(after_hook.st_size, MAX_TEMPLATE_MARKER_BYTES)
    {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "template marker was replaced while template_destroy ran; replacement retained",
        ));
    }
    let after_file = fs::openat_regular(parent_fd.as_raw_fd(), &name, &marker_path)
        .map_err(|error| CacheGcFsError::new("unsafe_path", error.to_string()))?;
    let opened_after = fs::fstat(after_file.as_raw_fd(), &marker_path)
        .map_err(|error| io_error("entry_unreadable", &marker_path, error))?;
    verify_same(&after_hook, &opened_after, &marker_path)?;
    verify_same_mount(
        parent_fd.as_raw_fd(),
        &parent_path,
        after_file.as_raw_fd(),
        &marker_path,
    )?;
    let mut after_bytes = Vec::with_capacity(after_hook.st_size.max(0) as usize);
    after_file
        .take((MAX_TEMPLATE_MARKER_BYTES + 1) as u64)
        .read_to_end(&mut after_bytes)
        .map_err(|error| io_error("metadata_unreadable", &marker_path, error))?;
    if after_bytes != bytes {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "template marker contents changed while template_destroy ran; replacement retained",
        ));
    }
    let named_after = fs::stat_at(parent_fd.as_raw_fd(), &name, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("entry_unreadable", &marker_path, error))?;
    verify_same(&after_hook, &named_after, &marker_path)?;
    // SAFETY: `name` is one component relative to the pinned component
    // directory; unlinkat removes the marker itself without following links.
    if unsafe { libc::unlinkat(parent_fd.as_raw_fd(), name.as_ptr(), 0) } < 0 {
        return Err(io_error(
            "delete_failed",
            &marker_path,
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: `parent_fd` is the pinned component directory whose marker was
    // just removed; syncing makes that removal durable.
    if unsafe { libc::fsync(parent_fd.as_raw_fd()) } < 0 {
        return Err(io_error(
            "marker_sync_failed",
            &marker_path,
            io::Error::last_os_error(),
        ));
    }
    Ok(DeleteOutcome::Removed)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[allow(clippy::too_many_arguments)] // Keep each deletion-proof identity and hook explicit.
pub(crate) fn remove_template_marker(
    _environment_root: &Path,
    _expected_cache_path: &Path,
    _expected_cache_identity: CacheFsIdentity,
    _repository: &str,
    _component: &str,
    _marker_name: &str,
    _expected_parent_identity: CacheFsIdentity,
    _expected_marker_identity: CacheFsIdentity,
    _expected_contents: &[u8],
    _before_unlink: impl FnOnce(libc::c_int, &[u8]) -> Result<(), String>,
    _deadline: Option<Instant>,
) -> Result<DeleteOutcome, CacheGcFsError> {
    Err(CacheGcFsError::new(
        "unsupported_platform",
        "safe environment-cache template removal is supported only on macOS and Linux",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[allow(clippy::too_many_arguments)] // Keep each deletion-proof identity and hook explicit.
pub(crate) fn remove_build(
    environment_root: &Path,
    expected_cache_path: &Path,
    expected_cache_identity: CacheFsIdentity,
    repository: &str,
    component: &str,
    digest: &str,
    expected_build_identity: CacheFsIdentity,
    expected_key: Option<&str>,
    expected_last_used_at_ms: Option<i64>,
    deletion_started: bool,
    before_delete: impl FnOnce() -> Result<(), String>,
    deadline: Option<Instant>,
) -> Result<DeleteOutcome, CacheGcFsError> {
    remove_build_with_marker_hook(
        environment_root,
        expected_cache_path,
        expected_cache_identity,
        repository,
        component,
        digest,
        expected_build_identity,
        expected_key,
        expected_last_used_at_ms,
        deletion_started,
        before_delete,
        || Ok(()),
        deadline,
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[allow(clippy::too_many_arguments)] // Keep each deletion-proof identity and hook explicit.
fn remove_build_with_marker_hook(
    environment_root: &Path,
    expected_cache_path: &Path,
    expected_cache_identity: CacheFsIdentity,
    repository: &str,
    component: &str,
    digest: &str,
    expected_build_identity: CacheFsIdentity,
    expected_key: Option<&str>,
    expected_last_used_at_ms: Option<i64>,
    deletion_started: bool,
    before_delete: impl FnOnce() -> Result<(), String>,
    after_marker_invalidated: impl FnOnce() -> Result<(), String>,
    deadline: Option<Instant>,
) -> Result<DeleteOutcome, CacheGcFsError> {
    let mut budget = Budget::new(deadline);
    budget.check()?;
    let cache = open_cache_root(environment_root)?.ok_or_else(|| {
        CacheGcFsError::new("identity_changed", "environment state root disappeared")
    })?;
    if cache.path != expected_cache_path || cache.identity != expected_cache_identity {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "environment cache root changed since the plan",
        ));
    }
    let relative = [repository, component, digest];
    for name in relative {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\0')
        {
            return Err(CacheGcFsError::new(
                "unsafe_path",
                "cache candidate contains an invalid path component",
            ));
        }
    }
    let Some((build_fd, build_stat, parent_fd, leaf_name, build_path)) =
        open_relative_directory(&cache, relative)?
    else {
        return Ok(DeleteOutcome::Gone);
    };
    if identity(&build_stat) != expected_build_identity
        || fs::file_type(&build_stat) != libc::S_IFDIR
    {
        return Err(CacheGcFsError::new(
            "identity_changed",
            "build directory identity changed since the plan",
        ));
    }
    let named_before = fs::stat_at(parent_fd.as_raw_fd(), &leaf_name, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("entry_unreadable", &build_path, error))?;
    verify_same(&build_stat, &named_before, &build_path)?;

    let observed_key = read_key(build_fd.as_raw_fd(), &build_path)?;
    let key_is_expected = observed_key.as_deref() == expected_key;
    let key_removed_by_prior_attempt = deletion_started && observed_key.is_none();
    if !key_is_expected && !key_removed_by_prior_attempt {
        return Err(CacheGcFsError::new(
            "key_changed",
            "build cache key changed since the plan",
        ));
    }
    let observed_marker = completion_marker(build_fd.as_raw_fd(), &build_path)?;
    let observed_last_used = observed_marker.map(|marker| marker.last_used_at_ms);
    let last_used_is_expected = observed_last_used == Some(expected_last_used_at_ms);
    if (deletion_started && observed_marker.is_some())
        || (!deletion_started && !last_used_is_expected)
    {
        return Err(CacheGcFsError::new(
            "used_since_plan",
            "build was used or became complete after deletion was planned",
        ));
    }
    if !deletion_started {
        before_delete().map_err(|error| CacheGcFsError::new("journal_write_failed", error))?;
        invalidate_completion_marker(
            build_fd.as_raw_fd(),
            &build_path,
            observed_marker,
            &mut budget,
        )?;
        after_marker_invalidated()
            .map_err(|error| CacheGcFsError::new("deletion_interrupted", error))?;
    }
    remove_directory_contents(
        build_fd.as_raw_fd(),
        &build_path,
        &build_stat,
        cache.device,
        &mut budget,
        true,
        0,
    )?;

    // Keep the key until every other child is gone. The completion marker was
    // invalidated before payload deletion so readers cannot consume a partial
    // output; a resumed deletion refuses if a marker has reappeared.
    let key_name = CString::new(".key").expect("constant has no NUL");
    let key_path = build_path.join(".key");
    match fs::stat_at(build_fd.as_raw_fd(), &key_name, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(key_stat) => {
            budget.visit()?;
            if fs::file_type(&key_stat) != libc::S_IFREG {
                return Err(CacheGcFsError::new(
                    "unsafe_path",
                    format!("{} is not a regular cache key", key_path.display()),
                ));
            }
            if read_key(build_fd.as_raw_fd(), &build_path)?.as_deref() != expected_key {
                return Err(CacheGcFsError::new(
                    "key_changed",
                    "build cache key changed during removal",
                ));
            }
            let named_key = fs::stat_at(build_fd.as_raw_fd(), &key_name, libc::AT_SYMLINK_NOFOLLOW)
                .map_err(|error| io_error("entry_unreadable", &key_path, error))?;
            verify_same(&key_stat, &named_key, &key_path)?;
            budget.check()?;
            // SAFETY: the cache-key name is one component relative to the
            // pinned build descriptor; unlinkat does not follow symlinks.
            if unsafe { libc::unlinkat(build_fd.as_raw_fd(), key_name.as_ptr(), 0) } < 0 {
                return Err(io_error(
                    "delete_failed",
                    &key_path,
                    std::io::Error::last_os_error(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && deletion_started => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && expected_key.is_none() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CacheGcFsError::new(
                "key_changed",
                "build cache key disappeared during removal",
            ));
        }
        Err(error) => return Err(io_error("entry_unreadable", &key_path, error)),
    }

    budget.visit()?;
    let named_after = fs::stat_at(parent_fd.as_raw_fd(), &leaf_name, libc::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| io_error("entry_unreadable", &build_path, error))?;
    verify_same_object(&build_stat, &named_after, &build_path)?;
    // SAFETY: the build's parent is pinned, leaf_name is one component, and
    // AT_REMOVEDIR refuses links and non-empty replacement directories.
    if unsafe {
        libc::unlinkat(
            parent_fd.as_raw_fd(),
            leaf_name.as_ptr(),
            libc::AT_REMOVEDIR,
        )
    } < 0
    {
        return Err(io_error(
            "delete_failed",
            &build_path,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(DeleteOutcome::Removed)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn inventory(
    _environment_root: &Path,
    _deadline: Option<Instant>,
) -> Result<Option<CacheInventory>, CacheGcFsError> {
    Err(CacheGcFsError::new(
        "unsupported_platform",
        "safe environment-cache inventory is supported only on macOS and Linux",
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn inventory_for_prune(
    _environment_root: &Path,
    _deadline: Option<Instant>,
    _include_templates: bool,
) -> Result<Option<CacheInventory>, CacheGcFsError> {
    Err(CacheGcFsError::new(
        "unsupported_platform",
        "safe environment-cache inventory is supported only on macOS and Linux",
    ))
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    fn build_tree(root: &Path) -> PathBuf {
        let entry = root.join("cache/builds/repo/component/digest");
        std::fs::create_dir_all(entry.join("output")).unwrap();
        std::fs::write(entry.join(".key"), "cache-lock-key").unwrap();
        std::fs::write(entry.join(".complete"), b"").unwrap();
        std::fs::write(entry.join("output/app.bin"), b"payload").unwrap();
        entry
    }

    #[test]
    fn inventory_refuses_a_symlinked_hierarchy_before_proposing_any_work() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        std::fs::create_dir_all(root.join("cache/builds")).unwrap();
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"leave me").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("cache/builds/repo")).unwrap();

        let error = inventory(&root, None).unwrap_err();
        assert_eq!(error.code, "unsafe_path");
        assert_eq!(
            std::fs::read(outside.join("sentinel")).unwrap(),
            b"leave me"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inventory_and_removal_refuse_same_device_bind_mounts() {
        use std::os::unix::fs::MetadataExt as _;

        struct BindMount(CString);
        impl Drop for BindMount {
            fn drop(&mut self) {
                // SAFETY: the CString is a valid NUL-terminated target path
                // for the bind mount created by this test.
                unsafe { libc::umount2(self.0.as_ptr(), 0) };
            }
        }

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let entry_path = build_tree(&root);
        let inventory_before_mount = inventory(&root, None).unwrap().unwrap();
        let candidate = inventory_before_mount.builds.first().unwrap().clone();
        let outside = temporary.path().join("outside");
        let mountpoint = entry_path.join("output/mounted");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir(&mountpoint).unwrap();
        std::fs::write(outside.join("sentinel"), b"leave me").unwrap();
        assert_eq!(
            std::fs::metadata(&outside).unwrap().dev(),
            std::fs::metadata(&mountpoint).unwrap().dev(),
            "the bind mount fixture must share st_dev with the cache"
        );
        let source = CString::new(outside.as_os_str().as_bytes()).unwrap();
        let target = CString::new(mountpoint.as_os_str().as_bytes()).unwrap();
        // SAFETY: source and target are stable NUL-terminated absolute paths;
        // MS_BIND creates the same-filesystem mount boundary under the cache.
        if unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        } < 0
        {
            let error = std::io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EACCES)) {
                eprintln!("skipping bind-mount regression without mount permission: {error}");
                return;
            }
            panic!("could not create same-device bind mount fixture: {error}");
        }
        let _mount = BindMount(target);

        let inventory_error = inventory(&root, None).unwrap_err();
        assert_eq!(inventory_error.code, "unsafe_path");

        let removal_error = remove_build(
            &root,
            &inventory_before_mount.cache_root,
            inventory_before_mount.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            candidate.last_used_at_ms,
            false,
            || Ok(()),
            None,
        )
        .unwrap_err();
        assert_eq!(removal_error.code, "unsafe_path");
        assert!(!entry_path.join(".complete").exists());
        assert_eq!(
            std::fs::read(outside.join("sentinel")).unwrap(),
            b"leave me"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inventory_refuses_a_bind_mounted_file_inside_a_build() {
        use std::os::unix::fs::MetadataExt as _;

        struct BindMount(CString);
        impl Drop for BindMount {
            fn drop(&mut self) {
                // SAFETY: the CString is a valid NUL-terminated target path
                // for the bind mount created by this test.
                unsafe { libc::umount2(self.0.as_ptr(), 0) };
            }
        }

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let entry_path = build_tree(&root);
        let inventory_before_mount = inventory(&root, None).unwrap().unwrap();
        let candidate = inventory_before_mount.builds.first().unwrap().clone();
        let outside = temporary.path().join("outside.bin");
        let mountpoint = entry_path.join("output/mounted.bin");
        std::fs::write(&outside, b"leave me").unwrap();
        std::fs::write(&mountpoint, b"hidden target").unwrap();
        assert_eq!(
            std::fs::metadata(&outside).unwrap().dev(),
            std::fs::metadata(&mountpoint).unwrap().dev(),
            "the bind mount fixture must share st_dev with the cache"
        );
        let source = CString::new(outside.as_os_str().as_bytes()).unwrap();
        let target = CString::new(mountpoint.as_os_str().as_bytes()).unwrap();
        // SAFETY: source and target are stable NUL-terminated absolute paths;
        // MS_BIND creates the same-filesystem mount boundary under the cache.
        if unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        } < 0
        {
            let error = std::io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EACCES)) {
                eprintln!("skipping file bind-mount regression without mount permission: {error}");
                return;
            }
            panic!("could not create same-device file bind mount fixture: {error}");
        }
        let _mount = BindMount(target);

        let inventory_error = inventory(&root, None).unwrap_err();
        assert_eq!(inventory_error.code, "unsafe_path");

        let removal_error = remove_build(
            &root,
            &inventory_before_mount.cache_root,
            inventory_before_mount.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            candidate.last_used_at_ms,
            false,
            || Ok(()),
            None,
        )
        .unwrap_err();
        assert_eq!(removal_error.code, "unsafe_path");
        assert!(!entry_path.join(".complete").exists());
        assert_eq!(std::fs::read(&outside).unwrap(), b"leave me");
    }

    #[test]
    fn bounded_metadata_reader_refuses_a_leaf_symlink() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("environment");
        std::fs::create_dir_all(&directory).unwrap();
        let outside = temporary.path().join("outside.json");
        std::fs::write(&outside, b"{\"entries\":[]}").unwrap();
        std::os::unix::fs::symlink(&outside, directory.join("cache-uses.json")).unwrap();

        let error = read_regular_file(&directory, "cache-uses.json", 1024, None).unwrap_err();

        assert_eq!(error.code, "unsafe_path");
        assert_eq!(std::fs::read(&outside).unwrap(), b"{\"entries\":[]}");
    }

    #[test]
    fn removal_unlinks_a_leaf_symlink_without_following_its_target() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let entry_path = build_tree(&root);
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"leave me").unwrap();
        std::os::unix::fs::symlink(outside.join("sentinel"), entry_path.join("output/link"))
            .unwrap();
        let inventory = inventory(&root, None).unwrap().unwrap();
        let candidate = inventory
            .builds
            .iter()
            .find(|entry| entry.repository == "repo")
            .unwrap();
        let mut journal_started = false;

        let removed = remove_build(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            candidate.last_used_at_ms,
            false,
            || {
                journal_started = true;
                Ok(())
            },
            None,
        )
        .unwrap();

        assert_eq!(removed, DeleteOutcome::Removed);
        assert!(journal_started);
        assert_eq!(
            std::fs::read(outside.join("sentinel")).unwrap(),
            b"leave me"
        );
        assert!(!entry_path.exists());
    }

    #[test]
    fn removal_refuses_a_replaced_ancestor_without_traversing_its_symlink() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        build_tree(&root);
        let inventory = inventory(&root, None).unwrap().unwrap();
        let candidate = inventory.builds.first().unwrap();
        let builds = root.join("cache/builds");
        let repo_path = builds.join("repo");
        let saved_repo = builds.join("repo-saved");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"leave me").unwrap();
        std::fs::rename(&repo_path, &saved_repo).unwrap();
        std::os::unix::fs::symlink(&outside, &repo_path).unwrap();

        let error = remove_build(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            candidate.last_used_at_ms,
            false,
            || Ok(()),
            None,
        )
        .unwrap_err();

        assert_eq!(error.code, "unsafe_path");
        assert_eq!(
            std::fs::read(saved_repo.join("component/digest/output/app.bin")).unwrap(),
            b"payload"
        );
        assert!(
            std::fs::symlink_metadata(&repo_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(outside.join("sentinel")).unwrap(),
            b"leave me"
        );
    }

    #[test]
    fn removal_refuses_a_replaced_candidate_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let entry_path = build_tree(&root);
        let inventory = inventory(&root, None).unwrap().unwrap();
        let candidate = inventory.builds.first().unwrap();
        let replacement = entry_path.with_file_name("replacement");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(replacement.join("sentinel"), b"leave me").unwrap();
        std::fs::remove_dir_all(&entry_path).unwrap();
        std::fs::rename(&replacement, &entry_path).unwrap();

        let error = remove_build(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            candidate.last_used_at_ms,
            false,
            || Ok(()),
            None,
        )
        .unwrap_err();

        assert_eq!(error.code, "identity_changed");
        assert_eq!(
            std::fs::read(entry_path.join("sentinel")).unwrap(),
            b"leave me"
        );
    }

    #[test]
    fn removal_resumes_after_the_final_key_was_unlinked() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let entry_path = build_tree(&root);
        let inventory = inventory(&root, None).unwrap().unwrap();
        let candidate = inventory.builds.first().unwrap();
        let last_used = candidate.last_used_at_ms;
        std::fs::remove_file(entry_path.join("output/app.bin")).unwrap();
        std::fs::remove_file(entry_path.join(".key")).unwrap();
        std::fs::remove_file(entry_path.join(".complete")).unwrap();

        let removed = remove_build(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            last_used,
            true,
            || panic!("a resumed deletion must not rewrite its start receipt"),
            None,
        )
        .unwrap();

        assert_eq!(removed, DeleteOutcome::Removed);
        assert!(!entry_path.exists());
    }

    #[test]
    fn missing_completion_marker_cannot_start_deletion_with_an_unknown_timestamp() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let entry_path = build_tree(&root);
        let inventory = inventory(&root, None).unwrap().unwrap();
        let candidate = inventory.builds.first().unwrap();
        std::fs::remove_file(entry_path.join(".complete")).unwrap();

        let refused = remove_build(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            None,
            false,
            || panic!("missing completion evidence must not authorize a deletion receipt"),
            None,
        )
        .unwrap_err();

        assert_eq!(refused.code, "used_since_plan");
        assert!(entry_path.join("output/app.bin").exists());
    }

    #[test]
    fn interrupted_payload_removal_keeps_marker_invalidated_and_rejects_recompletion() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let entry_path = build_tree(&root);
        let inventory = inventory(&root, None).unwrap().unwrap();
        let candidate = inventory.builds.first().unwrap();

        let interrupted = remove_build_with_marker_hook(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            candidate.last_used_at_ms,
            false,
            || Ok(()),
            || Err("simulated interruption after marker invalidation".into()),
            None,
        )
        .unwrap_err();

        assert_eq!(interrupted.code, "deletion_interrupted");
        assert!(!entry_path.join(".complete").exists());
        assert!(entry_path.join("output/app.bin").exists());

        std::fs::write(entry_path.join(".complete"), b"").unwrap();
        let reused = remove_build(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            candidate.last_used_at_ms,
            true,
            || panic!("a resumed deletion must not rewrite its start receipt"),
            None,
        )
        .unwrap_err();
        assert_eq!(reused.code, "used_since_plan");
        assert!(entry_path.join("output/app.bin").exists());

        std::fs::remove_file(entry_path.join(".complete")).unwrap();
        let removed = remove_build(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            candidate.last_used_at_ms,
            true,
            || panic!("a resumed deletion must not rewrite its start receipt"),
            None,
        )
        .unwrap();

        assert_eq!(removed, DeleteOutcome::Removed);
        assert!(!entry_path.exists());
    }

    #[test]
    fn resumed_deletion_refuses_a_replacement_build_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let entry_path = build_tree(&root);
        let inventory = inventory(&root, None).unwrap().unwrap();
        let candidate = inventory.builds.first().unwrap().clone();

        let interrupted = remove_build_with_marker_hook(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            candidate.last_used_at_ms,
            false,
            || Ok(()),
            || Err("simulated interruption after marker invalidation".into()),
            None,
        )
        .unwrap_err();
        assert_eq!(interrupted.code, "deletion_interrupted");

        let replacement_path = entry_path.with_file_name("replacement");
        std::fs::create_dir_all(replacement_path.join("output")).unwrap();
        std::fs::write(replacement_path.join(".key"), "cache-lock-key").unwrap();
        std::fs::write(replacement_path.join(".complete"), b"").unwrap();
        std::fs::write(replacement_path.join("output/app.bin"), b"replacement").unwrap();
        let old_path = entry_path.with_file_name("original");
        std::fs::rename(&entry_path, &old_path).unwrap();
        std::fs::rename(&replacement_path, &entry_path).unwrap();
        let replacement = remove_build(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            candidate.last_used_at_ms,
            true,
            || panic!("a resumed deletion must not rewrite its start receipt"),
            None,
        )
        .unwrap_err();

        assert_eq!(replacement.code, "identity_changed");
        assert!(entry_path.join(".complete").exists());
        assert!(entry_path.join("output/app.bin").exists());
    }

    #[test]
    fn a_failed_journal_start_receipt_deletes_nothing() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let entry_path = build_tree(&root);
        let inventory = inventory(&root, None).unwrap().unwrap();
        let candidate = inventory.builds.first().unwrap();
        let error = remove_build(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &candidate.repository,
            &candidate.component,
            &candidate.digest,
            candidate.identity,
            candidate.key.as_deref(),
            candidate.last_used_at_ms,
            false,
            || Err("journal refused".into()),
            None,
        )
        .unwrap_err();

        assert_eq!(error.code, "journal_write_failed");
        assert!(entry_path.join(".key").exists());
        assert!(entry_path.join(".complete").exists());
        assert!(entry_path.join("output/app.bin").exists());
    }

    #[test]
    fn entry_budget_refuses_the_next_path_before_inventory_continues() {
        let mut budget = Budget {
            deadline: Instant::now() + Duration::from_secs(1),
            entries: MAX_ENTRIES,
        };

        assert_eq!(budget.visit().unwrap_err().code, "entry_limit");
    }

    #[test]
    fn an_expired_inventory_deadline_withholds_the_scan() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let _entry = build_tree(&root);
        let error = inventory(&root, Some(Instant::now())).unwrap_err();
        assert_eq!(error.code, "deadline_exceeded");
    }

    #[test]
    fn prune_log_rotates_and_tails_through_the_pinned_cache_root() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        std::fs::create_dir_all(root.join("cache")).unwrap();
        let cache = open_cache_root(&root).unwrap().unwrap();
        let mut log = open_prune_log(&root, &cache.path, cache.identity, 1).unwrap();

        log.append("old line\n").unwrap();
        log.append("new line\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(cache.path.join("prune.log")).unwrap(),
            "new line\n"
        );
        assert_eq!(
            std::fs::read_to_string(cache.path.join("prune.log.1")).unwrap(),
            "old line\n"
        );
        assert_eq!(log.tail(2).unwrap(), ["old line", "new line"]);
    }

    #[test]
    fn prune_log_refuses_a_symlink_without_writing_its_target() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let cache_path = root.join("cache");
        std::fs::create_dir_all(&cache_path).unwrap();
        let outside = temporary.path().join("outside.log");
        std::fs::write(&outside, b"keep me").unwrap();
        std::os::unix::fs::symlink(&outside, cache_path.join("prune.log")).unwrap();
        let cache = open_cache_root(&root).unwrap().unwrap();

        let error = open_prune_log(&root, &cache.path, cache.identity, 1024).unwrap_err();

        assert_eq!(error.code, "unsafe_path");
        assert_eq!(std::fs::read(outside).unwrap(), b"keep me");
    }

    #[test]
    fn template_marker_replacement_during_destroy_is_preserved() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("state");
        let component = root.join("cache/templates/repo/component");
        std::fs::create_dir_all(&component).unwrap();
        let marker_path = component.join("1234567890abcdef.json");
        std::fs::write(&marker_path, b"initial marker").unwrap();
        let inventory = inventory_for_prune(&root, None, true).unwrap().unwrap();
        let marker = inventory.templates.first().unwrap();
        let replacement_path = component.join("replacement.tmp");
        let marker_path_for_hook = marker_path.clone();
        let replacement = b"replacement marker".to_vec();

        let error = remove_template_marker(
            &root,
            &inventory.cache_root,
            inventory.cache_identity,
            &marker.repository,
            &marker.component,
            &marker.marker_name,
            marker.parent_identity,
            marker.identity,
            &marker.bytes,
            move |_directory_fd, _contents| {
                std::fs::write(&replacement_path, &replacement)
                    .map_err(|error| error.to_string())?;
                std::fs::rename(&replacement_path, &marker_path_for_hook)
                    .map_err(|error| error.to_string())
            },
            None,
        )
        .unwrap_err();

        assert_eq!(error.code, "identity_changed");
        assert_eq!(std::fs::read(marker_path).unwrap(), b"replacement marker");
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[allow(clippy::too_many_arguments)] // Keep each deletion-proof identity and hook explicit.
pub(crate) fn remove_build(
    _environment_root: &Path,
    _expected_cache_path: &Path,
    _expected_cache_identity: CacheFsIdentity,
    _repository: &str,
    _component: &str,
    _digest: &str,
    _expected_build_identity: CacheFsIdentity,
    _expected_key: Option<&str>,
    _expected_last_used_at_ms: Option<i64>,
    _deletion_started: bool,
    _before_delete: impl FnOnce() -> Result<(), String>,
    _deadline: Option<Instant>,
) -> Result<DeleteOutcome, CacheGcFsError> {
    Err(CacheGcFsError::new(
        "unsupported_platform",
        "safe environment-cache removal is supported only on macOS and Linux",
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn read_regular_file(
    _directory_path: &Path,
    _name: &str,
    _max_bytes: usize,
    _deadline: Option<Instant>,
) -> Result<Option<Vec<u8>>, CacheGcFsError> {
    Err(CacheGcFsError::new(
        "unsupported_platform",
        "safe environment-cache metadata reads are supported only on macOS and Linux",
    ))
}
