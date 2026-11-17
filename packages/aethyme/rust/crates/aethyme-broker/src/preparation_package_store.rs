//! Repository-wide package-manager stores shared by every preparation input
//! set, and the link capability between such a store and the checkouts it
//! feeds.
//!
//! The repository-shared preparation cache gives each distinct input set (each
//! preparation digest) its own directory. Package-manager content stores are
//! content addressed, so keying them by the preparation digest only forces a
//! full download and a full new copy of every package whenever any declared
//! input changes: a lockfile edit, a backend requirement, a helper script.
//! The broker therefore keeps one store per repository, platform and manager
//! beside the digest directories, under the same captured cache root, the
//! same repository lease and the same whole-store rotation.
//!
//! The store key is `(cache root, repository identity, OS, architecture,
//! manager directory)`. What else makes a stored file valid is the manager's
//! own business and it already keys on it: pnpm writes under a store-layout
//! version directory (`v10/`) and keys post-install side effects by platform
//! and Node version, uv and pip key wheels by interpreter tags. A lockfile or
//! runtime change therefore never reuses a wrong file; it only adds the files
//! that differ. Worktree outputs (`node_modules/`, `.venv/`) stay keyed by the
//! full preparation digest, runtime identities included.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The repository-level directory holding shared package-manager stores. It
/// sits beside the 12-hex digest directories, which it can never collide with.
pub(crate) const SHARED_PACKAGE_STORE_DIR: &str = "packages";

/// Where a repository's package-manager stores live.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PackageStoreScope {
    /// One store per repository, platform and manager, shared by every input
    /// set. The default.
    #[default]
    Repository,
    /// One store per preparation digest: every distinct input set downloads
    /// and imports its packages afresh. Explicit isolation only.
    PerInput,
}

impl PackageStoreScope {
    pub(crate) fn is_repository(&self) -> bool {
        *self == Self::Repository
    }
}

/// The cheapest way the filesystem lets a package manager import a file from
/// the store into a checkout, in the order pnpm's default `auto` import tries
/// them. This is the filesystem's capability, probed by the broker; the tool
/// may still be configured to choose differently.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PackageImportMethod {
    /// Copy-on-write clone: APFS `clonefile(2)` or Linux `FICLONE`. New
    /// checkouts allocate metadata only.
    Clone,
    /// Hard link: shared bytes, shared inode.
    Hardlink,
    /// Full copy: the store is on another volume, or neither primitive works.
    Copy,
}

/// The components of the shared package-store key, so a reader can see what
/// two sessions must agree on to share it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PackageStoreKey {
    pub repository: String,
    pub os: String,
    pub arch: String,
}

/// One runtime the preparation declares, as the preparation digest saw it.
/// The digest hashes the resolved executable's canonical path, size and
/// modification time; it does not run the probe command.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RuntimeIdentity {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_ns: Option<u64>,
}

/// What one preparation used for package storage, and what it cost the
/// worktree volume. Persisted with the preparation record and exposed by
/// `submit prepare status --json`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PackageStoreReport {
    /// The directory holding the manager stores (`pnpm-store`, `npm-cache`,
    /// `uv-cache`, `pip-cache`).
    pub path: PathBuf,
    pub scope: PackageStoreScope,
    pub key: PackageStoreKey,
    /// Every runtime the preparation digest keyed on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<RuntimeIdentity>,
    /// Whether the store and the checkout share a volume; `None` when it
    /// could not be determined. Sharing is impossible when this is false.
    pub same_volume: Option<bool>,
    pub import_method: PackageImportMethod,
    /// Store names linked from this digest directory to the shared store in
    /// this run (`$AETHYME_PREPARE_CACHE_DIR/<name>` compatibility).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compatibility_links: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_free_bytes_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_free_bytes_after: Option<u64>,
    /// Best effort: the worktree volume's free-space drop across all steps.
    /// Other writers on the volume add to it, and freed space hides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_bytes_allocated: Option<u64>,
}

/// The platform component of the store key, `<os>-<arch>`.
pub(crate) fn platform_component() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The repository-wide store directory for the digest directory `cache_dir`
/// (`<cache root>/<repository>/<digest>`):
/// `<cache root>/<repository>/packages/<os>-<arch>`.
pub(crate) fn shared_store_root(cache_dir: &Path) -> Option<PathBuf> {
    cache_dir.parent().map(|repository| {
        repository
            .join(SHARED_PACKAGE_STORE_DIR)
            .join(platform_component())
    })
}

/// Create `path` as a real directory (one level), refusing a symlink or any
/// other file type in its place.
pub(crate) fn ensure_real_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            io::Error::other(format!("{} is not a real directory", path.display())),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                        Ok(())
                    }
                    _ => Err(io::Error::other(format!(
                        "{} is not a real directory",
                        path.display()
                    ))),
                }
            }
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    }
}

/// Create the shared store for `cache_dir` and one real directory per
/// manager `names`, returning the store root.
pub(crate) fn ensure_shared_store(cache_dir: &Path, names: &[&str]) -> io::Result<PathBuf> {
    let store_root = shared_store_root(cache_dir)
        .ok_or_else(|| io::Error::other("the preparation-cache directory has no parent"))?;
    if let Some(packages) = store_root.parent() {
        ensure_real_directory(packages)?;
    }
    ensure_real_directory(&store_root)?;
    for name in names {
        ensure_real_directory(&store_root.join(name))?;
    }
    Ok(store_root)
}

/// Point each manager directory of the digest directory `cache_dir` at the
/// repository-wide store with a relative symlink, so a command written against
/// `$AETHYME_PREPARE_CACHE_DIR/<name>` (the form `prepare draft` writes)
/// shares the store too. An existing entry is left as it is: a real directory
/// from the earlier layout keeps serving the digest it was made for, and a
/// concurrent preparation of the same digest may have linked it already.
/// Returns the names linked by this call.
pub(crate) fn link_digest_directory(cache_dir: &Path, names: &[&str]) -> io::Result<Vec<String>> {
    ensure_shared_store(cache_dir, names)?;
    let mut linked = Vec::new();
    for name in names {
        let link = cache_dir.join(name);
        match fs::symlink_metadata(&link) {
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let target = Path::new("..")
            .join(SHARED_PACKAGE_STORE_DIR)
            .join(platform_component())
            .join(name);
        match symlink_directory(&target, &link) {
            Ok(()) => linked.push((*name).to_owned()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Ok(linked)
}

#[cfg(unix)]
fn symlink_directory(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink_directory(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory symlinks are unsupported on this platform",
    ))
}

/// Whether `left` and `right` live on one volume (one `st_dev`), or `None`
/// when either cannot be inspected. A package manager can only clone or hard
/// link within one volume.
#[cfg(unix)]
pub(crate) fn same_volume(left: &Path, right: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let left = fs::metadata(left).ok()?;
    let right = fs::metadata(right).ok()?;
    Some(left.dev() == right.dev())
}

#[cfg(not(unix))]
pub(crate) fn same_volume(_left: &Path, _right: &Path) -> Option<bool> {
    None
}

/// Probe, inside `store_root`, the cheapest import the volume supports:
/// clone, then hard link, then copy. A store on another volume than the
/// checkout can only be copied from, whatever the volume supports. The probe
/// writes and removes three small files in the store.
pub(crate) fn probe_import_method(
    store_root: &Path,
    same_volume: Option<bool>,
) -> PackageImportMethod {
    if same_volume == Some(false) {
        return PackageImportMethod::Copy;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let stem = format!(".aethyme-import-probe-{}-{nanos}", std::process::id());
    let source = store_root.join(&stem);
    let cloned = store_root.join(format!("{stem}.clone"));
    let linked = store_root.join(format!("{stem}.link"));
    if fs::write(&source, b"aethyme import probe\n").is_err() {
        return PackageImportMethod::Copy;
    }
    let method = if clone_file(&source, &cloned).is_ok() {
        PackageImportMethod::Clone
    } else if fs::hard_link(&source, &linked).is_ok() {
        PackageImportMethod::Hardlink
    } else {
        PackageImportMethod::Copy
    };
    for path in [&cloned, &linked, &source] {
        remove_probe_file(path);
    }
    method
}

/// Remove one probe file. A file that was never created is fine; any other
/// failure leaves a few bytes behind and is reported, never fatal.
fn remove_probe_file(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        eprintln!(
            "warning: aethyme could not remove import probe {}: {error}",
            path.display()
        );
    }
}

#[cfg(target_os = "macos")]
fn clone_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let source = CString::new(source.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: both arguments are valid NUL-terminated strings that outlive
    // the call; clonefile(2) only reads them.
    if unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn clone_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    /// `FICLONE`, `_IOW(0x94, 9, int)`, spelled out so glibc and musl (whose
    /// `ioctl` request types differ) build the same call.
    const FICLONE: u32 = 0x4004_9409;
    let source_file = fs::File::open(source)?;
    let destination_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    // SAFETY: both descriptors stay open for the duration of the call, and
    // FICLONE takes the source descriptor by value.
    let result = unsafe {
        libc::ioctl(
            destination_file.as_raw_fd(),
            FICLONE as _,
            source_file.as_raw_fd(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        drop(destination_file);
        remove_probe_file(destination);
        Err(error)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_file(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no copy-on-write clone primitive on this platform",
    ))
}

/// Free bytes on the volume holding `path`, or `None` when unknown.
pub(crate) fn volume_free_bytes(path: &Path) -> Option<u64> {
    crate::disk_headroom::available_headroom(path).map(|headroom| headroom.bytes)
}

/// The free-space drop between two readings; `None` when either is unknown.
pub(crate) fn allocated_between(before: Option<u64>, after: Option<u64>) -> Option<u64> {
    Some(before?.saturating_sub(after?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 2] = ["pnpm-store", "uv-cache"];

    fn digest_dir(root: &Path, key: &str) -> PathBuf {
        let path = root.join("repository").join(key);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn shared_store_sits_beside_digest_directories_keyed_by_platform() {
        let root = tempfile::tempdir().unwrap();
        let cache_dir = digest_dir(root.path(), "0123456789ab");
        let store = shared_store_root(&cache_dir).unwrap();
        assert_eq!(
            store,
            root.path()
                .join("repository")
                .join(SHARED_PACKAGE_STORE_DIR)
                .join(platform_component())
        );
        assert!(platform_component().contains(std::env::consts::ARCH));
    }

    #[cfg(unix)]
    #[test]
    fn two_digest_directories_resolve_to_one_store() {
        let root = tempfile::tempdir().unwrap();
        let first = digest_dir(root.path(), "0123456789ab");
        let second = digest_dir(root.path(), "ba9876543210");
        assert_eq!(
            link_digest_directory(&first, &NAMES).unwrap(),
            vec!["pnpm-store".to_string(), "uv-cache".to_string()]
        );
        link_digest_directory(&second, &NAMES).unwrap();
        fs::write(first.join("pnpm-store").join("content"), "package").unwrap();
        assert_eq!(
            fs::read_to_string(second.join("pnpm-store").join("content")).unwrap(),
            "package"
        );
        assert!(
            fs::symlink_metadata(first.join("pnpm-store"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let store = shared_store_root(&first).unwrap().join("pnpm-store");
        assert!(
            !fs::symlink_metadata(&store)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // A second call links nothing new.
        assert!(link_digest_directory(&first, &NAMES).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_real_directory_from_the_earlier_layout_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let cache_dir = digest_dir(root.path(), "0123456789ab");
        fs::create_dir(cache_dir.join("pnpm-store")).unwrap();
        fs::write(cache_dir.join("pnpm-store").join("old"), "kept").unwrap();
        let linked = link_digest_directory(&cache_dir, &NAMES).unwrap();
        assert_eq!(linked, vec!["uv-cache".to_string()]);
        let metadata = fs::symlink_metadata(cache_dir.join("pnpm-store")).unwrap();
        assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
        assert_eq!(
            fs::read_to_string(cache_dir.join("pnpm-store").join("old")).unwrap(),
            "kept"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_store_directory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let cache_dir = digest_dir(root.path(), "0123456789ab");
        fs::create_dir_all(
            root.path()
                .join("repository")
                .join(SHARED_PACKAGE_STORE_DIR),
        )
        .unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), shared_store_root(&cache_dir).unwrap())
            .unwrap();
        assert!(ensure_shared_store(&cache_dir, &NAMES).is_err());
        assert!(link_digest_directory(&cache_dir, &NAMES).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn same_volume_compares_devices() {
        let root = tempfile::tempdir().unwrap();
        let left = root.path().join("left");
        let right = root.path().join("right");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        assert_eq!(same_volume(&left, &right), Some(true));
        assert_eq!(same_volume(&left, &root.path().join("missing")), None);
    }

    #[test]
    fn another_volume_can_only_copy() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            probe_import_method(root.path(), Some(false)),
            PackageImportMethod::Copy
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_prefers_clone_then_hardlink_and_cleans_up() {
        let root = tempfile::tempdir().unwrap();
        let method = probe_import_method(root.path(), Some(true));
        // tmpfs and ext4 cannot clone but can hard link; APFS, btrfs and XFS
        // clone. Copy would mean even hard links failed on one directory.
        assert_ne!(method, PackageImportMethod::Copy);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn allocation_is_the_free_space_drop() {
        assert_eq!(allocated_between(Some(10), Some(4)), Some(6));
        assert_eq!(allocated_between(Some(4), Some(10)), Some(0));
        assert_eq!(allocated_between(None, Some(10)), None);
    }

    #[test]
    fn the_default_scope_is_repository_wide() {
        assert_eq!(PackageStoreScope::default(), PackageStoreScope::Repository);
        assert_eq!(
            serde_json::to_value(PackageStoreScope::PerInput).unwrap(),
            "per_input"
        );
    }
}
