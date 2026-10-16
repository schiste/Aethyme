//! Descriptor-relative deletion for broker-reviewed cache trees.
//!
//! Path-based recursive deletion has a check/use gap: an entry can be a
//! directory when inspected and a symlink by the time recursion opens it.
//! This module pins the tree root and performs every walk and unlink relative
//! to retained directory descriptors. Platforms without the required
//! no-follow descriptor operations fail closed. These checks stop symlink
//! redirection, but do not lock the filesystem namespace against arbitrary
//! concurrent renames; callers must use them under the broker's cooperative
//! writer and closed-worktree rules.

use std::io;
use std::path::Path;
use std::time::Instant;

/// The directory is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreeRemoval {
    Complete,
    /// The budget ran out. The directory and its witness remain resumable.
    Interrupted,
}

/// Error from a bounded tree removal, including successful unlink progress.
#[derive(Debug)]
pub(crate) struct TreeRemovalError {
    pub(crate) source: io::Error,
    pub(crate) entries_reclaimed: usize,
}

/// Stable identity of one opened directory object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TreeObjectIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

/// A condemned tree opened without following its final path component.
///
/// The object and filesystem identities are read from the retained directory
/// handle. A registry claim can hold this value through apply so the identity
/// it verified is the object the deleter removes.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) struct PinnedTreeRemoval(unix::PinnedTreeRemoval);

/// Deletion is deliberately unavailable where the no-follow implementation
/// has not been qualified.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) struct PinnedTreeRemoval;

impl PinnedTreeRemoval {
    /// Open and pin `path` and its containing directory without following any
    /// symlink in the canonical parent path or at the final path component.
    pub(crate) fn open(_path: &Path) -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            Ok(Self(unix::PinnedTreeRemoval::open(_path)?))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(unsupported_platform())
        }
    }

    /// Identity of the directory opened by [`Self::open`].
    pub(crate) fn object_identity(&self) -> io::Result<TreeObjectIdentity> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            Ok(self.0.object_identity)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(unsupported_platform())
        }
    }

    /// Filesystem identity read from the retained directory descriptor.
    ///
    /// The byte representation is the platform `statfs.f_fsid` field, matching
    /// the worktree-root registry's saved filesystem fingerprint.
    pub(crate) fn filesystem_identity(&self) -> io::Result<Vec<u8>> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            Ok(self.0.filesystem_identity.clone())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(unsupported_platform())
        }
    }

    /// Remove this exact tree through its retained root and parent handles.
    ///
    /// `keep_until_last` is removed only after every other entry is gone. The
    /// root name is checked against the pinned inode before mutations and
    /// before final `rmdir`, so a replaced or unmounted root is left alone.
    pub(crate) fn remove(
        self,
        _keep_until_last: Option<&str>,
        _deadline: Option<Instant>,
    ) -> io::Result<TreeRemoval> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.0.remove(_keep_until_last, _deadline, &mut || Ok(()))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _self = self;
            Err(unsupported_platform())
        }
    }

    /// Remove this tree only after validating the retained witness bytes.
    ///
    /// The callback receives bytes read through the pinned root directory
    /// descriptor after the file's identity and bounded size are checked, and
    /// before the first unlink. It can bind removal to an owner's recorded
    /// marker payload. This is a point-in-time check; callers must still use
    /// the broker's cooperative writer rules because an unrelated process can
    /// rewrite the witness after validation.
    pub(crate) fn remove_verified_witness<F>(
        self,
        _witness_name: &str,
        _deadline: Option<Instant>,
        _validate: F,
    ) -> io::Result<TreeRemoval>
    where
        F: FnOnce(&[u8]) -> io::Result<()>,
    {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.0
                .remove_verified_witness(_witness_name, _deadline, _validate)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _self = self;
            Err(unsupported_platform())
        }
    }
}

/// Remove a path after pinning it. Registered orphan apply should instead keep
/// a `PinnedTreeRemoval` inside its claim and invoke that handle directly.
pub(crate) fn remove_condemned_tree(
    _dir: &Path,
    _keep_until_last: Option<&str>,
    _deadline: Option<Instant>,
) -> io::Result<TreeRemoval> {
    remove_condemned_tree_with_progress(_dir, _keep_until_last, _deadline)
        .map(|(outcome, _entries_reclaimed)| outcome)
        .map_err(|error| error.source)
}

/// Remove a path after pinning it and preserve the number of successful
/// unlinks even when the deadline interrupts removal or a later operation
/// fails. A missing final leaf is complete with zero progress; an unavailable
/// parent remains an error.
pub(crate) fn remove_condemned_tree_with_progress(
    _dir: &Path,
    _keep_until_last: Option<&str>,
    _deadline: Option<Instant>,
) -> Result<(TreeRemoval, usize), TreeRemovalError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        match unix::PinnedTreeRemoval::open_optional(_dir) {
            Ok(Some(tree)) => tree.remove_with_progress(_keep_until_last, _deadline),
            Ok(None) => Ok((TreeRemoval::Complete, 0)),
            Err(source) => Err(TreeRemovalError {
                source,
                entries_reclaimed: 0,
            }),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(TreeRemovalError {
            source: unsupported_platform(),
            entries_reclaimed: 0,
        })
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn unsupported_platform() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "safe descriptor-relative tree removal is unavailable on this platform",
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::MetadataExt;
    use std::path::{Component, Path};
    use std::time::Instant;

    use super::{TreeObjectIdentity, TreeRemoval};

    /// How many times a directory refilled by a concurrent writer is swept
    /// again before the error is reported.
    const TREE_REMOVAL_ATTEMPTS: usize = 3;
    /// Recovery manifests can be larger than ordinary marker files, but
    /// retaining an unbounded marker in memory would make cleanup itself an
    /// unbounded allocation. Refuse before mutation when it exceeds this cap.
    const MAX_WITNESS_RESTORE_BYTES: u64 = 16 * 1024 * 1024;

    pub(super) struct PinnedTreeRemoval {
        parent: File,
        name: CString,
        root: File,
        pub(super) object_identity: TreeObjectIdentity,
        pub(super) filesystem_identity: Vec<u8>,
        mount_identity: Vec<u8>,
    }

    impl PinnedTreeRemoval {
        pub(super) fn open(path: &Path) -> io::Result<Self> {
            Self::open_optional(path)?
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "tree root does not exist"))
        }

        pub(super) fn open_optional(path: &Path) -> io::Result<Option<Self>> {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()?.join(path)
            };
            let leaf = absolute.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "tree root has no leaf name")
            })?;
            let parent_path = absolute.parent().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "tree root has no parent")
            })?;

            // Resolve only explicitly supported platform aliases. General
            // canonicalization could follow a replaced ancestor symlink and
            // redirect removal to a different tree.
            let canonical_parent = canonical_parent_without_arbitrary_symlinks(parent_path)?;
            let parent = open_directory_chain(&canonical_parent)?;
            let name = c_name(leaf)?;
            let root = match open_directory_at(parent.as_raw_fd(), &name) {
                Ok(root) => root,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            let metadata = root.metadata()?;
            if !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "tree root is not a directory",
                ));
            }
            let object_identity = TreeObjectIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            };
            let root_filesystem_identity = filesystem_identity(root.as_raw_fd())?;
            let root_mount_identity = mount_identity(root.as_raw_fd())?;
            if metadata.dev() != parent.metadata()?.dev()
                || filesystem_identity(parent.as_raw_fd())? != root_filesystem_identity
                || mount_identity(parent.as_raw_fd())? != root_mount_identity
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "refusing to remove a mount-point root",
                ));
            }

            Ok(Some(Self {
                parent,
                name,
                root,
                object_identity,
                filesystem_identity: root_filesystem_identity,
                mount_identity: root_mount_identity,
            }))
        }

        pub(super) fn remove<F>(
            self,
            keep_until_last: Option<&str>,
            deadline: Option<Instant>,
            before_directory_open: &mut F,
        ) -> io::Result<TreeRemoval>
        where
            F: FnMut() -> io::Result<()>,
        {
            self.remove_with_hooks(
                keep_until_last,
                deadline,
                before_directory_open,
                &mut || Ok(()),
            )
        }

        pub(super) fn remove_with_progress(
            self,
            keep_until_last: Option<&str>,
            deadline: Option<Instant>,
        ) -> Result<(TreeRemoval, usize), super::TreeRemovalError> {
            self.remove_with_hooks_and_progress(
                keep_until_last,
                deadline,
                &mut || Ok(()),
                &mut || Ok(()),
                |_| Ok(()),
            )
        }

        pub(super) fn remove_verified_witness<F>(
            self,
            witness_name: &str,
            deadline: Option<Instant>,
            validate: F,
        ) -> io::Result<TreeRemoval>
        where
            F: FnOnce(&[u8]) -> io::Result<()>,
        {
            self.remove_with_hooks_and_progress(
                Some(witness_name),
                deadline,
                &mut || Ok(()),
                &mut || Ok(()),
                validate,
            )
            .map(|(outcome, _entries_reclaimed)| outcome)
            .map_err(|error| error.source)
        }

        fn remove_with_hooks<F, G>(
            self,
            keep_until_last: Option<&str>,
            deadline: Option<Instant>,
            before_directory_open: &mut F,
            after_witness_unlink: &mut G,
        ) -> io::Result<TreeRemoval>
        where
            F: FnMut() -> io::Result<()>,
            G: FnMut() -> io::Result<()>,
        {
            self.remove_with_hooks_and_progress(
                keep_until_last,
                deadline,
                before_directory_open,
                after_witness_unlink,
                |_| Ok(()),
            )
            .map(|(outcome, _entries_reclaimed)| outcome)
            .map_err(|error| error.source)
        }

        fn remove_with_hooks_and_progress<F, G, V>(
            self,
            keep_until_last: Option<&str>,
            deadline: Option<Instant>,
            before_directory_open: &mut F,
            after_witness_unlink: &mut G,
            validate_witness: V,
        ) -> Result<(TreeRemoval, usize), super::TreeRemovalError>
        where
            F: FnMut() -> io::Result<()>,
            G: FnMut() -> io::Result<()>,
            V: FnOnce(&[u8]) -> io::Result<()>,
        {
            let mut entries_reclaimed = 0;
            let result = (|| -> io::Result<TreeRemoval> {
                let keep = keep_until_last.map(CString::new).transpose().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "witness name contains NUL")
                })?;
                if keep.as_ref().is_some_and(|name| {
                    name.as_bytes().contains(&b'/')
                        || name.as_bytes() == b"."
                        || name.as_bytes() == b".."
                }) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "witness name must be a single ordinary path component",
                    ));
                }
                let witness_backup = if let Some(witness) = keep.as_ref() {
                    Some(read_witness(self.root.as_raw_fd(), witness)?)
                } else {
                    None
                };
                if let Some(backup) = witness_backup.as_ref() {
                    validate_witness(&backup.bytes)?;
                }
                let mut witness_identity = witness_backup.as_ref().map(|backup| backup.identity);
                let mut witness_removed = false;
                let mut attempts_left = TREE_REMOVAL_ATTEMPTS;
                loop {
                    attempts_left -= 1;
                    self.ensure_root_binding()?;
                    match drain_directory(
                        &self,
                        keep.as_ref(),
                        deadline,
                        before_directory_open,
                        &mut entries_reclaimed,
                    ) {
                        Ok(TreeRemoval::Complete) => {}
                        Ok(TreeRemoval::Interrupted) => {
                            if witness_removed {
                                restore_witness(
                                    self.root.as_raw_fd(),
                                    keep.as_ref().expect("removed witness has a name"),
                                    witness_backup
                                        .as_ref()
                                        .expect("removed witness has a backup"),
                                )?;
                            }
                            return Ok(TreeRemoval::Interrupted);
                        }
                        Err(error) => {
                            return Err(restore_after_failure(
                                error,
                                witness_removed,
                                self.root.as_raw_fd(),
                                keep.as_ref(),
                                witness_backup.as_ref(),
                            ));
                        }
                    }

                    if let (Some(witness), Some(identity), Some(backup)) =
                        (keep.as_ref(), witness_identity, witness_backup.as_ref())
                        && !witness_removed
                    {
                        self.ensure_root_binding()?;
                        ensure_named_regular_file(self.root.as_raw_fd(), witness, identity)?;
                        match unlink_at(self.root.as_raw_fd(), witness, false) {
                            Ok(()) => {
                                entries_reclaimed = entries_reclaimed.saturating_add(1);
                            }
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                                witness_removed = true;
                                return Err(restore_after_failure(
                                    changed_entry_error(),
                                    witness_removed,
                                    self.root.as_raw_fd(),
                                    keep.as_ref(),
                                    Some(backup),
                                ));
                            }
                            Err(error) => return Err(error),
                        }
                        witness_removed = true;
                        if let Err(error) = after_witness_unlink() {
                            return Err(restore_after_failure(
                                error,
                                witness_removed,
                                self.root.as_raw_fd(),
                                keep.as_ref(),
                                Some(backup),
                            ));
                        }
                    }

                    let final_unlink = self
                        .ensure_root_binding()
                        .and_then(|()| unlink_at(self.parent.as_raw_fd(), &self.name, true));
                    match final_unlink {
                        Ok(()) => {
                            entries_reclaimed = entries_reclaimed.saturating_add(1);
                            return Ok(TreeRemoval::Complete);
                        }
                        Err(error)
                            if error.kind() == io::ErrorKind::DirectoryNotEmpty
                                && attempts_left > 0 =>
                        {
                            if witness_removed {
                                witness_identity = Some(restore_witness(
                                    self.root.as_raw_fd(),
                                    keep.as_ref().expect("removed witness has a name"),
                                    witness_backup
                                        .as_ref()
                                        .expect("removed witness has a backup"),
                                )?);
                                witness_removed = false;
                            }
                        }
                        Err(error) => {
                            return Err(restore_after_failure(
                                error,
                                witness_removed,
                                self.root.as_raw_fd(),
                                keep.as_ref(),
                                witness_backup.as_ref(),
                            ));
                        }
                    }
                }
            })();
            result
                .map(|outcome| (outcome, entries_reclaimed))
                .map_err(|source| super::TreeRemovalError {
                    source,
                    entries_reclaimed,
                })
        }

        fn ensure_root_binding(&self) -> io::Result<()> {
            ensure_named_directory(self.parent.as_raw_fd(), &self.name, self.object_identity)
        }
    }

    #[derive(Clone)]
    struct DirectoryBinding {
        parent_fd: RawFd,
        name: CString,
        identity: TreeObjectIdentity,
    }

    struct DirectoryFrame {
        // Owns the descriptor named by `directory_fd` for nested frames. The
        // root descriptor is retained by `PinnedTreeRemoval` instead.
        _directory: Option<File>,
        directory_fd: RawFd,
        binding: Option<DirectoryBinding>,
        entries: DirectoryEntries,
    }

    fn drain_directory<F>(
        pinned: &PinnedTreeRemoval,
        keep_until_last: Option<&CString>,
        deadline: Option<Instant>,
        before_directory_open: &mut F,
        entries_reclaimed: &mut usize,
    ) -> io::Result<TreeRemoval>
    where
        F: FnMut() -> io::Result<()>,
    {
        let root_fd = pinned.root.as_raw_fd();
        let mut stack = vec![DirectoryFrame {
            _directory: None,
            directory_fd: root_fd,
            binding: None,
            entries: DirectoryEntries::open(root_fd)?,
        }];
        let mut bindings = Vec::new();

        loop {
            let next_name = stack
                .last_mut()
                .expect("the root directory frame remains until completion")
                .entries
                .next_name()?;
            let Some(name) = next_name else {
                if stack.len() == 1 {
                    return Ok(TreeRemoval::Complete);
                }

                let frame = stack.pop().expect("nested frame was just observed");
                let binding = frame
                    .binding
                    .expect("every nested directory frame has a parent binding");
                bindings.pop();
                let parent_fd = stack
                    .last()
                    .expect("the root frame remains below each nested frame")
                    .directory_fd;
                ensure_bindings(pinned, &bindings)?;
                ensure_named_directory(binding.parent_fd, &binding.name, binding.identity)?;
                match unlink_at(parent_fd, &binding.name, true) {
                    Ok(()) => {
                        *entries_reclaimed = entries_reclaimed.saturating_add(1);
                    }
                    Err(error)
                        if error.kind() == io::ErrorKind::NotFound
                            || error.kind() == io::ErrorKind::DirectoryNotEmpty => {}
                    Err(error) => return Err(error),
                }
                // As before, a pass buys one attempted unlink before
                // consulting its budget.
                if super::super::check_deadline(deadline) {
                    return Ok(TreeRemoval::Interrupted);
                }
                continue;
            };
            if name == OsStr::new(".") || name == OsStr::new("..") {
                continue;
            }
            let c_name = c_name(&name)?;
            if stack.len() == 1
                && keep_until_last.is_some_and(|keep| keep.as_bytes() == c_name.as_bytes())
            {
                continue;
            }

            let directory_fd = stack
                .last()
                .expect("the root directory frame remains until completion")
                .directory_fd;
            let Some(observed) = stat_at(directory_fd, &c_name)? else {
                continue;
            };
            if observed.is_directory {
                before_directory_open()?;
                ensure_bindings(pinned, &bindings)?;
                let child = open_directory_at(directory_fd, &c_name)?;
                let metadata = child.metadata()?;
                let opened = TreeObjectIdentity {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                };
                if opened != observed.identity {
                    return Err(changed_entry_error());
                }
                if opened.device != pinned.object_identity.device
                    || filesystem_identity(child.as_raw_fd())? != pinned.filesystem_identity
                    || mount_identity(child.as_raw_fd())? != pinned.mount_identity
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "refusing to cross a filesystem boundary during tree removal",
                    ));
                }

                let child_binding = DirectoryBinding {
                    parent_fd: directory_fd,
                    name: c_name.clone(),
                    identity: opened,
                };
                let child_fd = child.as_raw_fd();
                let entries = DirectoryEntries::open(child_fd)?;
                if super::super::check_deadline(deadline) {
                    return Ok(TreeRemoval::Interrupted);
                }
                bindings.push(child_binding.clone());
                stack.push(DirectoryFrame {
                    _directory: Some(child),
                    directory_fd: child_fd,
                    binding: Some(child_binding),
                    entries,
                });
            } else {
                ensure_bindings(pinned, &bindings)?;
                ensure_named_entry(directory_fd, &c_name, observed.identity)?;
                match unlink_at(directory_fd, &c_name, false) {
                    Ok(()) => {
                        *entries_reclaimed = entries_reclaimed.saturating_add(1);
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                // As before, a pass buys one attempted unlink before
                // consulting its budget.
                if super::super::check_deadline(deadline) {
                    return Ok(TreeRemoval::Interrupted);
                }
            }
        }
    }

    fn ensure_bindings(
        pinned: &PinnedTreeRemoval,
        bindings: &[DirectoryBinding],
    ) -> io::Result<()> {
        pinned.ensure_root_binding()?;
        for binding in bindings {
            ensure_named_directory(binding.parent_fd, &binding.name, binding.identity)?;
        }
        Ok(())
    }

    fn ensure_named_entry(
        parent_fd: RawFd,
        name: &CString,
        expected: TreeObjectIdentity,
    ) -> io::Result<()> {
        match stat_at(parent_fd, name)? {
            Some(entry) if entry.identity == expected => Ok(()),
            _ => Err(changed_entry_error()),
        }
    }

    fn ensure_named_regular_file(
        parent_fd: RawFd,
        name: &CString,
        expected: TreeObjectIdentity,
    ) -> io::Result<()> {
        match stat_at(parent_fd, name)? {
            Some(entry) if entry.is_regular && entry.identity == expected => Ok(()),
            _ => Err(changed_entry_error()),
        }
    }

    struct WitnessBackup {
        identity: TreeObjectIdentity,
        mode: u32,
        bytes: Vec<u8>,
    }

    fn read_witness(parent_fd: RawFd, name: &CString) -> io::Result<WitnessBackup> {
        let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        // SAFETY: `parent_fd` is a live pinned directory, `name` is one
        // NUL-terminated component, and `O_NOFOLLOW` rejects a symlink leaf.
        let fd = unsafe { libc::openat(parent_fd, name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        let before = file.metadata()?;
        if !before.is_file() || before.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "tree witness must be a regular file with one link",
            ));
        }
        if before.len() > MAX_WITNESS_RESTORE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "tree witness exceeds the bounded restore size",
            ));
        }
        let identity = TreeObjectIdentity {
            device: before.dev(),
            inode: before.ino(),
        };
        let mut reader = file.take(MAX_WITNESS_RESTORE_BYTES + 1);
        let mut bytes = Vec::with_capacity(before.len() as usize);
        reader.read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_WITNESS_RESTORE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "tree witness exceeds the bounded restore size",
            ));
        }
        let after = reader.get_ref().metadata()?;
        if !after.is_file()
            || after.nlink() != 1
            || after.dev() != identity.device
            || after.ino() != identity.inode
            || after.len() != bytes.len() as u64
        {
            return Err(changed_entry_error());
        }
        Ok(WitnessBackup {
            identity,
            mode: before.mode() & 0o7777,
            bytes,
        })
    }

    fn restore_witness(
        parent_fd: RawFd,
        name: &CString,
        backup: &WitnessBackup,
    ) -> io::Result<TreeObjectIdentity> {
        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        // O_EXCL is the final no-overwrite check: a writer-created replacement
        // is left intact rather than replaced with the retained witness bytes.
        // SAFETY: `parent_fd` is a live pinned directory and `name` is one
        // NUL-terminated component. The flags create a new regular file without
        // following a symlink or replacing an existing entry.
        let fd = unsafe { libc::openat(parent_fd, name.as_ptr(), flags, 0o600 as libc::c_uint) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh owned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(&backup.bytes)?;
        // SAFETY: `file` owns a live descriptor for the newly created witness;
        // the mode was captured from the original validated witness.
        if unsafe { libc::fchmod(file.as_raw_fd(), backup.mode as libc::mode_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        file.sync_all()?;
        let metadata = file.metadata()?;
        let identity = TreeObjectIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        drop(file);
        ensure_named_regular_file(parent_fd, name, identity)?;
        Ok(identity)
    }

    fn restore_after_failure(
        error: io::Error,
        witness_removed: bool,
        root_fd: RawFd,
        witness: Option<&CString>,
        backup: Option<&WitnessBackup>,
    ) -> io::Error {
        if !witness_removed {
            return error;
        }
        let Some((witness, backup)) = witness.zip(backup) else {
            return io::Error::new(
                error.kind(),
                format!("{error}; removed witness has no retained restore data"),
            );
        };
        match restore_witness(root_fd, witness, backup) {
            Ok(_) => error,
            Err(restore_error) => io::Error::new(
                error.kind(),
                format!("{error}; could not restore the tree witness: {restore_error}"),
            ),
        }
    }

    fn ensure_named_directory(
        parent_fd: RawFd,
        name: &CString,
        expected: TreeObjectIdentity,
    ) -> io::Result<()> {
        match stat_at(parent_fd, name)? {
            Some(entry) if entry.is_directory && entry.identity == expected => Ok(()),
            _ => Err(changed_entry_error()),
        }
    }

    fn changed_entry_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "tree entry changed while it was being removed",
        )
    }

    struct EntryMetadata {
        identity: TreeObjectIdentity,
        is_directory: bool,
        is_regular: bool,
    }

    fn stat_at(parent_fd: RawFd, name: &CString) -> io::Result<Option<EntryMetadata>> {
        // SAFETY: `stat` is initialized by `fstatat`; `name` is NUL-terminated,
        // and `AT_SYMLINK_NOFOLLOW` requests metadata for the entry itself.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers are valid for this call and the flags prevent
        // following a symlink at `name`.
        let result = unsafe {
            libc::fstatat(
                parent_fd,
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error);
        }
        let mode = stat.st_mode;
        #[cfg(target_os = "linux")]
        let device = stat.st_dev;
        #[cfg(target_os = "macos")]
        let device = u64::try_from(stat.st_dev).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "negative device number in stat result",
            )
        })?;
        Ok(Some(EntryMetadata {
            identity: TreeObjectIdentity {
                device,
                inode: stat.st_ino,
            },
            is_directory: mode & libc::S_IFMT == libc::S_IFDIR,
            is_regular: mode & libc::S_IFMT == libc::S_IFREG,
        }))
    }

    fn unlink_at(parent_fd: RawFd, name: &CString, directory: bool) -> io::Result<()> {
        let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: `parent_fd` remains open for the walk and `name` is a single
        // NUL-terminated component. `unlinkat` never follows a symlink at the
        // final component.
        let result = unsafe { libc::unlinkat(parent_fd, name.as_ptr(), flags) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        Err(error)
    }

    fn open_directory_at(parent_fd: RawFd, name: &CString) -> io::Result<File> {
        let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        // SAFETY: `parent_fd` is an open directory and `name` is a single
        // component. `O_NOFOLLOW|O_DIRECTORY` refuses a symlink replacement.
        let fd = unsafe { libc::openat(parent_fd, name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` returned a fresh owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn open_directory_chain(path: &Path) -> io::Result<File> {
        let mut current = open_root_directory()?;
        for component in path.components() {
            match component {
                Component::Normal(name) => {
                    current = open_directory_at(current.as_raw_fd(), &c_name(name)?)?;
                }
                Component::RootDir | Component::CurDir => continue,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "canonical parent contains an unsafe path component",
                    ));
                }
            }
        }
        Ok(current)
    }

    fn canonical_parent_without_arbitrary_symlinks(path: &Path) -> io::Result<std::path::PathBuf> {
        let canonical = std::fs::canonicalize(path)?;
        if canonical == path {
            return Ok(canonical);
        }

        #[cfg(target_os = "macos")]
        for (alias, target) in [
            (Path::new("/var"), Path::new("/private/var")),
            (Path::new("/tmp"), Path::new("/private/tmp")),
        ] {
            if let Ok(suffix) = path.strip_prefix(alias) {
                let expected = target.join(suffix);
                if canonical == expected {
                    return Ok(canonical);
                }
            }
        }

        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing tree removal through an unsupported symlinked ancestor",
        ))
    }

    fn open_root_directory() -> io::Result<File> {
        let path = CString::new("/").expect("static root path has no NUL");
        let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        // SAFETY: `path` is NUL-terminated and the returned descriptor is
        // immediately owned by `File`.
        let fd = unsafe { libc::open(path.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `open` returned a fresh owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn c_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))
    }

    fn filesystem_identity(fd: RawFd) -> io::Result<Vec<u8>> {
        // SAFETY: `statfs` is a plain C struct and zero initialization is
        // valid; `fstatfs` writes the complete value on success.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is a live directory descriptor and `stat` is writable.
        if unsafe { libc::fstatfs(fd, &mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the initialized `f_fsid` field is an in-bounds byte region.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                std::ptr::addr_of!(stat.f_fsid).cast::<u8>(),
                std::mem::size_of_val(&stat.f_fsid),
            )
        };
        Ok(bytes.to_vec())
    }

    #[cfg(target_os = "linux")]
    fn mount_identity(fd: RawFd) -> io::Result<Vec<u8>> {
        let empty_path = CString::new("").expect("empty path has no NUL");
        // SAFETY: `statx` is zero-initialized, `fd` is a live descriptor, and
        // the empty path with AT_EMPTY_PATH asks for the descriptor itself.
        let mut stat: libc::statx = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers are valid for the duration of the syscall.
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
            return Err(io::Error::last_os_error());
        }
        if stat.stx_mask & libc::STATX_MNT_ID == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "kernel did not provide a mount identity for safe tree removal",
            ));
        }
        Ok(stat.stx_mnt_id.to_ne_bytes().to_vec())
    }

    #[cfg(target_os = "macos")]
    fn mount_identity(fd: RawFd) -> io::Result<Vec<u8>> {
        // f_fsid identifies the volume; f_mntonname distinguishes a second
        // mount of that volume from an ordinary child directory.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatfs(fd, &mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mountpoint = unsafe { CStr::from_ptr(stat.f_mntonname.as_ptr()) }.to_bytes();
        if mountpoint.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "filesystem did not provide a mountpoint identity",
            ));
        }
        Ok(mountpoint.to_vec())
    }

    struct DirectoryEntries(*mut libc::DIR);

    impl DirectoryEntries {
        fn open(fd: RawFd) -> io::Result<Self> {
            // Open `.` relative to the pinned directory to get a fresh file
            // description and directory offset. `dup` would share the old
            // offset, preventing a retry from seeing entries added meanwhile.
            let dot = CString::new(".").expect("dot has no NUL");
            // SAFETY: `fd` is a live directory descriptor and `dot` is a
            // NUL-terminated component. These flags open that directory itself
            // without following a symlink.
            let duplicate = unsafe {
                libc::openat(
                    fd,
                    dot.as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                )
            };
            if duplicate < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `duplicate` is a fresh owned descriptor opened on `.`.
            let stream = unsafe { libc::fdopendir(duplicate) };
            if stream.is_null() {
                let error = io::Error::last_os_error();
                // SAFETY: `fdopendir` did not take ownership on failure.
                unsafe { libc::close(duplicate) };
                return Err(error);
            }
            Ok(Self(stream))
        }

        fn next_name(&mut self) -> io::Result<Option<OsString>> {
            reset_errno();
            // SAFETY: `self.0` is an open `DIR*` owned by this object. The
            // returned entry is copied before the next `readdir` call.
            let entry = unsafe { libc::readdir(self.0) };
            if entry.is_null() {
                let errno = current_errno();
                return if errno == 0 {
                    Ok(None)
                } else {
                    Err(io::Error::from_raw_os_error(errno))
                };
            }
            // SAFETY: `d_name` is NUL-terminated by `readdir` and remains valid
            // until the next call; bytes are copied into owned storage here.
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }
                .to_bytes()
                .to_vec();
            Ok(Some(OsString::from_vec(bytes)))
        }
    }

    impl Drop for DirectoryEntries {
        fn drop(&mut self) {
            // SAFETY: `fdopendir` returned this owned stream exactly once.
            unsafe { libc::closedir(self.0) };
        }
    }

    fn reset_errno() {
        // SAFETY: the platform errno pointer is thread-local and valid.
        unsafe { *errno_location() = 0 };
    }

    fn current_errno() -> libc::c_int {
        // SAFETY: the platform errno pointer is thread-local and valid.
        unsafe { *errno_location() }
    }

    #[cfg(target_os = "linux")]
    unsafe fn errno_location() -> *mut libc::c_int {
        // SAFETY: this is the libc-provided pointer to the calling thread's errno.
        unsafe { libc::__errno_location() }
    }

    #[cfg(target_os = "macos")]
    unsafe fn errno_location() -> *mut libc::c_int {
        // SAFETY: this is the libc-provided pointer to the calling thread's errno.
        unsafe { libc::__error() }
    }

    #[cfg(test)]
    mod tests {
        use std::ffi::OsString;
        use std::fs;
        use std::fs::File;
        use std::io;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::symlink;
        use std::path::Path;
        use std::time::Instant;

        use tempfile::tempdir;

        use super::*;
        use crate::gc::{
            PinnedTreeRemoval as PublicPinnedTreeRemoval, remove_condemned_tree,
            remove_condemned_tree_with_progress,
        };

        fn remove_with_hook<F>(
            root: &Path,
            witness: Option<&str>,
            deadline: Option<Instant>,
            hook: &mut F,
        ) -> io::Result<TreeRemoval>
        where
            F: FnMut() -> io::Result<()>,
        {
            PinnedTreeRemoval::open(root)?.remove(witness, deadline, hook)
        }

        fn remove_with_hooks<F, G>(
            root: &Path,
            witness: Option<&str>,
            deadline: Option<Instant>,
            before_directory_open: &mut F,
            after_witness_unlink: &mut G,
        ) -> io::Result<TreeRemoval>
        where
            F: FnMut() -> io::Result<()>,
            G: FnMut() -> io::Result<()>,
        {
            PinnedTreeRemoval::open(root)?.remove_with_hooks(
                witness,
                deadline,
                before_directory_open,
                after_witness_unlink,
            )
        }

        #[test]
        fn swapping_a_nested_directory_for_a_symlink_does_not_follow_it() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            let child = root.join("child");
            let outside = temp.path().join("outside");
            fs::create_dir_all(&child).unwrap();
            fs::create_dir(&outside).unwrap();
            fs::write(root.join("CACHEDIR.TAG"), b"witness").unwrap();
            fs::write(outside.join("keep.txt"), b"outside data").unwrap();

            let mut swapped = false;
            let mut hook = || {
                if !swapped {
                    fs::remove_dir(&child)?;
                    symlink(&outside, &child)?;
                    swapped = true;
                }
                Ok(())
            };
            let result = remove_with_hook(&root, Some("CACHEDIR.TAG"), None, &mut hook);

            assert!(result.is_err(), "the changed child must stop the removal");
            assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"outside data");
            assert_eq!(fs::read(root.join("CACHEDIR.TAG")).unwrap(), b"witness");
            assert!(
                fs::symlink_metadata(&child)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }

        #[test]
        fn replacing_the_witness_with_a_symlink_does_not_remove_it_as_evidence() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            let child = root.join("child");
            let outside = temp.path().join("outside");
            fs::create_dir_all(&child).unwrap();
            fs::create_dir(&outside).unwrap();
            fs::write(root.join("CACHEDIR.TAG"), b"witness").unwrap();
            fs::write(outside.join("keep.txt"), b"outside data").unwrap();

            let mut swapped = false;
            let mut hook = || {
                if !swapped {
                    fs::remove_file(root.join("CACHEDIR.TAG"))?;
                    symlink(outside.join("keep.txt"), root.join("CACHEDIR.TAG"))?;
                    swapped = true;
                }
                Ok(())
            };
            let result = remove_with_hook(&root, Some("CACHEDIR.TAG"), None, &mut hook);

            assert!(result.is_err(), "the changed witness must stop removal");
            assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"outside data");
            assert!(
                fs::symlink_metadata(root.join("CACHEDIR.TAG"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }

        #[test]
        fn replacing_the_root_after_pin_does_not_remove_the_replacement() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            let moved = temp.path().join("moved-root");
            fs::create_dir(&root).unwrap();
            fs::write(root.join("CACHEDIR.TAG"), b"witness").unwrap();
            fs::write(root.join("payload"), b"original").unwrap();
            let pinned = PublicPinnedTreeRemoval::open(&root).unwrap();
            let identity = pinned.object_identity().unwrap();
            assert_eq!(identity.device, fs::metadata(&root).unwrap().dev());
            assert_eq!(identity.inode, fs::metadata(&root).unwrap().ino());

            fs::rename(&root, &moved).unwrap();
            fs::create_dir(&root).unwrap();
            fs::write(root.join("user-data"), b"preserve").unwrap();
            assert!(pinned.remove(Some("CACHEDIR.TAG"), None).is_err());

            assert_eq!(fs::read(moved.join("payload")).unwrap(), b"original");
            assert_eq!(fs::read(root.join("user-data")).unwrap(), b"preserve");
        }

        #[test]
        fn missing_requested_witness_refuses_before_mutating_the_tree() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            fs::create_dir(&root).unwrap();
            fs::write(root.join("payload"), b"preserve without witness proof").unwrap();

            let error = remove_condemned_tree(&root, Some("CACHEDIR.TAG"), None).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            assert_eq!(
                fs::read(root.join("payload")).unwrap(),
                b"preserve without witness proof"
            );
        }

        #[test]
        fn edited_same_inode_witness_refuses_before_mutating_the_tree() {
            use std::io::Write;
            use std::os::unix::fs::MetadataExt;

            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            let witness = root.join("CACHEDIR.TAG");
            fs::create_dir(&root).unwrap();
            fs::write(&witness, b"registered owner marker").unwrap();
            fs::write(root.join("payload"), b"preserve after marker edit").unwrap();
            let original_inode = fs::metadata(&witness).unwrap().ino();
            let pinned = PublicPinnedTreeRemoval::open(&root).unwrap();

            let mut edited = File::options()
                .write(true)
                .truncate(true)
                .open(&witness)
                .unwrap();
            edited.write_all(b"forged owner marker!").unwrap();
            edited.sync_all().unwrap();
            assert_eq!(fs::metadata(&witness).unwrap().ino(), original_inode);

            let error = pinned
                .remove_verified_witness("CACHEDIR.TAG", None, |bytes| {
                    if bytes == b"registered owner marker" {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "witness differs from the registered owner payload",
                        ))
                    }
                })
                .unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(
                fs::read(root.join("payload")).unwrap(),
                b"preserve after marker edit"
            );
            assert_eq!(fs::read(witness).unwrap(), b"forged owner marker!");
        }

        #[test]
        fn interrupted_removal_reports_only_successful_unlinks() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            fs::create_dir(&root).unwrap();
            let payload = root.join("payload");
            fs::write(&payload, b"remove once").unwrap();

            let (outcome, entries_reclaimed) =
                remove_condemned_tree_with_progress(&root, None, Some(Instant::now())).unwrap();

            assert_eq!(outcome, TreeRemoval::Interrupted);
            assert_eq!(entries_reclaimed, 1);
            assert!(root.is_dir());
            assert!(!payload.exists());
        }

        #[test]
        fn deadline_interrupts_after_progress_and_keeps_the_witness() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            fs::create_dir(&root).unwrap();
            fs::write(root.join("CACHEDIR.TAG"), b"witness").unwrap();
            fs::write(root.join("payload"), b"remove me").unwrap();

            assert_eq!(
                remove_condemned_tree(&root, Some("CACHEDIR.TAG"), Some(Instant::now())).unwrap(),
                TreeRemoval::Interrupted
            );
            assert!(root.join("CACHEDIR.TAG").is_file());
            assert!(!root.join("payload").exists());
        }

        #[test]
        fn expired_deadline_does_not_descend_an_arbitrary_directory_chain() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            fs::create_dir(&root).unwrap();
            fs::write(root.join("CACHEDIR.TAG"), b"witness").unwrap();
            let mut leaf = root.clone();
            for depth in 0..40 {
                leaf.push(format!("d{depth}"));
                fs::create_dir(&leaf).unwrap();
            }
            fs::write(leaf.join("payload"), b"preserve").unwrap();

            let mut directory_opens = 0;
            let mut before_directory_open = || {
                directory_opens += 1;
                Ok(())
            };
            let result = remove_with_hook(
                &root,
                Some("CACHEDIR.TAG"),
                Some(Instant::now()),
                &mut before_directory_open,
            )
            .unwrap();

            assert_eq!(result, TreeRemoval::Interrupted);
            assert_eq!(
                directory_opens, 1,
                "do not descend after the budget expires"
            );
            assert_eq!(fs::read(root.join("CACHEDIR.TAG")).unwrap(), b"witness");
            assert_eq!(fs::read(leaf.join("payload")).unwrap(), b"preserve");
        }

        #[test]
        fn repeated_refills_restore_witness_and_never_report_complete_early() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            fs::create_dir(&root).unwrap();
            fs::write(root.join("CACHEDIR.TAG"), b"validated witness bytes").unwrap();
            let mut refills = 0;
            let mut before_directory_open = || Ok(());
            let mut after_witness_unlink = || {
                refills += 1;
                fs::write(root.join(format!("late-{refills}")), b"concurrent refill")?;
                Ok(())
            };

            let error = remove_with_hooks(
                &root,
                Some("CACHEDIR.TAG"),
                None,
                &mut before_directory_open,
                &mut after_witness_unlink,
            )
            .unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::DirectoryNotEmpty);
            assert_eq!(refills, TREE_REMOVAL_ATTEMPTS);
            assert_eq!(
                fs::read(root.join("CACHEDIR.TAG")).unwrap(),
                b"validated witness bytes"
            );
            assert_eq!(
                fs::read(root.join(format!("late-{refills}"))).unwrap(),
                b"concurrent refill"
            );
        }

        #[test]
        fn witness_restore_never_overwrites_a_writer_replacement() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            let outside = temp.path().join("outside");
            fs::create_dir(&root).unwrap();
            fs::create_dir(&outside).unwrap();
            fs::write(root.join("CACHEDIR.TAG"), b"original witness").unwrap();
            fs::write(outside.join("keep.txt"), b"outside data").unwrap();
            let mut before_directory_open = || Ok(());
            let mut after_witness_unlink = || {
                symlink(outside.join("keep.txt"), root.join("CACHEDIR.TAG"))?;
                Ok(())
            };

            let error = remove_with_hooks(
                &root,
                Some("CACHEDIR.TAG"),
                None,
                &mut before_directory_open,
                &mut after_witness_unlink,
            )
            .unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"outside data");
            assert!(
                fs::symlink_metadata(root.join("CACHEDIR.TAG"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }

        #[test]
        fn oversized_witness_is_refused_before_tree_mutation() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            fs::create_dir(&root).unwrap();
            let witness = File::create(root.join("manifest.json")).unwrap();
            witness.set_len(MAX_WITNESS_RESTORE_BYTES + 1).unwrap();
            fs::write(root.join("payload"), b"preserve before refusal").unwrap();

            let error = remove_condemned_tree(&root, Some("manifest.json"), None).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(
                fs::read(root.join("payload")).unwrap(),
                b"preserve before refusal"
            );
            assert_eq!(
                fs::metadata(root.join("manifest.json")).unwrap().len(),
                MAX_WITNESS_RESTORE_BYTES + 1
            );
        }

        #[test]
        fn ordinary_tree_removal_unlinks_symlink_without_following_target() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            let outside = temp.path().join("outside");
            fs::create_dir(&root).unwrap();
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("keep.txt"), b"outside data").unwrap();
            symlink(&outside, root.join("linked")).unwrap();

            assert_eq!(
                remove_condemned_tree(&root, None, None).unwrap(),
                TreeRemoval::Complete
            );
            assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"outside data");
        }

        #[test]
        fn rescans_use_a_fresh_directory_offset() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            fs::create_dir(&root).unwrap();
            fs::write(root.join("first"), b"one").unwrap();
            let root_handle = File::open(&root).unwrap();

            let mut first_scan = DirectoryEntries::open(root_handle.as_raw_fd()).unwrap();
            while first_scan.next_name().unwrap().is_some() {}
            drop(first_scan);
            fs::write(root.join("appeared-during-retry"), b"two").unwrap();

            let mut retry_scan = DirectoryEntries::open(root_handle.as_raw_fd()).unwrap();
            let mut names = Vec::new();
            while let Some(name) = retry_scan.next_name().unwrap() {
                names.push(name);
            }
            assert!(names.contains(&OsString::from("appeared-during-retry")));
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn mount_identity_distinguishes_procfs_when_mounted() {
            let proc_root = match File::open("/proc") {
                Ok(directory) => directory,
                Err(_) => return,
            };
            let root = File::open("/").unwrap();
            let proc_identity = mount_identity(proc_root.as_raw_fd()).unwrap();
            let root_identity = mount_identity(root.as_raw_fd()).unwrap();
            assert_ne!(proc_identity, root_identity);

            match PinnedTreeRemoval::open(Path::new("/proc")) {
                Err(error) => assert_eq!(error.kind(), io::ErrorKind::PermissionDenied),
                Ok(_) => panic!("refused to pin a mount point as a deletion root"),
            }
        }

        #[test]
        fn missing_leaf_is_complete_but_missing_parent_is_not() {
            let temp = tempdir().unwrap();
            let missing_leaf = temp.path().join("missing-leaf");
            assert_eq!(
                remove_condemned_tree(&missing_leaf, None, None).unwrap(),
                TreeRemoval::Complete
            );

            let missing_parent = temp.path().join("missing-parent").join("root");
            let error = remove_condemned_tree(&missing_parent, None, None).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
        }

        #[test]
        fn refuses_an_arbitrary_symlinked_parent() {
            let temp = tempdir().unwrap();
            let outside = temp.path().join("outside");
            let alias = temp.path().join("alias");
            fs::create_dir(&outside).unwrap();
            fs::create_dir(outside.join("root")).unwrap();
            fs::write(outside.join("root/CACHEDIR.TAG"), b"witness").unwrap();
            symlink(&outside, &alias).unwrap();

            let error =
                remove_condemned_tree(&alias.join("root"), Some("CACHEDIR.TAG"), None).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(
                fs::read(outside.join("root/CACHEDIR.TAG")).unwrap(),
                b"witness"
            );
        }

        #[test]
        fn witness_must_be_a_single_path_component() {
            let temp = tempdir().unwrap();
            let root = temp.path().join("root");
            fs::create_dir(&root).unwrap();
            fs::write(root.join("CACHEDIR.TAG"), b"witness").unwrap();

            let error =
                remove_condemned_tree(&root, Some("nested/CACHEDIR.TAG"), None).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(fs::read(root.join("CACHEDIR.TAG")).unwrap(), b"witness");
        }

        #[cfg(target_os = "macos")]
        #[test]
        fn accepts_the_known_tmp_alias_without_following_other_symlinks() {
            let canonical_tmp = std::fs::canonicalize("/tmp").unwrap();
            if canonical_tmp != Path::new("/private/tmp") {
                return;
            }
            let temp = tempfile::tempdir_in(&canonical_tmp).unwrap();
            let alias_parent = Path::new("/tmp").join(temp.path().file_name().unwrap());
            let root = alias_parent.join("root");
            fs::create_dir(&root).unwrap();
            fs::write(root.join("payload"), b"remove me").unwrap();

            assert_eq!(
                remove_condemned_tree(&root, None, None).unwrap(),
                TreeRemoval::Complete
            );
            assert!(!root.exists());
        }
    }
}
