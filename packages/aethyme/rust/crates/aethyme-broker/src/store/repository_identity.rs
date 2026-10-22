//! Stable per-repository identity stored in the Git common directory.
//!
//! The marker is read-only during migration and inspection. Only an explicit
//! binding transition may create or settle its no-replace publication prefixes.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::error::BrokerError;

const MARKER_NAME: &str = "aethyme-repository-uuid";
const TEMP_PREFIX: &str = ".aethyme-repository-uuid.";
const TEMP_SUFFIX: &str = ".tmp";
const MARKER_HEADER: &str = "AETHYME-REPO-UUID-V1\n";
const MAX_MARKER_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepositoryBindingScope {
    pub repository_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub repository_key: String,
    pub broker_db_path: PathBuf,
    pub repository_uuid: Option<String>,
}

impl RepositoryBindingScope {
    pub(crate) fn capture(
        repository_root: &Path,
        broker_db_path: &Path,
    ) -> Result<Self, BrokerError> {
        Self::capture_with_recovery_prefix_policy(repository_root, broker_db_path, false)
    }

    /// Capture identity for the explicit digest-confirmed apply path only.
    /// Recognized marker publication prefixes are validated without repair and
    /// represented as an uncommitted UUID until the apply transition settles
    /// them. Ordinary and read-only opens must continue to use `capture`.
    pub(crate) fn capture_for_explicit_recovery(
        repository_root: &Path,
        broker_db_path: &Path,
    ) -> Result<Self, BrokerError> {
        Self::capture_with_recovery_prefix_policy(repository_root, broker_db_path, true)
    }

    fn capture_with_recovery_prefix_policy(
        repository_root: &Path,
        broker_db_path: &Path,
        allow_recovery_prefix: bool,
    ) -> Result<Self, BrokerError> {
        let repository_root =
            fs::canonicalize(repository_root).map_err(|source| BrokerError::Io {
                path: repository_root.to_path_buf(),
                source,
            })?;
        let repo = crate::git::GitRepo::discover(&repository_root).map_err(|error| {
            refusal(format!(
                "could not capture repository Git identity: {error}"
            ))
        })?;
        let main_root = fs::canonicalize(repo.main_root().map_err(|error| {
            refusal(format!("could not resolve main repository root: {error}"))
        })?)
        .map_err(|source| BrokerError::Io {
            path: repository_root.clone(),
            source,
        })?;
        let git_common_dir = fs::canonicalize(repo.git_common_dir().map_err(|error| {
            refusal(format!("could not capture Git common directory: {error}"))
        })?)
        .map_err(|source| BrokerError::Io {
            path: main_root.clone(),
            source,
        })?;
        let repository_key = crate::host_state::repository_key(&main_root, Some(&git_common_dir));
        let repository_uuid = if allow_recovery_prefix {
            read_repository_uuid_marker_for_explicit_recovery(&git_common_dir)?
        } else {
            read_repository_uuid_marker(&git_common_dir)?
        };
        Ok(Self {
            repository_root: main_root,
            git_common_dir,
            repository_key,
            broker_db_path: absolute_path(broker_db_path)?,
            repository_uuid,
        })
    }
}

/// Read the UUID without repairing, creating, or removing any marker entry.
pub(crate) fn read_repository_uuid_marker(
    git_common_dir: &Path,
) -> Result<Option<String>, BrokerError> {
    #[cfg(unix)]
    {
        let (directory, identity) = open_verified_directory(git_common_dir)?;
        let temporary_names = marker_temporary_names(git_common_dir, &directory, identity)?;
        if !temporary_names.is_empty() {
            return Err(refusal(
                "repository UUID marker publication is incomplete; run the explicit binding recovery",
            ));
        }
        let Some((uuid, _)) = read_marker_file(&directory, MARKER_NAME, 1)? else {
            return Ok(None);
        };
        verify_directory_name(git_common_dir, identity)?;
        Ok(Some(uuid))
    }
    #[cfg(not(unix))]
    {
        let _ = git_common_dir;
        Ok(None)
    }
}

/// Read identity only for a digest-confirmed explicit apply. Exact
/// temp-only and same-inode link-pair states are fully inventoried but remain
/// unsettled; returning no UUID ensures a bound database row cannot mistake a
/// publication prefix for a completed marker.
fn read_repository_uuid_marker_for_explicit_recovery(
    git_common_dir: &Path,
) -> Result<Option<String>, BrokerError> {
    #[cfg(unix)]
    {
        let (directory, identity) = open_verified_directory(git_common_dir)?;
        let inventory = inspect_marker_inventory(git_common_dir, &directory, identity)?;
        match (inventory.target, inventory.temporaries.as_slice()) {
            (None, []) => Ok(None),
            (Some(target), []) => Ok(Some(target.uuid)),
            (None, [_temporary]) => Ok(None),
            (Some(target), [temporary])
                if target.identity.device == temporary.identity.device
                    && target.identity.inode == temporary.identity.inode
                    && target.identity.links == 2
                    && temporary.identity.links == 2
                    && target.uuid == temporary.uuid =>
            {
                Ok(None)
            }
            _ => Err(refusal(
                "repository UUID marker has conflicting or unrecognized recovery entries",
            )),
        }
    }
    #[cfg(not(unix))]
    {
        read_repository_uuid_marker(git_common_dir)
    }
}

/// Create or recover the UUID marker. Call only after an explicit binding
/// confirmation; ordinary opens, schema migration, and read-only inspection
/// must use `read_repository_uuid_marker`.
pub(crate) fn ensure_repository_uuid_marker(
    git_common_dir: &Path,
    expected_uuid: Option<&str>,
) -> Result<String, BrokerError> {
    #[cfg(unix)]
    {
        if let Some(uuid) = expected_uuid {
            validate_uuid_v4(uuid)?;
        }

        let (directory, directory_identity) = open_verified_directory(git_common_dir)?;
        let inventory = inspect_marker_inventory(git_common_dir, &directory, directory_identity)?;
        match (inventory.target, inventory.temporaries.as_slice()) {
            (Some(target), []) => {
                require_expected_uuid(&target.uuid, expected_uuid)?;
                unix::sync_marker_file(git_common_dir, &directory, directory_identity, &target)?;
                sync_directory(&directory, git_common_dir, directory_identity)?;
                Ok(target.uuid)
            }
            (Some(target), [temporary])
                if target.identity.device == temporary.identity.device
                    && target.identity.inode == temporary.identity.inode
                    && target.identity.links == 2
                    && temporary.identity.links == 2
                    && target.uuid == temporary.uuid =>
            {
                require_expected_uuid(&target.uuid, expected_uuid)?;
                unix::sync_marker_file(git_common_dir, &directory, directory_identity, &target)?;
                sync_directory(&directory, git_common_dir, directory_identity)?;
                unlink_temporary(&directory, git_common_dir, directory_identity, temporary)?;
                Ok(read_repository_uuid_marker(git_common_dir)?
                    .ok_or_else(|| refusal("repository UUID marker disappeared after recovery"))?)
            }
            (Some(_), _) => Err(refusal(
                "repository UUID marker has conflicting or unrecognized recovery entries",
            )),
            (None, [temporary]) => {
                require_expected_uuid(&temporary.uuid, expected_uuid)?;
                unix::publish_temporary(git_common_dir, directory, directory_identity, temporary)?;
                Ok(read_repository_uuid_marker(git_common_dir)?
                    .ok_or_else(|| refusal("repository UUID marker disappeared after recovery"))?)
            }
            (None, []) => {
                if expected_uuid.is_some() {
                    return Err(refusal(
                        "bound repository UUID marker is missing; automatic marker repair is refused",
                    ));
                }
                let uuid = unix::new_uuid_v4(git_common_dir)?;
                let temporary = unix::create_temporary_marker(
                    git_common_dir,
                    &directory,
                    directory_identity,
                    &uuid,
                )?;
                unix::publish_temporary(git_common_dir, directory, directory_identity, &temporary)?;
                Ok(read_repository_uuid_marker(git_common_dir)?.ok_or_else(|| {
                    refusal("repository UUID marker disappeared after publication")
                })?)
            }
            (None, _) => Err(refusal(
                "multiple repository UUID marker recovery entries are present",
            )),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (git_common_dir, expected_uuid);
        Err(refusal(
            "durable repository UUID markers currently require Unix no-follow filesystem support",
        ))
    }
}

fn require_expected_uuid(actual: &str, expected: Option<&str>) -> Result<(), BrokerError> {
    if expected.is_some_and(|expected| expected != actual) {
        return Err(refusal(
            "repository UUID marker differs from the confirmed binding identity",
        ));
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf, BrokerError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|source| BrokerError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
}

fn refusal(message: impl Into<String>) -> BrokerError {
    BrokerError::InvalidRepositoryWorktreeBinding(message.into())
}

pub(crate) fn validate_uuid_v4(value: &str) -> Result<(), BrokerError> {
    let valid_shape = value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        });
    if !valid_shape
        || value.as_bytes().get(14) != Some(&b'4')
        || !matches!(value.as_bytes().get(19), Some(b'8' | b'9' | b'a' | b'b'))
    {
        return Err(refusal(
            "repository UUID marker is not a canonical version-4 UUID",
        ));
    }
    Ok(())
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::ffi::CString;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct DirectoryIdentity {
        device: u64,
        inode: u64,
        uid: u32,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct FileIdentity {
        pub(super) device: u64,
        pub(super) inode: u64,
        pub(super) uid: u32,
        pub(super) mode: u32,
        pub(super) links: u64,
        pub(super) size: u64,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct MarkerFile {
        pub(super) name: String,
        pub(super) identity: FileIdentity,
        pub(super) uuid: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct MarkerInventory {
        pub(super) target: Option<MarkerFile>,
        pub(super) temporaries: Vec<MarkerFile>,
    }

    pub(super) fn open_verified_directory(
        path: &Path,
    ) -> Result<(File, DirectoryIdentity), BrokerError> {
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        );
        let directory = options.open(path).map_err(|source| BrokerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let metadata = directory.metadata().map_err(|source| BrokerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let identity = DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
        };
        // SAFETY: `geteuid` takes no pointers and has no caller-side safety preconditions.
        let effective_uid = unsafe { libc::geteuid() };
        if !metadata.is_dir() || identity.uid != effective_uid || metadata.mode() & 0o022 != 0 {
            return Err(refusal(format!(
                "Git common directory has unsafe type, owner, or permissions: {}",
                path.display()
            )));
        }
        verify_directory_name(path, identity)?;
        Ok((directory, identity))
    }

    pub(super) fn verify_directory_name(
        path: &Path,
        expected: DirectoryIdentity,
    ) -> Result<(), BrokerError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| BrokerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.dev() != expected.device
            || metadata.ino() != expected.inode
            || metadata.uid() != expected.uid
        {
            return Err(refusal(
                "Git common directory changed while verifying its UUID marker",
            ));
        }
        Ok(())
    }

    fn c_name(name: &str) -> Result<CString, BrokerError> {
        CString::new(name).map_err(|_| refusal("repository UUID marker name contains NUL"))
    }

    fn checked_stat_identity_cast<Source, Target>(
        value: Source,
        message: &'static str,
    ) -> Result<Target, BrokerError>
    where
        Target: TryFrom<Source>,
    {
        Target::try_from(value).map_err(|_| refusal(message))
    }

    fn stat_at(directory: &File, name: &str) -> Result<Option<FileIdentity>, BrokerError> {
        let name_c = c_name(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: name_c is NUL-terminated, stat is writable, and directory is
        // an open directory descriptor retained by this function.
        let result = unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name_c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(BrokerError::Io {
                path: PathBuf::from(name),
                source: error,
            });
        }
        // SAFETY: fstatat returned success and initialized the stat.
        let stat = unsafe { stat.assume_init() };
        let file_type_mode = checked_stat_identity_cast::<_, u32>(
            stat.st_mode,
            "repository UUID marker has an invalid file mode",
        )?;
        let identity = FileIdentity {
            device: checked_stat_identity_cast::<_, u64>(
                stat.st_dev,
                "repository UUID marker has an invalid device identity",
            )?,
            inode: checked_stat_identity_cast::<_, u64>(
                stat.st_ino,
                "repository UUID marker has an invalid inode identity",
            )?,
            uid: checked_stat_identity_cast::<_, u32>(
                stat.st_uid,
                "repository UUID marker has an invalid owner identity",
            )?,
            mode: file_type_mode & 0o777,
            links: checked_stat_identity_cast::<_, u64>(
                stat.st_nlink,
                "repository UUID marker has an invalid link count",
            )?,
            size: checked_stat_identity_cast::<_, u64>(
                stat.st_size,
                "repository UUID marker has an invalid size",
            )?,
        };
        let file_type_mask = checked_stat_identity_cast::<_, u32>(
            libc::S_IFMT,
            "repository UUID marker has an invalid file type mask",
        )?;
        let regular_file_type = checked_stat_identity_cast::<_, u32>(
            libc::S_IFREG,
            "repository UUID marker has an invalid regular-file type",
        )?;
        if file_type_mode & file_type_mask != regular_file_type {
            return Err(refusal(format!(
                "repository UUID marker entry {name:?} is not a regular file"
            )));
        }
        Ok(Some(identity))
    }

    fn open_marker_file(
        directory: &File,
        name: &str,
        expected_links: u64,
    ) -> Result<Option<(String, FileIdentity)>, BrokerError> {
        let Some(named_before) = stat_at(directory, name)? else {
            return Ok(None);
        };
        let name_c = c_name(name)?;
        // SAFETY: name_c is NUL-terminated; openat returns a new descriptor or
        // -1, and the directory descriptor remains open.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name_c.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(BrokerError::Io {
                path: PathBuf::from(name),
                source: io::Error::last_os_error(),
            });
        }
        // SAFETY: fd is a newly opened descriptor owned by this function.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let opened = file.metadata().map_err(|source| BrokerError::Io {
            path: PathBuf::from(name),
            source,
        })?;
        let identity = FileIdentity {
            device: opened.dev(),
            inode: opened.ino(),
            uid: opened.uid(),
            mode: opened.mode() & 0o777,
            links: opened.nlink(),
            size: opened.size(),
        };
        let file_size = usize::try_from(identity.size)
            .map_err(|_| refusal("repository UUID marker has an invalid size"))?;
        // SAFETY: `geteuid` takes no pointers and has no caller-side safety preconditions.
        let effective_uid = unsafe { libc::geteuid() };
        if !opened.is_file()
            || identity.uid != effective_uid
            || identity.mode != 0o600
            || identity.links != expected_links
            || file_size > MAX_MARKER_BYTES
            || identity != named_before
        {
            return Err(refusal(format!(
                "repository UUID marker entry {name:?} has unsafe owner, mode, links, size, or identity"
            )));
        }
        let mut bytes = Vec::with_capacity(file_size);
        (&mut file)
            .take((MAX_MARKER_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| BrokerError::Io {
                path: PathBuf::from(name),
                source,
            })?;
        let after = file.metadata().map_err(|source| BrokerError::Io {
            path: PathBuf::from(name),
            source,
        })?;
        let after_identity = FileIdentity {
            device: after.dev(),
            inode: after.ino(),
            uid: after.uid(),
            mode: after.mode() & 0o777,
            links: after.nlink(),
            size: after.size(),
        };
        if after_identity != identity || stat_at(directory, name)? != Some(identity) {
            return Err(refusal(format!(
                "repository UUID marker entry {name:?} changed while reading"
            )));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            refusal(format!(
                "repository UUID marker entry {name:?} is not UTF-8"
            ))
        })?;
        let value = text
            .strip_prefix(MARKER_HEADER)
            .and_then(|value| value.strip_suffix('\n'))
            .ok_or_else(|| {
                refusal(format!(
                    "repository UUID marker entry {name:?} has invalid framing"
                ))
            })?;
        validate_uuid_v4(value)?;
        Ok(Some((value.to_owned(), identity)))
    }

    pub(super) fn read_marker_file(
        directory: &File,
        name: &str,
        expected_links: u64,
    ) -> Result<Option<(String, FileIdentity)>, BrokerError> {
        open_marker_file(directory, name, expected_links)
    }

    pub(super) fn marker_temporary_names(
        path: &Path,
        directory: &File,
        expected_directory: DirectoryIdentity,
    ) -> Result<Vec<String>, BrokerError> {
        let mut names = Vec::new();
        for entry in fs::read_dir(path).map_err(|source| BrokerError::Io {
            path: path.to_path_buf(),
            source,
        })? {
            let entry = entry.map_err(|source| BrokerError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            let bytes = entry.file_name();
            let raw = bytes.as_bytes();
            if !raw.starts_with(TEMP_PREFIX.as_bytes()) {
                continue;
            }
            let suffix = &raw[TEMP_PREFIX.len()..];
            if suffix.len() != 32 + TEMP_SUFFIX.len()
                || !suffix[..32]
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
                || &suffix[32..] != TEMP_SUFFIX.as_bytes()
            {
                return Err(refusal(
                    "repository UUID marker has an unrecognized temporary-file prefix",
                ));
            }
            let name = std::str::from_utf8(raw)
                .map_err(|_| refusal("repository UUID temporary name is not UTF-8"))?
                .to_owned();
            names.push(name);
        }
        verify_directory_name(path, expected_directory)?;
        let _ = directory;
        names.sort();
        Ok(names)
    }

    pub(super) fn inspect_marker_inventory(
        path: &Path,
        directory: &File,
        expected_directory: DirectoryIdentity,
    ) -> Result<MarkerInventory, BrokerError> {
        let temporary_names = marker_temporary_names(path, directory, expected_directory)?;
        if temporary_names.len() > 1 {
            return Err(refusal(
                "multiple repository UUID marker recovery entries are present",
            ));
        }
        let target = open_marker_file(
            directory,
            MARKER_NAME,
            if temporary_names.is_empty() { 1 } else { 2 },
        )?
        .map(|(uuid, identity)| MarkerFile {
            name: MARKER_NAME.to_owned(),
            identity,
            uuid,
        });
        let mut temporaries = Vec::new();
        for name in temporary_names {
            if let Some((uuid, identity)) =
                open_marker_file(directory, &name, if target.is_some() { 2 } else { 1 })?
            {
                temporaries.push(MarkerFile {
                    name,
                    identity,
                    uuid,
                });
            } else {
                return Err(refusal("repository UUID marker temporary disappeared"));
            }
        }
        verify_directory_name(path, expected_directory)?;
        Ok(MarkerInventory {
            target,
            temporaries,
        })
    }

    pub(super) fn sync_directory(
        directory: &File,
        path: &Path,
        identity: DirectoryIdentity,
    ) -> Result<(), BrokerError> {
        directory.sync_all().map_err(|source| BrokerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        verify_directory_name(path, identity)
    }

    pub(super) fn sync_marker_file(
        path: &Path,
        directory: &File,
        directory_identity: DirectoryIdentity,
        marker: &MarkerFile,
    ) -> Result<(), BrokerError> {
        if stat_at(directory, &marker.name)? != Some(marker.identity) {
            return Err(refusal("repository UUID marker changed before data sync"));
        }
        let name_c = c_name(&marker.name)?;
        // SAFETY: name_c is a single entry under the verified directory. The
        // descriptor is opened without following links and checked below.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name_c.as_ptr(),
                libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(BrokerError::Io {
                path: path.join(&marker.name),
                source: io::Error::last_os_error(),
            });
        }
        // SAFETY: fd is a newly opened descriptor owned by this function.
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(|source| BrokerError::Io {
            path: path.join(&marker.name),
            source,
        })?;
        let identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            mode: metadata.mode() & 0o777,
            links: metadata.nlink(),
            size: metadata.size(),
        };
        if !metadata.is_file() || identity != marker.identity {
            return Err(refusal(
                "repository UUID marker changed while opening for data sync",
            ));
        }
        file.sync_all().map_err(|source| BrokerError::Io {
            path: path.join(&marker.name),
            source,
        })?;
        let after = file.metadata().map_err(|source| BrokerError::Io {
            path: path.join(&marker.name),
            source,
        })?;
        let after_identity = FileIdentity {
            device: after.dev(),
            inode: after.ino(),
            uid: after.uid(),
            mode: after.mode() & 0o777,
            links: after.nlink(),
            size: after.size(),
        };
        if after_identity != marker.identity
            || stat_at(directory, &marker.name)? != Some(marker.identity)
        {
            return Err(refusal("repository UUID marker changed during data sync"));
        }
        verify_directory_name(path, directory_identity)
    }

    pub(super) fn unlink_temporary(
        directory: &File,
        path: &Path,
        identity: DirectoryIdentity,
        temporary: &MarkerFile,
    ) -> Result<(), BrokerError> {
        if stat_at(directory, &temporary.name)? != Some(temporary.identity) {
            return Err(refusal("repository UUID temporary changed before cleanup"));
        }
        let name_c = c_name(&temporary.name)?;
        // SAFETY: name_c refers to one validated directory entry and the
        // directory fd anchors the unlink to the verified common directory.
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name_c.as_ptr(), 0) } != 0 {
            return Err(BrokerError::Io {
                path: path.join(&temporary.name),
                source: io::Error::last_os_error(),
            });
        }
        sync_directory(directory, path, identity)
    }

    pub(super) fn publish_temporary(
        path: &Path,
        directory: File,
        directory_identity: DirectoryIdentity,
        temporary: &MarkerFile,
    ) -> Result<(), BrokerError> {
        let inventory = inspect_marker_inventory(path, &directory, directory_identity)?;
        if inventory.target.is_some()
            || inventory.temporaries.len() != 1
            || inventory.temporaries[0] != *temporary
            || temporary.identity.links != 1
        {
            return Err(refusal(
                "repository UUID marker changed before no-replace publication",
            ));
        }
        // A recovered temporary may have survived a crash before its data
        // reached stable storage. Re-sync the exact inventoried inode and its
        // directory entry before publishing the target name.
        sync_marker_file(path, &directory, directory_identity, temporary)?;
        sync_directory(&directory, path, directory_identity)?;
        let temporary_c = c_name(&temporary.name)?;
        let target_c = c_name(MARKER_NAME)?;
        // SAFETY: both names are single path components and the open directory
        // fd anchors both sides of this no-replace hard-link operation.
        let result = unsafe {
            libc::linkat(
                directory.as_raw_fd(),
                temporary_c.as_ptr(),
                directory.as_raw_fd(),
                target_c.as_ptr(),
                0,
            )
        };
        if result != 0 {
            return Err(BrokerError::Io {
                path: path.join(MARKER_NAME),
                source: io::Error::last_os_error(),
            });
        }
        sync_directory(&directory, path, directory_identity)?;
        let linked = inspect_marker_inventory(path, &directory, directory_identity)?;
        let target = linked
            .target
            .as_ref()
            .ok_or_else(|| refusal("repository UUID marker link was not published"))?;
        let linked_temp = linked
            .temporaries
            .first()
            .ok_or_else(|| refusal("repository UUID temporary disappeared after linking"))?;
        if target.identity.device != temporary.identity.device
            || target.identity.inode != temporary.identity.inode
            || linked_temp.identity.device != target.identity.device
            || linked_temp.identity.inode != target.identity.inode
            || target.uuid != temporary.uuid
        {
            return Err(refusal(
                "repository UUID marker hard-link pair does not match the reviewed temporary",
            ));
        }
        unlink_temporary(&directory, path, directory_identity, linked_temp)?;
        let final_inventory = inspect_marker_inventory(path, &directory, directory_identity)?;
        if final_inventory.temporaries.is_empty()
            && final_inventory
                .target
                .as_ref()
                .is_some_and(|marker| marker.uuid == temporary.uuid && marker.identity.links == 1)
        {
            Ok(())
        } else {
            Err(refusal(
                "repository UUID marker did not settle to one private file",
            ))
        }
    }

    pub(super) fn create_temporary_marker(
        path: &Path,
        directory: &File,
        directory_identity: DirectoryIdentity,
        uuid: &str,
    ) -> Result<MarkerFile, BrokerError> {
        let bytes = format!("{MARKER_HEADER}{uuid}\n").into_bytes();
        for _ in 0..8 {
            let nonce = random_hex(path, 16)?;
            let name = format!("{TEMP_PREFIX}{nonce}{TEMP_SUFFIX}");
            let name_c = c_name(&name)?;
            // SAFETY: name_c is one component under the verified directory;
            // O_EXCL and O_NOFOLLOW prevent replacing another entry.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name_c.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                    0o600,
                )
            };
            if fd < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(BrokerError::Io {
                    path: path.join(&name),
                    source: error,
                });
            }
            // SAFETY: fd is a newly created descriptor owned by this function.
            let mut file = unsafe { File::from_raw_fd(fd) };
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|source| BrokerError::Io {
                    path: path.join(&name),
                    source,
                })?;
            file.write_all(&bytes).map_err(|source| BrokerError::Io {
                path: path.join(&name),
                source,
            })?;
            file.sync_all().map_err(|source| BrokerError::Io {
                path: path.join(&name),
                source,
            })?;
            let inventory = inspect_marker_inventory(path, directory, directory_identity)?;
            let marker = inventory
                .temporaries
                .into_iter()
                .find(|marker| marker.name == name)
                .ok_or_else(|| refusal("new repository UUID temporary was not inventoried"))?;
            if marker.uuid != uuid || marker.identity.links != 1 {
                return Err(refusal("new repository UUID temporary failed verification"));
            }
            sync_directory(directory, path, directory_identity)?;
            return Ok(marker);
        }
        Err(refusal(
            "could not allocate a unique repository UUID marker temporary",
        ))
    }

    pub(super) fn new_uuid_v4(path: &Path) -> Result<String, BrokerError> {
        let bytes = random_bytes(path, 16)?;
        let mut bytes: [u8; 16] = bytes
            .try_into()
            .map_err(|_| refusal("random UUID source returned the wrong byte count"))?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Ok(format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3],
            bytes[4],
            bytes[5],
            bytes[6],
            bytes[7],
            bytes[8],
            bytes[9],
            bytes[10],
            bytes[11],
            bytes[12],
            bytes[13],
            bytes[14],
            bytes[15]
        ))
    }

    fn random_hex(path: &Path, length: usize) -> Result<String, BrokerError> {
        let bytes = random_bytes(path, length)?;
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}")
                .map_err(|_| refusal("could not encode repository UUID temporary nonce"))?;
        }
        Ok(output)
    }

    fn random_bytes(path: &Path, length: usize) -> Result<Vec<u8>, BrokerError> {
        let mut bytes = vec![0; length];
        let mut random = File::open("/dev/urandom").map_err(|source| BrokerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        random
            .read_exact(&mut bytes)
            .map_err(|source| BrokerError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(bytes)
    }
}

#[cfg(unix)]
use unix::{
    inspect_marker_inventory, marker_temporary_names, open_verified_directory, read_marker_file,
    sync_directory, unlink_temporary, verify_directory_name,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_uuid_is_canonical_v4() {
        assert!(validate_uuid_v4("f47ac10b-58cc-4372-a567-0e02b2c3d479").is_ok());
        assert!(validate_uuid_v4("f47ac10b-58cc-3372-a567-0e02b2c3d479").is_err());
        assert!(validate_uuid_v4("f47ac10b-58cc-4372-c567-0e02b2c3d479").is_err());
        assert!(validate_uuid_v4("F47AC10B-58CC-4372-A567-0E02B2C3D479").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn explicit_marker_publication_is_stable_and_read_only_reads_do_not_create_it() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(read_repository_uuid_marker(directory.path()).unwrap(), None);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        let first = ensure_repository_uuid_marker(directory.path(), None).unwrap();
        assert_eq!(
            read_repository_uuid_marker(directory.path()).unwrap(),
            Some(first.clone())
        );
        assert_eq!(
            ensure_repository_uuid_marker(directory.path(), Some(&first)).unwrap(),
            first
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn explicit_marker_recovers_only_exact_temporary_prefix() {
        use std::io::Write as _;
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

        let directory = tempfile::tempdir().unwrap();
        let uuid = "f47ac10b-58cc-4372-a567-0e02b2c3d479";
        let name = format!("{TEMP_PREFIX}0123456789abcdef0123456789abcdef{TEMP_SUFFIX}");
        let path = directory.path().join(&name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(format!("{MARKER_HEADER}{uuid}\n").as_bytes())
            .unwrap();
        file.sync_all().unwrap();
        assert!(read_repository_uuid_marker(directory.path()).is_err());
        assert_eq!(
            read_repository_uuid_marker_for_explicit_recovery(directory.path()).unwrap(),
            None
        );
        assert_eq!(
            ensure_repository_uuid_marker(directory.path(), Some(uuid)).unwrap(),
            uuid
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        assert_eq!(
            std::fs::metadata(directory.path().join(MARKER_NAME))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_recovery_inspects_link_pair_without_settling_it() {
        use std::os::unix::fs::OpenOptionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let uuid = "f47ac10b-58cc-4372-a567-0e02b2c3d479";
        let target = directory.path().join(MARKER_NAME);
        let temporary = directory.path().join(format!(
            "{TEMP_PREFIX}0123456789abcdef0123456789abcdef{TEMP_SUFFIX}"
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .unwrap();
        file.write_all(format!("{MARKER_HEADER}{uuid}\n").as_bytes())
            .unwrap();
        file.sync_all().unwrap();
        std::fs::hard_link(&temporary, &target).unwrap();

        assert!(read_repository_uuid_marker(directory.path()).is_err());
        assert_eq!(
            read_repository_uuid_marker_for_explicit_recovery(directory.path()).unwrap(),
            None
        );
        assert!(target.exists());
        assert!(temporary.exists());

        assert_eq!(
            ensure_repository_uuid_marker(directory.path(), Some(uuid)).unwrap(),
            uuid
        );
        assert!(target.exists());
        assert!(!temporary.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn explicit_marker_refuses_conflicting_or_unrecognized_prefixes() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join(MARKER_NAME);
        let valid = format!("{MARKER_HEADER}f47ac10b-58cc-4372-a567-0e02b2c3d479\n");
        std::fs::write(&target, valid).unwrap();
        assert!(read_repository_uuid_marker(directory.path()).is_err());
        assert!(read_repository_uuid_marker_for_explicit_recovery(directory.path()).is_err());
        assert!(ensure_repository_uuid_marker(directory.path(), None).is_err());

        std::fs::remove_file(&target).unwrap();
        std::fs::write(
            directory
                .path()
                .join(format!("{TEMP_PREFIX}not-a-canonical-suffix{TEMP_SUFFIX}")),
            b"unknown",
        )
        .unwrap();
        assert!(read_repository_uuid_marker_for_explicit_recovery(directory.path()).is_err());
        assert!(ensure_repository_uuid_marker(directory.path(), None).is_err());
    }
}
