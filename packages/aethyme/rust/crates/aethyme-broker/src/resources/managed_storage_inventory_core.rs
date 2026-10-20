//! Shared bounded traversal core for pinned managed-storage roots.
//!
//! The public owner supplies the exact root binding and keeps its host lease
//! fresh around polls. Each poll budgets one directory read/queue operation
//! or one entry visit; the established deadline is cooperative and cannot
//! interrupt a blocking filesystem syscall. This module owns only the no-follow
//! inventory state; it does not reserve resources or create another owner.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{
    HostResourceError, SCAN_MAX_DEPTH, SCAN_MAX_DURATION, SCAN_MAX_ENTRIES, managed_storage_refusal,
};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::time::Instant;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use sha2::{Digest, Sha256};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use super::{
    cstring_from_os, device_id, ensure_same_identity, file_type, filesystem_identity, fstat,
    get_errno, io_failure, open_absolute_directory_nofollow, openat_directory, openat_regular,
    parse_filesystem_identity, same_identity, scan_refusal, set_errno, stat_at,
};

#[derive(Debug)]
pub(in crate::resources) struct PinnedManagedStorageRoot {
    parent: PathBuf,
    leaf: std::ffi::OsString,
    materialization: String,
    parent_device: u64,
    parent_inode: u64,
    identity: String,
    filesystem_id: String,
}

impl PinnedManagedStorageRoot {
    pub(in crate::resources) fn parent(&self) -> &Path {
        &self.parent
    }

    pub(in crate::resources) fn leaf(&self) -> &std::ffi::OsStr {
        &self.leaf
    }

    pub(in crate::resources) fn materialization(&self) -> &str {
        &self.materialization
    }

    pub(in crate::resources) fn parent_identity(&self) -> (u64, u64) {
        (self.parent_device, self.parent_inode)
    }

    pub(in crate::resources) fn root_identity(&self) -> &str {
        &self.identity
    }

    pub(in crate::resources) fn filesystem_id(&self) -> &str {
        &self.filesystem_id
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl PinnedManagedStorageRoot {
    pub(in crate::resources) fn new(
        preparation_cache_root: &Path,
        repository: &str,
        materialization: &str,
    ) -> Result<Self, HostResourceError> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::ffi::OsStrExt as _;

        let path = preparation_cache_root
            .join(repository)
            .join(materialization);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|source| HostResourceError::Io {
                    path: PathBuf::from("."),
                    source,
                })?
                .join(path)
        };
        let leaf = absolute.file_name().ok_or_else(|| {
            managed_storage_refusal(
                "managed_storage_root_invalid",
                "managed root must name a directory below a parent",
            )
        })?;
        if leaf == "." || leaf == ".." {
            return Err(managed_storage_refusal(
                "managed_storage_root_invalid",
                "managed root leaf cannot be dot or dot-dot",
            ));
        }
        let canonical_store = std::fs::canonicalize(preparation_cache_root).map_err(|source| {
            HostResourceError::Io {
                path: preparation_cache_root.to_path_buf(),
                source,
            }
        })?;
        let store_fd = open_absolute_directory_nofollow(&canonical_store)?;
        let store_stat = fstat(store_fd.as_raw_fd(), &canonical_store)?;
        if file_type(&store_stat) != libc::S_IFDIR {
            return Err(managed_storage_refusal(
                "managed_storage_root_invalid",
                "preparation cache root must resolve to a directory",
            ));
        }
        let repository_name = std::ffi::CString::new(repository)
            .map_err(|_| scan_refusal("repository identity contained an invalid NUL"))?;
        let repository_stat = match stat_at(
            store_fd.as_raw_fd(),
            &repository_name,
            libc::AT_SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) if file_type(&stat) == libc::S_IFDIR && stat.st_dev == store_stat.st_dev => {
                stat
            }
            Ok(_) => {
                return Err(managed_storage_refusal(
                    "managed_storage_repository_root_invalid",
                    "repository cache root must be a non-symlink directory on the preparation-cache filesystem",
                ));
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                return Err(managed_storage_refusal(
                    "managed_storage_repository_root_missing",
                    "repository cache root must be created under its exact exclusive lease before reserving a materialization",
                ));
            }
            Err(source) => {
                return Err(HostResourceError::Io {
                    path: canonical_store.join(repository),
                    source,
                });
            }
        };
        let repository_fd = openat_directory(
            store_fd.as_raw_fd(),
            &repository_name,
            &canonical_store.join(repository),
        )?;
        let opened_repository_stat =
            fstat(repository_fd.as_raw_fd(), &canonical_store.join(repository))?;
        if !same_identity(&repository_stat, &opened_repository_stat) {
            return Err(scan_refusal(
                "repository cache root changed while it was being opened",
            ));
        }
        let canonical_parent = canonical_store.join(repository);
        let parent_fd = open_absolute_directory_nofollow(&canonical_parent)?;
        let parent_stat = fstat(parent_fd.as_raw_fd(), &canonical_parent)?;
        if !same_identity(&opened_repository_stat, &parent_stat) {
            return Err(scan_refusal(
                "repository cache root changed while its parent binding was established",
            ));
        }
        let leaf_string = leaf.to_os_string();
        let leaf_c = cstring_from_os(&leaf_string)?;
        match stat_at(parent_fd.as_raw_fd(), &leaf_c, libc::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) if file_type(&stat) == libc::S_IFDIR && stat.st_dev == parent_stat.st_dev => {}
            Ok(_) => {
                return Err(managed_storage_refusal(
                    "managed_storage_root_invalid",
                    "existing managed root must be a non-symlink directory on its parent filesystem",
                ));
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            Err(source) => {
                return Err(HostResourceError::Io {
                    path: canonical_parent.join(&leaf_string),
                    source,
                });
            }
        }

        let mut path_bytes = canonical_parent.as_os_str().as_bytes().to_vec();
        path_bytes.push(b'/');
        path_bytes.extend_from_slice(leaf_string.as_bytes());
        let mut hash = Sha256::new();
        hash.update(b"aethyme-managed-storage-root-v1\0");
        hash.update(&path_bytes);
        let identity = format!("path-v1:{:x}", hash.finalize());
        let filesystem_id = filesystem_identity(device_id(&parent_stat));
        Ok(Self {
            parent: canonical_parent,
            leaf: leaf_string,
            materialization: materialization.to_owned(),
            parent_device: device_id(&parent_stat),
            parent_inode: parent_stat.st_ino as u64,
            identity,
            filesystem_id,
        })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl PinnedManagedStorageRoot {
    pub(in crate::resources) fn new(
        _preparation_cache_root: &Path,
        _repository: &str,
        _materialization: &str,
    ) -> Result<Self, HostResourceError> {
        Err(managed_storage_refusal(
            "managed_storage_inventory_unsupported",
            "bounded managed-storage inventory is currently supported only on macOS and Linux",
        ))
    }
}

#[derive(Clone, Copy, Debug)]
pub(in crate::resources) struct ScanLimits {
    max_entries: usize,
    max_depth: usize,
    max_duration: Duration,
}

impl ScanLimits {
    pub(in crate::resources) fn new(
        max_entries: usize,
        max_depth: usize,
        max_duration: Duration,
    ) -> Result<Self, HostResourceError> {
        if max_entries == 0
            || max_entries > SCAN_MAX_ENTRIES
            || max_depth > SCAN_MAX_DEPTH
            || max_duration.is_zero()
            || max_duration > SCAN_MAX_DURATION
        {
            return Err(HostResourceError::InvalidRequest(
                "managed-storage scan limits must be positive and cannot exceed the established entry, depth, or duration ceilings".into(),
            ));
        }
        Ok(Self {
            max_entries,
            max_depth,
            max_duration,
        })
    }

    pub(in crate::resources) fn existing_owner() -> Self {
        Self::new(SCAN_MAX_ENTRIES, SCAN_MAX_DEPTH, SCAN_MAX_DURATION)
            .expect("established managed-storage scan limits are valid")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::resources) struct TreeInventory {
    allocated_bytes: u64,
    root_instance_id: Option<String>,
    entry_count: usize,
    inventory_digest: String,
    root_present: bool,
}

impl TreeInventory {
    pub(in crate::resources) fn allocated_bytes(&self) -> u64 {
        self.allocated_bytes
    }

    pub(in crate::resources) fn root_instance_id(&self) -> Option<&str> {
        self.root_instance_id.as_deref()
    }

    pub(in crate::resources) fn entry_count(&self) -> usize {
        self.entry_count
    }

    pub(in crate::resources) fn inventory_digest(&self) -> &str {
        &self.inventory_digest
    }

    pub(in crate::resources) fn root_present(&self) -> bool {
        self.root_present
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::resources) enum InventoryScanPoll {
    Pending,
    Complete(TreeInventory),
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod unix {
    use super::*;
    use std::cmp::Reverse;
    use std::collections::{BinaryHeap, HashSet};
    use std::ffi::{CStr, CString, OsStr};
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    pub(in crate::resources) struct InventoryScan<'a> {
        root: &'a PinnedManagedStorageRoot,
        parent_fd: OwnedFd,
        parent_before: libc::stat,
        leaf_c: CString,
        leaf_path: PathBuf,
        initial_root_stat: Option<libc::stat>,
        root_instance_id: Option<String>,
        scanner: TreeScanner,
        finished: bool,
    }

    impl<'a> InventoryScan<'a> {
        pub(in crate::resources) fn begin(
            root: &'a PinnedManagedStorageRoot,
            limits: ScanLimits,
        ) -> Result<Self, HostResourceError> {
            use super::open_absolute_directory_nofollow;

            let parent_fd = open_absolute_directory_nofollow(root.parent())?;
            let leaf_c = super::cstring_from_os(root.leaf())?;
            let parent_before = super::fstat(parent_fd.as_raw_fd(), root.parent())?;
            let leaf_path = root.parent().join(root.leaf());
            let initial_root_stat =
                match super::stat_at(parent_fd.as_raw_fd(), &leaf_c, libc::AT_SYMLINK_NOFOLLOW) {
                    Ok(stat) => Some(stat),
                    Err(error) if error.raw_os_error() == Some(libc::ENOENT) => None,
                    Err(source) => {
                        return Err(HostResourceError::Io {
                            path: leaf_path.clone(),
                            source,
                        });
                    }
                };
            let expected_device = super::parse_filesystem_identity(root.filesystem_id())?;
            if super::device_id(&parent_before) != expected_device
                || super::device_id(&parent_before) != root.parent_identity().0
                || parent_before.st_ino != root.parent_identity().1
            {
                return Err(managed_storage_refusal(
                    "managed_storage_filesystem_changed",
                    "managed root parent identity or filesystem changed after owner creation",
                ));
            }

            let mut scanner = TreeScanner::new(expected_device, limits, Instant::now());
            let mut root_instance_id = None;
            if let Some(root_stat) = initial_root_stat.as_ref() {
                if super::file_type(root_stat) != libc::S_IFDIR
                    || super::device_id(root_stat) != expected_device
                {
                    return Err(managed_storage_refusal(
                        "managed_storage_root_invalid",
                        "managed root must remain a directory on the owner's filesystem",
                    ));
                }
                let root_fd = super::openat_directory(parent_fd.as_raw_fd(), &leaf_c, &leaf_path)?;
                let opened_stat = super::fstat(root_fd.as_raw_fd(), &leaf_path)?;
                super::ensure_same_identity(
                    root_stat,
                    &opened_stat,
                    "managed root changed while it was being opened",
                )?;
                root_instance_id = Some(format!(
                    "unix-inode-v1:{}:{}",
                    opened_stat.st_dev, opened_stat.st_ino
                ));
                scanner.push_directory(root_fd, leaf_path.clone(), 0, Some(&opened_stat))?;
            }

            Ok(Self {
                root,
                parent_fd,
                parent_before,
                leaf_c,
                leaf_path,
                initial_root_stat,
                root_instance_id,
                scanner,
                finished: false,
            })
        }

        /// Perform at most `work_budget` directory-read/queue or entry-visit
        /// operations. Ordered names use a bounded min-heap, so a poll never
        /// performs an unbounded whole-directory sort.
        ///
        /// `Pending` retains every open descriptor and traversal position. A
        /// caller holding a host lease must renew it between polls as needed.
        pub(in crate::resources) fn poll(
            &mut self,
            work_budget: usize,
        ) -> Result<InventoryScanPoll, HostResourceError> {
            if self.finished {
                return Err(super::scan_refusal(
                    "managed inventory scan was polled after completion or refusal",
                ));
            }
            if work_budget == 0 {
                self.finished = true;
                return Err(super::scan_refusal(
                    "managed inventory poll work budget must be positive",
                ));
            }

            match self.poll_steps(work_budget) {
                Ok(result) => Ok(result),
                Err(error) => {
                    // A failed step may already have popped a name or consumed a
                    // directory entry. Never let a later poll complete a receipt
                    // from that partially advanced state.
                    self.finished = true;
                    Err(error)
                }
            }
        }

        fn poll_steps(
            &mut self,
            work_budget: usize,
        ) -> Result<InventoryScanPoll, HostResourceError> {
            let mut work = 0;
            while work < work_budget {
                self.scanner.check_deadline(&self.leaf_path)?;
                if !self.scanner.poll_one()? {
                    let tree = self.finish()?;
                    self.finished = true;
                    return Ok(InventoryScanPoll::Complete(tree));
                }
                work += 1;
            }
            self.scanner.check_deadline(&self.leaf_path)?;
            Ok(InventoryScanPoll::Pending)
        }

        fn finish(&mut self) -> Result<TreeInventory, HostResourceError> {
            use super::open_absolute_directory_nofollow;

            let parent_after = super::fstat(self.parent_fd.as_raw_fd(), self.root.parent())?;
            let named_parent_fd = open_absolute_directory_nofollow(self.root.parent())?;
            let named_parent_after = super::fstat(named_parent_fd.as_raw_fd(), self.root.parent())?;
            if !super::same_identity(&self.parent_before, &parent_after)
                || !super::same_identity(&self.parent_before, &named_parent_after)
            {
                return Err(super::scan_refusal(
                    "managed root parent changed during inventory",
                ));
            }

            match self.initial_root_stat.as_ref() {
                Some(opened_stat) => {
                    let named_after = super::stat_at(
                        self.parent_fd.as_raw_fd(),
                        &self.leaf_c,
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                    .map_err(|source| HostResourceError::Io {
                        path: self.leaf_path.clone(),
                        source,
                    })?;
                    super::ensure_same_identity(
                        opened_stat,
                        &named_after,
                        "managed root name changed during inventory",
                    )?;
                }
                None => match super::stat_at(
                    self.parent_fd.as_raw_fd(),
                    &self.leaf_c,
                    libc::AT_SYMLINK_NOFOLLOW,
                ) {
                    Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
                    _ => {
                        return Err(super::scan_refusal(
                            "managed root appeared during absent-root inventory",
                        ));
                    }
                },
            }

            let mut receipt_hash = self.scanner.digest.clone();
            receipt_hash.update(self.root.parent().as_os_str().as_bytes());
            receipt_hash.update(self.root.leaf().as_bytes());
            receipt_hash.update(self.scanner.allocated_bytes.to_le_bytes());
            receipt_hash.update((self.scanner.entry_count as u64).to_le_bytes());
            if let Some(root_instance_id) = &self.root_instance_id {
                receipt_hash.update(root_instance_id.as_bytes());
            }
            Ok(TreeInventory {
                allocated_bytes: self.scanner.allocated_bytes,
                root_instance_id: self.root_instance_id.clone(),
                entry_count: self.scanner.entry_count.max(1),
                inventory_digest: format!("{:x}", receipt_hash.finalize()),
                root_present: self.initial_root_stat.is_some(),
            })
        }
    }

    struct TreeScanner {
        device: u64,
        limits: ScanLimits,
        deadline: Instant,
        entry_count: usize,
        pending_entries: usize,
        allocated_bytes: u64,
        seen_inodes: HashSet<(u64, u64)>,
        digest: Sha256,
        frames: Vec<DirectoryFrame>,
    }

    impl TreeScanner {
        fn new(device: u64, limits: ScanLimits, started_at: Instant) -> Self {
            Self {
                device,
                limits,
                deadline: started_at + limits.max_duration,
                entry_count: 0,
                pending_entries: 0,
                allocated_bytes: 0,
                seen_inodes: HashSet::new(),
                digest: Sha256::new(),
                frames: Vec::new(),
            }
        }

        fn push_directory(
            &mut self,
            fd: OwnedFd,
            path: PathBuf,
            depth: usize,
            expected: Option<&libc::stat>,
        ) -> Result<(), HostResourceError> {
            if depth > self.limits.max_depth {
                return Err(super::scan_refusal("directory depth limit exceeded"));
            }
            self.check_deadline(&path)?;
            let dir_stat = super::fstat(fd.as_raw_fd(), &path)?;
            if super::device_id(&dir_stat) != self.device
                || super::file_type(&dir_stat) != libc::S_IFDIR
            {
                return Err(super::scan_refusal(
                    "directory crossed filesystem or changed type",
                ));
            }
            if expected.is_some_and(|expected| !super::same_identity(expected, &dir_stat)) {
                return Err(super::scan_refusal(
                    "directory identity changed before enumeration",
                ));
            }
            self.count_stat(&dir_stat, b".")?;

            // SAFETY: `fd` is the live directory descriptor checked above;
            // `fcntl` duplicates it and returns either an owned descriptor or -1.
            let duplicate = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
            if duplicate < 0 {
                return Err(super::io_failure(&path));
            }
            // SAFETY: `duplicate` is fresh; `fdopendir` takes ownership on
            // success, and the error path below closes it.
            let stream = unsafe { libc::fdopendir(duplicate) };
            if stream.is_null() {
                // SAFETY: `fdopendir` failed and did not take ownership.
                unsafe { libc::close(duplicate) };
                return Err(super::io_failure(&path));
            }
            self.frames.push(DirectoryFrame {
                fd,
                stream: Some(DirectoryStream(stream)),
                path,
                depth,
                initial_stat: dir_stat,
                names: BinaryHeap::new(),
                enumerated: false,
            });
            Ok(())
        }

        fn poll_one(&mut self) -> Result<bool, HostResourceError> {
            let Some(frame_index) = self.frames.len().checked_sub(1) else {
                return Ok(false);
            };
            let path = self.frames[frame_index].path.clone();
            self.check_deadline(&path)?;

            if !self.frames[frame_index].enumerated {
                let next_name = self.frames[frame_index].next_name()?;
                match next_name {
                    Some(name) if name == b"." || name == b".." => {}
                    Some(name) => {
                        if self
                            .entry_count
                            .checked_add(self.pending_entries)
                            .and_then(|count| count.checked_add(1))
                            .is_none_or(|count| count > self.limits.max_entries)
                        {
                            return Err(super::scan_refusal("entry limit exceeded"));
                        }
                        self.pending_entries += 1;
                        self.frames[frame_index].names.push(Reverse(name));
                    }
                    None => {
                        let frame = &mut self.frames[frame_index];
                        frame.stream = None;
                        frame.enumerated = true;
                    }
                }
                self.check_deadline(&path)?;
                return Ok(true);
            }

            if let Some(Reverse(name_bytes)) = self.frames[frame_index].names.pop() {
                self.pending_entries -= 1;
                let name = CString::new(name_bytes)
                    .map_err(|_| super::scan_refusal("directory entry contained an invalid NUL"))?;
                let parent_fd = self.frames[frame_index].fd.as_raw_fd();
                let parent_path = self.frames[frame_index].path.clone();
                let depth = self.frames[frame_index].depth;
                let child_stat = super::stat_at(parent_fd, &name, libc::AT_SYMLINK_NOFOLLOW)
                    .map_err(|_| {
                        super::scan_refusal(
                            "entry metadata could not be read without following links",
                        )
                    })?;
                if super::device_id(&child_stat) != self.device {
                    return Err(super::scan_refusal("entry crossed onto another filesystem"));
                }
                let child_path = parent_path.join(OsStr::from_bytes(name.as_bytes()));
                if super::file_type(&child_stat) == libc::S_IFDIR {
                    let child = super::openat_directory(parent_fd, &name, &child_path)?;
                    let opened_stat = super::fstat(child.as_raw_fd(), &child_path)?;
                    super::ensure_same_identity(
                        &child_stat,
                        &opened_stat,
                        "directory entry changed while opening",
                    )?;
                    self.push_directory(child, child_path, depth + 1, Some(&opened_stat))?;
                } else {
                    self.count_stat(&child_stat, name.as_bytes())?;
                    if super::file_type(&child_stat) == libc::S_IFREG {
                        let child = super::openat_regular(parent_fd, &name, &child_path)?;
                        let opened_stat = super::fstat(child.as_raw_fd(), &child_path)?;
                        super::ensure_same_identity(
                            &child_stat,
                            &opened_stat,
                            "regular file changed while opening",
                        )?;
                    }
                }
                return Ok(true);
            }

            let frame = self.frames.pop().expect("the last frame was present");
            let after = super::fstat(frame.fd.as_raw_fd(), &frame.path)?;
            if !super::same_identity(&frame.initial_stat, &after) {
                return Err(super::scan_refusal("directory changed during inventory"));
            }
            Ok(true)
        }

        fn count_stat(&mut self, stat: &libc::stat, name: &[u8]) -> Result<(), HostResourceError> {
            if self
                .seen_inodes
                .insert((super::device_id(stat), stat.st_ino))
            {
                let blocks = u64::try_from(stat.st_blocks)
                    .map_err(|_| super::scan_refusal("negative allocated block count"))?;
                let bytes = blocks
                    .checked_mul(512)
                    .ok_or_else(|| super::scan_refusal("allocated byte count overflowed"))?;
                self.allocated_bytes = self
                    .allocated_bytes
                    .checked_add(bytes)
                    .ok_or_else(|| super::scan_refusal("total allocated byte count overflowed"))?;
            }
            self.entry_count = self
                .entry_count
                .checked_add(1)
                .ok_or_else(|| super::scan_refusal("entry count overflowed"))?;
            self.digest.update((name.len() as u64).to_le_bytes());
            self.digest.update(name);
            self.digest.update(super::device_id(stat).to_le_bytes());
            self.digest.update(stat.st_ino.to_le_bytes());
            self.digest.update((stat.st_mode as u64).to_le_bytes());
            self.digest.update(stat.st_size.to_le_bytes());
            self.digest.update(stat.st_blocks.to_le_bytes());
            if self.entry_count > self.limits.max_entries {
                return Err(super::scan_refusal("entry limit exceeded"));
            }
            Ok(())
        }

        fn check_deadline(&self, path: &Path) -> Result<(), HostResourceError> {
            if Instant::now() >= self.deadline {
                return Err(super::scan_refusal(&format!(
                    "inventory exceeded its bounded deadline at {}",
                    path.display()
                )));
            }
            Ok(())
        }
    }

    struct DirectoryFrame {
        fd: OwnedFd,
        stream: Option<DirectoryStream>,
        path: PathBuf,
        depth: usize,
        initial_stat: libc::stat,
        names: BinaryHeap<Reverse<Vec<u8>>>,
        enumerated: bool,
    }

    impl DirectoryFrame {
        fn next_name(&mut self) -> Result<Option<Vec<u8>>, HostResourceError> {
            let stream = self
                .stream
                .as_ref()
                .expect("an unenumerated frame owns its directory stream");
            super::set_errno(0);
            // SAFETY: the frame uniquely owns this DIR pointer and the stream
            // remains live until enumeration completes.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                if super::get_errno() != 0 {
                    return Err(super::io_failure(&self.path));
                }
                return Ok(None);
            }
            // SAFETY: `readdir` returned a live entry whose `d_name` remains
            // valid until the next call on this stream.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            Ok(Some(name.to_vec()))
        }
    }

    struct DirectoryStream(*mut libc::DIR);

    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            // SAFETY: this wrapper uniquely owns the non-null DIR pointer.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(in crate::resources) use unix::InventoryScan;
