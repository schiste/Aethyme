//! File operations confined beneath a validated root.
//!
//! Transcripts hold secrets, and the lane removes originals unattended, so
//! every create, read, link and unlink goes through a [`Root`]:
//! - the root itself must exist, be a real directory (never a symbolic
//!   link) and be owned by the current user;
//! - every path below it is walked one component at a time with
//!   `openat(O_DIRECTORY | O_NOFOLLOW)`, so no symbolic link inside the root
//!   is ever followed, and each directory must be on the root's device, so
//!   no mount point is crossed;
//! - files are opened with `O_NOFOLLOW`, and an entry is only acted on when
//!   `fstatat(AT_SYMLINK_NOFOLLOW)` shows a regular file on the root's
//!   device;
//! - new files are created with `O_CREAT | O_EXCL | O_NOFOLLOW` and mode
//!   0600, new directories with mode 0700, and a directory an archive is
//!   written into is narrowed to 0700;
//! - publication is `linkat`, which never replaces an existing entry.

// `stat` field types differ between Linux and macOS; the casts below are
// no-ops on one platform and widenings on the other.
#![allow(clippy::unnecessary_cast)]

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
/// Times `F_FULLFSYNC` was refused and plain `fsync` stood in (macOS).
static FULL_SYNC_FALLBACKS: AtomicU64 = AtomicU64::new(0);

/// How many full syncs fell back to `fsync` in this process so far.
pub(crate) fn full_sync_fallbacks() -> u64 {
    FULL_SYNC_FALLBACKS.load(Ordering::Relaxed)
}

/// Flush a file or directory to stable storage. On macOS `fsync` leaves
/// data in the drive's cache, so `fcntl(F_FULLFSYNC)` is used, falling back
/// to `fsync` (and counting it) only when the file system refuses it with
/// ENOTSUP or EINVAL. Any other failure is returned.
pub(crate) fn full_sync(fd: libc::c_int) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `fd` is an open descriptor for the duration of the call;
        // F_FULLFSYNC takes no further argument.
        let result = unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) };
        if result != -1 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::ENOTSUP) | Some(libc::EINVAL)
        ) {
            return Err(error);
        }
        FULL_SYNC_FALLBACKS.fetch_add(1, Ordering::Relaxed);
    }
    // SAFETY: `fd` is an open descriptor for the duration of the call.
    check(unsafe { libc::fsync(fd) }).map(|_| ())
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains a NUL byte"))
}

/// `<prefix><pid>-<counter>-<nanos><suffix>`: unique within the process,
/// unlikely to collide across processes (callers retry on a collision).
fn unique_name(prefix: &str, suffix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or(0);
    format!(
        "{prefix}{}-{}-{nanos}{suffix}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

fn fstat(fd: &OwnedFd) -> io::Result<libc::stat> {
    // SAFETY: an all-zero `stat` is a valid value for this plain C struct,
    // and `fstat` only writes into it.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is an open descriptor owned for the duration of the call
    // and `stat` points to writable memory of the right size.
    check(unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) })?;
    Ok(stat)
}

fn is_dir(stat: &libc::stat) -> bool {
    stat.st_mode & libc::S_IFMT == libc::S_IFDIR
}

fn is_regular(stat: &libc::stat) -> bool {
    stat.st_mode & libc::S_IFMT == libc::S_IFREG
}

fn current_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The identity of a file at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub len: u64,
    pub modified_sec: i64,
    pub modified_nsec: i64,
    pub inode: u64,
    pub device: u64,
}

impl Stamp {
    fn of(stat: &libc::stat) -> Self {
        Self {
            len: stat.st_size as u64,
            modified_sec: stat.st_mtime as i64,
            modified_nsec: stat.st_mtime_nsec as i64,
            inode: stat.st_ino as u64,
            device: stat.st_dev as u64,
        }
    }
}

/// A validated directory everything else is confined beneath.
#[derive(Debug)]
pub(crate) struct Root {
    path: PathBuf,
    fd: OwnedFd,
    device: u64,
}

/// One directory beneath a [`Root`], opened without following links.
#[derive(Debug)]
pub(crate) struct Dir {
    fd: OwnedFd,
    device: u64,
}

fn open_directory_at(parent: libc::c_int, name: &CString) -> io::Result<OwnedFd> {
    // SAFETY: `name` is a valid NUL-terminated string and `parent` is an open
    // directory descriptor (or AT_FDCWD); the returned descriptor, when
    // non-negative, is owned by the `OwnedFd` built from it.
    let fd = check(unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })?;
    // SAFETY: `fd` was just returned by `openat` and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

impl Root {
    /// Validate `path` as a root: an existing directory, not a symbolic
    /// link, owned by the current user.
    pub fn open(path: &Path) -> io::Result<Self> {
        let name = c_name(path.as_os_str())?;
        let fd = open_directory_at(libc::AT_FDCWD, &name).map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                io::Error::new(io::ErrorKind::InvalidInput, "root is a symbolic link")
            } else {
                error
            }
        })?;
        let stat = fstat(&fd)?;
        if !is_dir(&stat) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "root is not a directory",
            ));
        }
        if stat.st_uid != current_uid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "root is not owned by the current user",
            ));
        }
        Ok(Self {
            path: path.to_path_buf(),
            device: stat.st_dev as u64,
            fd,
        })
    }

    /// Whether `path` lies beneath this root (lexically).
    pub fn contains(&self, path: &Path) -> bool {
        path.strip_prefix(&self.path)
            .is_ok_and(|relative| relative.components().next().is_some())
    }

    /// The normal components of `path` below the root; anything else
    /// (`..`, `.`, an absolute remainder) is refused.
    fn components(&self, path: &Path) -> io::Result<Vec<CString>> {
        let relative = path
            .strip_prefix(&self.path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path is outside its root"))?;
        let mut names = Vec::new();
        for component in relative.components() {
            match component {
                Component::Normal(name) => names.push(c_name(name)?),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "path has a non-normal component",
                    ));
                }
            }
        }
        if names.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path names the root itself",
            ));
        }
        Ok(names)
    }

    /// Open the directory holding `path` and return it with the entry's
    /// name. With `create`, missing directories are made with mode 0700.
    pub fn parent_of(&self, path: &Path, create: bool) -> io::Result<(Dir, CString)> {
        let mut names = self.components(path)?;
        let name = names.pop().expect("components is never empty");
        let mut current: Option<OwnedFd> = None;
        for component in &names {
            let parent = current
                .as_ref()
                .map_or(self.fd.as_raw_fd(), |fd| fd.as_raw_fd());
            let opened = match open_directory_at(parent, component) {
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    // SAFETY: `parent` is an open directory descriptor and
                    // `component` a valid NUL-terminated name.
                    let made = unsafe { libc::mkdirat(parent, component.as_ptr(), 0o700) };
                    if made < 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(error);
                        }
                    }
                    open_directory_at(parent, component)?
                }
                other => other?,
            };
            let stat = fstat(&opened)?;
            if stat.st_dev as u64 != self.device {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path crosses a mount point",
                ));
            }
            current = Some(opened);
        }
        let fd = match current {
            Some(fd) => fd,
            None => self.fd.try_clone()?,
        };
        Ok((
            Dir {
                fd,
                device: self.device,
            },
            name,
        ))
    }
}

impl Dir {
    /// The entry's identity, without following a link. Refuses anything
    /// but a regular file on the root's device.
    pub fn stamp(&self, name: &CString) -> io::Result<Stamp> {
        // SAFETY: an all-zero `stat` is a valid value for this plain C struct.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `self.fd` is an open directory descriptor, `name` a valid
        // NUL-terminated string and `stat` writable memory of the right size.
        check(unsafe {
            libc::fstatat(
                self.fd.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })?;
        if !is_regular(&stat) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        if stat.st_dev as u64 != self.device {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file is on another device",
            ));
        }
        Ok(Stamp::of(&stat))
    }

    /// Whether an entry of any kind exists under `name`.
    pub fn exists(&self, name: &CString) -> io::Result<bool> {
        // SAFETY: as in `stamp`.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: as in `stamp`.
        let result = unsafe {
            libc::fstatat(
                self.fd.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(error)
        }
    }

    /// Open a regular file for reading without following a link, and
    /// return it with its identity.
    pub fn open_read(&self, name: &CString) -> io::Result<(File, Stamp)> {
        // SAFETY: `self.fd` is an open directory descriptor and `name` a
        // valid NUL-terminated string; a non-negative result is owned below.
        let fd = check(unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        // SAFETY: `fd` was just returned by `openat` and nothing else owns it.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let stat = fstat(&fd)?;
        if !is_regular(&stat) || stat.st_dev as u64 != self.device {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file on the root's device",
            ));
        }
        Ok((File::from(fd), Stamp::of(&stat)))
    }

    /// Open an existing regular file for appending, without following a
    /// link.
    pub fn open_append(&self, name: &CString) -> io::Result<File> {
        // SAFETY: `self.fd` is an open directory descriptor and `name` a
        // valid NUL-terminated string; a non-negative result is owned below.
        let fd = check(unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_APPEND | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        // SAFETY: `fd` was just returned by `openat` and nothing else owns it.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let stat = fstat(&fd)?;
        if !is_regular(&stat) || stat.st_dev as u64 != self.device {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file on the root's device",
            ));
        }
        Ok(File::from(fd))
    }

    /// Create a new private (0600) temporary file named `<prefix>…<suffix>`.
    /// Never follows or replaces an existing entry.
    pub fn create_temp(&self, prefix: &str, suffix: &str) -> io::Result<TempFile<'_>> {
        for _ in 0..64 {
            let name = c_name(OsStr::new(&unique_name(prefix, suffix)))?;
            let mode: libc::c_uint = 0o600;
            // SAFETY: `self.fd` is an open directory descriptor and `name` a
            // valid NUL-terminated string; the mode argument is passed as the
            // variadic `c_uint` `openat` expects with O_CREAT.
            let fd = unsafe {
                libc::openat(
                    self.fd.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    mode,
                )
            };
            if fd < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(error);
            }
            // SAFETY: `fd` was just returned by `openat` and nothing else owns it.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            // The umask may have narrowed it further, never widened it; make
            // it exactly 0600.
            // SAFETY: `fd` is an open descriptor owned by this function.
            check(unsafe { libc::fchmod(fd.as_raw_fd(), 0o600) })?;
            return Ok(TempFile {
                dir: self,
                name,
                file: File::from(fd),
                armed: true,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not find a free temporary name",
        ))
    }

    /// Hard-link `from` to a new unique name `<prefix>…<suffix>` and return
    /// that name. Never replaces an entry.
    pub fn link_unique(&self, from: &CString, prefix: &str, suffix: &str) -> io::Result<CString> {
        for _ in 0..64 {
            let name = c_name(OsStr::new(&unique_name(prefix, suffix)))?;
            match self.link_noclobber(from, &name) {
                Ok(()) => return Ok(name),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not find a free backup name",
        ))
    }

    /// Publish `from` as `to` with a hard link; fails with `AlreadyExists`
    /// rather than replace anything.
    pub fn link_noclobber(&self, from: &CString, to: &CString) -> io::Result<()> {
        // SAFETY: both descriptors are this open directory and both names are
        // valid NUL-terminated strings; flag 0 does not follow a link.
        check(unsafe {
            libc::linkat(
                self.fd.as_raw_fd(),
                from.as_ptr(),
                self.fd.as_raw_fd(),
                to.as_ptr(),
                0,
            )
        })
        .map(|_| ())
    }

    /// Remove the regular file `name` if it is still exactly `expected`.
    pub fn unlink_if(&self, name: &CString, expected: Stamp) -> io::Result<()> {
        let now = self.stamp(name)?;
        if now != expected {
            return Err(io::Error::other("file changed before removal"));
        }
        self.unlink(name)
    }

    /// Remove the entry `name` (never a directory).
    pub fn unlink(&self, name: &CString) -> io::Result<()> {
        // SAFETY: `self.fd` is an open directory descriptor and `name` a
        // valid NUL-terminated string; flag 0 refuses directories.
        check(unsafe { libc::unlinkat(self.fd.as_raw_fd(), name.as_ptr(), 0) }).map(|_| ())
    }

    /// Make the directory durable after an entry changed.
    pub fn sync(&self) -> io::Result<()> {
        full_sync(self.fd.as_raw_fd())
    }

    /// Atomically replace `to` with `from` (both in this directory).
    pub fn replace(&self, from: &CString, to: &CString) -> io::Result<()> {
        // SAFETY: both descriptors are this open directory and both names are
        // valid NUL-terminated strings; `renameat` never follows `to`.
        check(unsafe {
            libc::renameat(
                self.fd.as_raw_fd(),
                from.as_ptr(),
                self.fd.as_raw_fd(),
                to.as_ptr(),
            )
        })
        .map(|_| ())
    }

    /// Narrow the directory to 0700 when group or others may enter it.
    pub fn make_private(&self) -> io::Result<()> {
        let stat = fstat(&self.fd)?;
        if stat.st_mode & 0o077 != 0 {
            // SAFETY: `self.fd` is an open directory descriptor owned by `self`.
            check(unsafe { libc::fchmod(self.fd.as_raw_fd(), 0o700) })?;
        }
        Ok(())
    }
}

/// A temporary file that is removed on drop unless it was published.
pub(crate) struct TempFile<'a> {
    dir: &'a Dir,
    pub name: CString,
    pub file: File,
    armed: bool,
}

impl TempFile<'_> {
    /// Flush the file's data to stable storage (see [`full_sync`]).
    pub fn sync(&self) -> io::Result<()> {
        full_sync(self.file.as_raw_fd())
    }

    /// Publish under `to` (no-clobber), then remove the temporary name and
    /// make the directory durable. A sync failure is returned after the
    /// link: the caller must then treat the published entry as not durable.
    pub fn publish(mut self, to: &CString) -> io::Result<()> {
        self.sync()?;
        self.dir.link_noclobber(&self.name, to)?;
        self.armed = false;
        super::best_effort(self.dir.unlink(&self.name));
        self.dir.sync()
    }

    /// Atomically replace `to` with this file, then make the directory
    /// durable.
    pub fn publish_replacing(mut self, to: &CString) -> io::Result<()> {
        self.sync()?;
        self.dir.replace(&self.name, to)?;
        self.armed = false;
        self.dir.sync()
    }
}

impl Drop for TempFile<'_> {
    fn drop(&mut self) {
        if self.armed {
            super::best_effort(self.dir.unlink(&self.name));
        }
    }
}
