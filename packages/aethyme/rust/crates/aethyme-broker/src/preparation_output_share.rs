//! Share the bytes of prepared worktree outputs between checkouts by
//! copy-on-write clone, and measure what each prepared checkout still owns.
//!
//! Package managers that import from a content store (pnpm's `auto` import,
//! uv's clone or hard-link modes) already share bytes between checkouts. A
//! tool that cannot, such as `pip install` into a virtual environment, writes
//! a private copy of every file into every checkout. On the busiest machine a
//! prepared checkout's Python environment was 574 MB, all of it private, and
//! 81% of its bytes were byte-identical to the same paths in another
//! checkout, while its `node_modules/` (1.7 GB, cloned by pnpm) owned 2.5 MB.
//!
//! After the steps succeed, this pass walks each output root and, for every
//! regular file of at least [`MIN_SHARED_FILE_BYTES`] that does not already
//! share its blocks, looks its SHA-256 up in a content-addressed pool kept
//! inside the repository's package store:
//!
//! * present: the file is replaced by a clone of the pool entry, which has the
//!   same bytes; the file keeps its own mode and timestamps;
//! * absent: the file is cloned into the pool, so the next checkout's
//!   identical file is shared with it.
//!
//! Correct for any tool, because only byte-identical content is substituted
//! and a clone is copy-on-write: a later write to the checkout's file never
//! reaches the pool or another checkout. Hard links are never used for that
//! reason. Files with more than one link are left alone (they are already
//! shared, or the tool relies on the link). The pass only runs where the
//! filesystem clones within the volume holding both the checkout and the pool;
//! anywhere else it reports why it was skipped and changes nothing.
//!
//! A root whose sampled large files all already share their blocks (a pnpm
//! `node_modules/` imported by clone) is skipped after the sample, so the
//! pass does not walk tens of thousands of files a package manager already
//! shares.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::package_store::{self, PackageImportMethod};

/// The pool's directory inside the repository's package store
/// (`<repository>/packages/<os>-<arch>/output-pool`), so it shares the
/// store's lease, cap and whole-store rotation.
pub(crate) const OUTPUT_POOL_DIR: &str = "output-pool";

/// Smaller files are measured but never substituted. On the busiest
/// machine's Python environment, files of 64 KiB and more held 66% of the
/// bytes in 3% of the files, and 92% of those bytes were shareable: past
/// that size each substitution's fixed metadata cost (clone, mode, times,
/// rename) is small against the bytes it saves.
pub(crate) const MIN_SHARED_FILE_BYTES: u64 = 64 * 1024;

/// How long one preparation may spend sharing and measuring its outputs.
pub(crate) const OUTPUT_SHARE_BUDGET: Duration = Duration::from_secs(90);

/// Large files sampled from a root before deciding the tool already shares
/// it.
const ALREADY_SHARED_SAMPLE: usize = 32;

/// How often the pass calls back so the caller can renew its lease.
const TICK_INTERVAL: Duration = Duration::from_secs(5);

/// What the pass did with the outputs.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputShareState {
    /// Every root was walked to the end.
    Complete,
    /// The pass stopped early (budget, lease, an unsupported clone); what it
    /// did stands and the rest of the outputs keep their own copies.
    Partial,
    /// Nothing was attempted; `fallback_reason` says why.
    Skipped,
}

/// How the unique-byte figures were obtained.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UniqueBytesMethod {
    /// The filesystem reported each file's unshared blocks (APFS private
    /// size, Linux FIEMAP shared-extent flags).
    BlockSharing,
    /// At least one file could not be asked; its allocated size was counted,
    /// so the figure is an upper bound.
    AllocatedUpperBound,
}

/// One preparation's output sharing, persisted with the package-store report
/// and attached to the `submit prepare` command metric.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct OutputShareReport {
    pub state: Option<OutputShareState>,
    /// Why the pass was skipped or stopped early.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// Output roots, relative to the checkout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<String>,
    /// Roots skipped because their sampled large files already shared their
    /// blocks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots_already_shared: Vec<String>,
    pub files_scanned: u64,
    pub bytes_scanned: u64,
    pub files_already_shared: u64,
    pub files_cloned_from_pool: u64,
    pub bytes_cloned_from_pool: u64,
    pub files_added_to_pool: u64,
    pub bytes_added_to_pool: u64,
    /// Candidates left alone: more than one link, or changed while hashed.
    pub files_left_alone: u64,
    pub files_failed: u64,
    /// Bytes the walked roots owned alone before and after the pass. Files
    /// added to the pool count as shared afterwards: the pool holds one copy
    /// for every later checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique_bytes_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique_bytes_after: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique_bytes_method: Option<UniqueBytesMethod>,
    pub elapsed_ms: u64,
}

impl OutputShareReport {
    pub(crate) fn skipped(reason: impl Into<String>) -> Self {
        Self {
            state: Some(OutputShareState::Skipped),
            fallback_reason: Some(reason.into()),
            ..Self::default()
        }
    }

    /// The command-metric form: counters and the reason, not the paths.
    pub(crate) fn metric(&self, import_method: PackageImportMethod) -> serde_json::Value {
        serde_json::json!({
            "import_method": import_method,
            "state": self.state,
            "fallback_reason": self.fallback_reason,
            "roots": self.roots.len(),
            "roots_already_shared": self.roots_already_shared.len(),
            "files_scanned": self.files_scanned,
            "bytes_scanned": self.bytes_scanned,
            "files_already_shared": self.files_already_shared,
            "files_cloned_from_pool": self.files_cloned_from_pool,
            "bytes_cloned_from_pool": self.bytes_cloned_from_pool,
            "files_added_to_pool": self.files_added_to_pool,
            "bytes_added_to_pool": self.bytes_added_to_pool,
            "files_left_alone": self.files_left_alone,
            "files_failed": self.files_failed,
            "unique_bytes_before": self.unique_bytes_before,
            "unique_bytes_after": self.unique_bytes_after,
            "unique_bytes_method": self.unique_bytes_method,
            "elapsed_ms": self.elapsed_ms,
        })
    }
}

/// The outermost Git-ignored directory holding each declared output, relative
/// to the checkout: `node_modules/` gives `node_modules`, and
/// `backend/.venv/bin/pytest` gives `backend/.venv` when `backend` is tracked
/// and `.venv` is ignored. Tracked paths are never a root, so a pass never
/// rewrites a tracked file. Nested roots collapse into the outer one.
pub(crate) fn output_roots<'a>(
    worktree: &Path,
    outputs: impl IntoIterator<Item = &'a str>,
    mut is_ignored: impl FnMut(&str) -> bool,
) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    for output in outputs {
        let relative = output.trim_end_matches(['/', '\\']);
        let components = relative
            .split('/')
            .filter(|component| !component.is_empty())
            .collect::<Vec<_>>();
        if components
            .iter()
            .any(|component| *component == ".." || *component == ".")
        {
            continue;
        }
        for depth in 1..=components.len() {
            let candidate = components[..depth].join("/");
            let Ok(metadata) = fs::symlink_metadata(worktree.join(&candidate)) else {
                break;
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                break;
            }
            if is_ignored(&candidate) {
                roots.push(candidate);
                break;
            }
        }
    }
    roots.sort();
    roots.dedup();
    let mut outermost: Vec<String> = Vec::new();
    for root in roots {
        if !outermost
            .iter()
            .any(|outer| root.starts_with(&format!("{outer}/")))
        {
            outermost.push(root);
        }
    }
    outermost
}

/// Everything the pass needs to know about where it runs.
pub(crate) struct OutputShareRequest<'a> {
    pub(crate) worktree: &'a Path,
    /// Relative to `worktree`, from [`output_roots`].
    pub(crate) roots: Vec<String>,
    /// The package store holding the pool.
    pub(crate) store: &'a Path,
    /// What the probe found the volume supports between store and checkout.
    pub(crate) import_method: PackageImportMethod,
    pub(crate) budget: Duration,
}

/// Why sharing cannot run at all, or `None` when it can.
pub(crate) fn skip_reason(import_method: PackageImportMethod, roots: &[String]) -> Option<String> {
    match import_method {
        PackageImportMethod::Clone if roots.is_empty() => {
            Some("no declared output lies in a Git-ignored directory".into())
        }
        PackageImportMethod::Clone => None,
        PackageImportMethod::Hardlink => Some(
            "the volume hard-links but cannot clone; a shared hard link would let one checkout's write reach the others"
                .into(),
        ),
        PackageImportMethod::Copy => Some(
            "the package store cannot clone into this checkout (another volume, or no clone primitive)"
                .into(),
        ),
    }
}

/// Run the pass. `tick` is called about every five seconds and between
/// roots; returning `false` stops the pass (the caller lost its lease).
pub(crate) fn share_outputs(
    request: &OutputShareRequest<'_>,
    tick: &mut dyn FnMut() -> bool,
) -> OutputShareReport {
    let started = Instant::now();
    if let Some(reason) = skip_reason(request.import_method, &request.roots) {
        let mut report = OutputShareReport::skipped(reason);
        report.roots = request.roots.clone();
        return report;
    }
    let pool = request.store.join(OUTPUT_POOL_DIR);
    if let Err(error) = package_store::ensure_real_directory(&pool) {
        let mut report =
            OutputShareReport::skipped(format!("the output pool is unusable: {error}"));
        report.roots = request.roots.clone();
        return report;
    }
    let mut pass = Pass {
        pool,
        deadline: started + request.budget,
        last_tick: started,
        report: OutputShareReport {
            roots: request.roots.clone(),
            ..OutputShareReport::default()
        },
        unique_before: 0,
        unique_after: 0,
        estimated: false,
        stop: None,
        temp_counter: 0,
    };
    for root in &request.roots {
        if pass.stop.is_some() {
            break;
        }
        if !tick() {
            pass.stop = Some("the shared-cache lease could not be renewed".into());
            break;
        }
        pass.last_tick = Instant::now();
        let path = request.worktree.join(root);
        if pass.already_shared(&path) {
            pass.report.roots_already_shared.push(root.clone());
            continue;
        }
        pass.walk(&path, tick);
    }
    let Pass {
        mut report,
        unique_before,
        unique_after,
        estimated,
        stop,
        ..
    } = pass;
    report.state = Some(if stop.is_some() {
        OutputShareState::Partial
    } else {
        OutputShareState::Complete
    });
    report.fallback_reason = stop;
    report.unique_bytes_before = Some(unique_before);
    report.unique_bytes_after = Some(unique_after);
    report.unique_bytes_method = Some(if estimated {
        UniqueBytesMethod::AllocatedUpperBound
    } else {
        UniqueBytesMethod::BlockSharing
    });
    report.elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    report
}

struct Pass {
    pool: PathBuf,
    deadline: Instant,
    last_tick: Instant,
    report: OutputShareReport,
    unique_before: u64,
    unique_after: u64,
    /// A file's unshared size was unavailable and its allocation was counted.
    estimated: bool,
    /// Why the pass stopped early.
    stop: Option<String>,
    temp_counter: u64,
}

impl Pass {
    /// Whether at least seven in eight of the first large files of `root`,
    /// depth first, already share every block. One in eight may be the
    /// tool's own bookkeeping (pnpm writes `.pnpm/lock.yaml` itself). A root
    /// with too few large files is walked.
    fn already_shared(&self, root: &Path) -> bool {
        let mut stack = vec![root.to_path_buf()];
        let mut sampled = 0_usize;
        let mut shared = 0_usize;
        while let Some(directory) = stack.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    stack.push(entry.path());
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let path = entry.path();
                let Ok(metadata) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.len() < MIN_SHARED_FILE_BYTES || link_count(&metadata) != 1 {
                    continue;
                }
                sampled += 1;
                if unshared_bytes(&path) == Some(0) {
                    shared += 1;
                }
                if sampled - shared > ALREADY_SHARED_SAMPLE / 8 {
                    return false;
                }
                if sampled >= ALREADY_SHARED_SAMPLE {
                    return true;
                }
            }
            if Instant::now() >= self.deadline {
                return false;
            }
        }
        false
    }

    fn walk(&mut self, root: &Path, tick: &mut dyn FnMut() -> bool) {
        let device = device_of(root);
        let mut stack = vec![root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                if self.should_stop(tick) {
                    return;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if file_type.is_dir() {
                    // Stay on the root's volume: a mount point inside an
                    // output is not this checkout's bytes.
                    if device.is_none() || device_of(&path) == device {
                        stack.push(path);
                    }
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let Ok(metadata) = fs::symlink_metadata(&path) else {
                    continue;
                };
                self.visit(&path, &metadata);
            }
        }
    }

    fn should_stop(&mut self, tick: &mut dyn FnMut() -> bool) -> bool {
        if self.stop.is_some() {
            return true;
        }
        let now = Instant::now();
        if now >= self.deadline {
            self.stop = Some("the output-sharing time budget ran out".into());
            return true;
        }
        if now.duration_since(self.last_tick) >= TICK_INTERVAL {
            self.last_tick = now;
            if !tick() {
                self.stop = Some("the shared-cache lease could not be renewed".into());
                return true;
            }
        }
        false
    }

    fn visit(&mut self, path: &Path, metadata: &fs::Metadata) {
        self.report.files_scanned += 1;
        self.report.bytes_scanned += metadata.len();
        let unshared = match unshared_bytes(path) {
            Some(bytes) => bytes,
            None => {
                self.estimated = true;
                allocated_bytes(metadata)
            }
        };
        self.unique_before = self.unique_before.saturating_add(unshared);
        if metadata.len() < MIN_SHARED_FILE_BYTES {
            self.unique_after = self.unique_after.saturating_add(unshared);
            return;
        }
        if unshared == 0 {
            self.report.files_already_shared += 1;
            return;
        }
        if link_count(metadata) != 1 {
            self.report.files_left_alone += 1;
            self.unique_after = self.unique_after.saturating_add(unshared);
            return;
        }
        match self.share_file(path, metadata) {
            Ok(Shared::FromPool) => {
                self.report.files_cloned_from_pool += 1;
                self.report.bytes_cloned_from_pool += metadata.len();
            }
            Ok(Shared::IntoPool) => {
                self.report.files_added_to_pool += 1;
                self.report.bytes_added_to_pool += metadata.len();
            }
            Ok(Shared::LeftAlone) => {
                self.report.files_left_alone += 1;
                self.unique_after = self.unique_after.saturating_add(unshared);
            }
            Err(error) => {
                self.report.files_failed += 1;
                self.unique_after = self.unique_after.saturating_add(unshared);
                if clone_unsupported(&error) {
                    self.stop = Some(format!("a clone failed, so sharing stopped: {error}"));
                }
            }
        }
    }

    fn share_file(&mut self, path: &Path, metadata: &fs::Metadata) -> io::Result<Shared> {
        let digest = sha256_file(path)?;
        // A file rewritten while it was hashed keeps its own copy.
        let after = fs::symlink_metadata(path)?;
        if !same_file_state(metadata, &after) {
            return Ok(Shared::LeftAlone);
        }
        let bucket = self.pool.join(&digest[..2]);
        package_store::ensure_real_directory(&bucket)?;
        let entry = bucket.join(&digest);
        match fs::symlink_metadata(&entry) {
            Ok(pooled)
                if pooled.is_file()
                    && !pooled.file_type().is_symlink()
                    && pooled.len() == metadata.len() =>
            {
                let parent = path
                    .parent()
                    .ok_or_else(|| io::Error::other("an output file has no parent directory"))?;
                let temporary = self.temporary_name(parent);
                let replaced = replace_with_pooled(&entry, &temporary, path, metadata);
                if replaced.is_err() {
                    remove_temporary(&temporary);
                }
                replaced.map(|()| Shared::FromPool)
            }
            Ok(_) => Err(io::Error::other(format!(
                "{} is not a pooled file of the expected size",
                entry.display()
            ))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let temporary = self.temporary_name(&bucket);
                let added = add_to_pool(path, &temporary, &entry);
                if added.is_err() {
                    remove_temporary(&temporary);
                }
                added.map(|()| Shared::IntoPool)
            }
            Err(error) => Err(error),
        }
    }

    fn temporary_name(&mut self, directory: &Path) -> PathBuf {
        self.temp_counter += 1;
        directory.join(format!(
            ".aethyme-share-{}-{}.tmp",
            std::process::id(),
            self.temp_counter
        ))
    }
}

enum Shared {
    FromPool,
    IntoPool,
    LeftAlone,
}

/// Replace `path` with a clone of the pool `entry` (same bytes) carrying
/// `path`'s own mode and timestamps, through `temporary` beside it.
fn replace_with_pooled(
    entry: &Path,
    temporary: &Path,
    path: &Path,
    original: &fs::Metadata,
) -> io::Result<()> {
    package_store::clone_file(entry, temporary)?;
    restore_metadata(temporary, original)?;
    if fs::symlink_metadata(temporary)?.len() != original.len() {
        return Err(io::Error::other("the pooled clone has the wrong size"));
    }
    fs::rename(temporary, path)
}

/// Clone `path` into the pool as `entry`, through `temporary` beside it.
/// Pool entries are read-only and never written after this.
fn add_to_pool(path: &Path, temporary: &Path, entry: &Path) -> io::Result<()> {
    package_store::clone_file(path, temporary)?;
    set_mode(temporary, 0o444)?;
    fs::rename(temporary, entry)
}

fn remove_temporary(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        eprintln!(
            "warning: aethyme could not remove {}: {error}",
            path.display()
        );
    }
}

/// Whether a clone failure means the volume cannot clone at all, so further
/// attempts are pointless.
fn clone_unsupported(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(
            error.raw_os_error(),
            Some(code) if code == libc::ENOTSUP || code == libc::EXDEV || code == libc::EOPNOTSUPP
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Give the clone the original file's mode and timestamps. Owner-writable
/// first, so the timestamps can be set on a clone of a read-only entry.
fn restore_metadata(path: &Path, original: &fs::Metadata) -> io::Result<()> {
    set_mode(path, 0o600)?;
    let mut times = fs::FileTimes::new().set_modified(original.modified()?);
    if let Ok(accessed) = original.accessed() {
        times = times.set_accessed(accessed);
    }
    fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_times(times)?;
    fs::set_permissions(path, original.permissions())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn same_file_state(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.ino() == after.ino()
        && before.dev() == after.dev()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
}

#[cfg(not(unix))]
fn same_file_state(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.len() == after.len() && before.modified().ok() == after.modified().ok()
}

#[cfg(unix)]
fn link_count(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink()
}

#[cfg(not(unix))]
fn link_count(_metadata: &fs::Metadata) -> u64 {
    1
}

#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

#[cfg(unix)]
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| metadata.dev())
}

#[cfg(not(unix))]
fn device_of(_path: &Path) -> Option<u64> {
    None
}

fn sha256_file(path: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// The bytes of `path` no other file shares, as the filesystem reports them:
/// APFS's private size on macOS, the extents without the shared flag from
/// FIEMAP on Linux. `None` when the filesystem cannot say.
#[cfg(target_os = "macos")]
pub(crate) fn unshared_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    /// `struct attrlist` from `<sys/attr.h>`.
    // The kernel reads and writes these fields; Rust reads only some.
    #[allow(dead_code)]
    #[repr(C)]
    struct AttributeList {
        bitmap_count: u16,
        reserved: u16,
        common: u32,
        volume: u32,
        directory: u32,
        file: u32,
        fork: u32,
    }
    const ATTR_BIT_MAP_COUNT: u16 = 5;
    /// `ATTR_CMNEXT_PRIVATESIZE`: an `off_t`, the bytes of the file's
    /// blocks that no clone shares. Requested in the fork group with
    /// `FSOPT_ATTR_CMN_EXTENDED`.
    const ATTR_CMNEXT_PRIVATESIZE: u32 = 0x0000_0008;
    const FSOPT_NOFOLLOW: u32 = 0x0000_0001;
    const FSOPT_ATTR_CMN_EXTENDED: u32 = 0x0000_0020;

    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut request = AttributeList {
        bitmap_count: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        common: 0,
        volume: 0,
        directory: 0,
        file: 0,
        fork: ATTR_CMNEXT_PRIVATESIZE,
    };
    // A u32 length, then the 8-byte private size at offset 4 (attributes
    // are packed on 4-byte boundaries).
    let mut reply = [0_u8; 16];
    // SAFETY: `path` is NUL-terminated, `request` is a valid attribute list
    // and `reply` is writable for its full length; all outlive the call.
    let result = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&mut request as *mut AttributeList).cast::<libc::c_void>(),
            reply.as_mut_ptr().cast::<libc::c_void>(),
            reply.len(),
            FSOPT_NOFOLLOW | FSOPT_ATTR_CMN_EXTENDED,
        )
    };
    if result != 0 {
        return None;
    }
    let length = u32::from_ne_bytes([reply[0], reply[1], reply[2], reply[3]]);
    if length < 12 {
        return None;
    }
    let mut size = [0_u8; 8];
    size.copy_from_slice(&reply[4..12]);
    u64::try_from(i64::from_ne_bytes(size)).ok()
}

#[cfg(target_os = "linux")]
pub(crate) fn unshared_bytes(path: &Path) -> Option<u64> {
    use std::os::fd::AsRawFd;
    /// `FS_IOC_FIEMAP`, `_IOWR('f', 11, struct fiemap)`, spelled out so glibc
    /// and musl (whose `ioctl` request types differ) build the same call.
    const FS_IOC_FIEMAP: u32 = 0xC020_660B;
    const FIEMAP_EXTENT_LAST: u32 = 0x0000_0001;
    const FIEMAP_EXTENT_SHARED: u32 = 0x0000_2000;
    const BATCH: usize = 32;
    /// `struct fiemap_extent` from `<linux/fiemap.h>`.
    // The kernel reads and writes these fields; Rust reads only some.
    #[allow(dead_code)]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Extent {
        logical: u64,
        physical: u64,
        length: u64,
        reserved64: [u64; 2],
        flags: u32,
        reserved: [u32; 3],
    }
    /// `struct fiemap` followed by room for `BATCH` extents.
    // The kernel reads and writes these fields; Rust reads only some.
    #[allow(dead_code)]
    #[repr(C)]
    struct Request {
        start: u64,
        length: u64,
        flags: u32,
        mapped_extents: u32,
        extent_count: u32,
        reserved: u32,
        extents: [Extent; BATCH],
    }
    const EMPTY: Extent = Extent {
        logical: 0,
        physical: 0,
        length: 0,
        reserved64: [0; 2],
        flags: 0,
        reserved: [0; 3],
    };

    let file = fs::File::open(path).ok()?;
    let mut unshared = 0_u64;
    let mut start = 0_u64;
    loop {
        let mut request = Request {
            start,
            length: u64::MAX - start,
            flags: 0,
            mapped_extents: 0,
            extent_count: BATCH as u32,
            reserved: 0,
            extents: [EMPTY; BATCH],
        };
        // SAFETY: the descriptor is open for the call, and `request` is a
        // `struct fiemap` with room for `extent_count` extents.
        let result = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                FS_IOC_FIEMAP as _,
                &mut request as *mut Request,
            )
        };
        if result != 0 {
            return None;
        }
        let mapped = (request.mapped_extents as usize).min(BATCH);
        if mapped == 0 {
            return Some(unshared);
        }
        for extent in &request.extents[..mapped] {
            if extent.flags & FIEMAP_EXTENT_SHARED == 0 {
                unshared = unshared.saturating_add(extent.length);
            }
            if extent.flags & FIEMAP_EXTENT_LAST != 0 {
                return Some(unshared);
            }
            start = extent.logical.saturating_add(extent.length);
        }
        if start == u64::MAX {
            return Some(unshared);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn unshared_bytes(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn payload(seed: u8) -> Vec<u8> {
        (0..(MIN_SHARED_FILE_BYTES as usize + 4096))
            .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn roots_are_the_outermost_ignored_directories_of_declared_outputs() {
        let worktree = tempfile::tempdir().unwrap();
        for directory in [
            "node_modules/.pnpm",
            "backend/.venv/bin",
            "backend/src",
            "linked-target",
        ] {
            fs::create_dir_all(worktree.path().join(directory)).unwrap();
        }
        write(&worktree.path().join("backend/.venv/bin/pytest"), b"#!");
        let ignored = ["node_modules", "backend/.venv", "node_modules/.pnpm"];
        let roots = output_roots(
            worktree.path(),
            [
                "node_modules/",
                "node_modules/.pnpm/",
                "backend/.venv/bin/pytest",
                "backend/src/",
                "missing/dir/",
                "../escape/",
            ],
            |candidate| ignored.contains(&candidate),
        );
        assert_eq!(roots, vec!["backend/.venv", "node_modules"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_output_directory_is_never_a_root() {
        let worktree = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), worktree.path().join("vendor")).unwrap();
        assert!(output_roots(worktree.path(), ["vendor/"], |_| true).is_empty());
    }

    #[test]
    fn only_a_cloning_volume_shares_outputs() {
        let roots = vec!["vendor".to_string()];
        assert_eq!(skip_reason(PackageImportMethod::Clone, &roots), None);
        assert!(
            skip_reason(PackageImportMethod::Hardlink, &roots)
                .unwrap()
                .contains("hard link")
        );
        assert!(skip_reason(PackageImportMethod::Copy, &roots).is_some());
        assert!(skip_reason(PackageImportMethod::Clone, &[]).is_some());

        let worktree = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        write(&worktree.path().join("vendor/lib.so"), &payload(1));
        let report = share_outputs(
            &OutputShareRequest {
                worktree: worktree.path(),
                roots,
                store: store.path(),
                import_method: PackageImportMethod::Copy,
                budget: OUTPUT_SHARE_BUDGET,
            },
            &mut || true,
        );
        assert_eq!(report.state, Some(OutputShareState::Skipped));
        assert!(!store.path().join(OUTPUT_POOL_DIR).exists());
    }

    /// Two checkouts with the same large file: the first seeds the pool, the
    /// second's copy is replaced by a clone with its own mode and mtime.
    /// Runs only where the test directory can clone (APFS, btrfs, XFS).
    #[cfg(unix)]
    #[test]
    fn identical_large_files_are_shared_through_the_pool() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store");
        fs::create_dir(&store).unwrap();
        if package_store::probe_import_method(&store, Some(true)) != PackageImportMethod::Clone {
            return;
        }
        let first = root.path().join("first");
        let second = root.path().join("second");
        let shared = payload(7);
        for checkout in [&first, &second] {
            write(&checkout.join("env/lib/big.so"), &shared);
            write(&checkout.join("env/lib/small.py"), b"print(1)\n");
        }
        write(&second.join("env/lib/own.so"), &payload(9));
        let file = second.join("env/lib/big.so");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let old = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000);
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
        let inode_before = fs::metadata(&file).unwrap().ino();

        let run = |checkout: &Path| {
            share_outputs(
                &OutputShareRequest {
                    worktree: checkout,
                    roots: vec!["env".into()],
                    store: &store,
                    import_method: PackageImportMethod::Clone,
                    budget: OUTPUT_SHARE_BUDGET,
                },
                &mut || true,
            )
        };
        let seeded = run(&first);
        assert_eq!(seeded.state, Some(OutputShareState::Complete));
        assert_eq!(seeded.files_added_to_pool, 1);
        assert_eq!(seeded.files_cloned_from_pool, 0);

        let shared_report = run(&second);
        assert_eq!(shared_report.files_cloned_from_pool, 1);
        assert_eq!(
            shared_report.files_added_to_pool, 1,
            "own.so seeds the pool"
        );
        assert_eq!(fs::read(&file).unwrap(), shared);
        let after = fs::metadata(&file).unwrap();
        assert_ne!(
            after.ino(),
            inode_before,
            "the file was replaced by a clone"
        );
        assert_eq!(after.permissions().mode() & 0o777, 0o755);
        assert_eq!(after.modified().unwrap(), old);
        assert_eq!(
            fs::read(second.join("env/lib/small.py")).unwrap(),
            b"print(1)\n"
        );
        // Copy on write: changing one checkout never reaches the pool or
        // the other checkout.
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&file, b"changed").unwrap();
        assert_eq!(fs::read(first.join("env/lib/big.so")).unwrap(), shared);
        let pooled = fs::read_dir(store.join(OUTPUT_POOL_DIR))
            .unwrap()
            .flatten()
            .flat_map(|bucket| fs::read_dir(bucket.path()).unwrap().flatten())
            .map(|entry| fs::read(entry.path()).unwrap())
            .collect::<Vec<_>>();
        assert!(pooled.contains(&shared));
        // No temporary is left behind anywhere.
        for directory in [second.join("env/lib"), store.join(OUTPUT_POOL_DIR)] {
            for entry in walkdir(&directory) {
                assert!(!entry.to_string_lossy().contains(".aethyme-share-"));
            }
        }
        // A third run finds everything shared or already pooled.
        let again = run(&first);
        assert_eq!(again.files_added_to_pool, 0);
    }

    #[cfg(unix)]
    fn walkdir(directory: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![directory.to_path_buf()];
        while let Some(next) = stack.pop() {
            for entry in fs::read_dir(&next).unwrap().flatten() {
                if entry.file_type().unwrap().is_dir() {
                    stack.push(entry.path());
                }
                found.push(entry.path());
            }
        }
        found
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_files_and_a_lost_lease_are_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store");
        fs::create_dir(&store).unwrap();
        if package_store::probe_import_method(&store, Some(true)) != PackageImportMethod::Clone {
            return;
        }
        let checkout = root.path().join("checkout");
        write(&checkout.join("env/a.so"), &payload(3));
        fs::hard_link(checkout.join("env/a.so"), checkout.join("env/b.so")).unwrap();
        let request = OutputShareRequest {
            worktree: &checkout,
            roots: vec!["env".into()],
            store: &store,
            import_method: PackageImportMethod::Clone,
            budget: OUTPUT_SHARE_BUDGET,
        };
        let report = share_outputs(&request, &mut || true);
        assert_eq!(report.files_left_alone, 2);
        assert_eq!(report.files_added_to_pool, 0);

        let stopped = share_outputs(&request, &mut || false);
        assert_eq!(stopped.state, Some(OutputShareState::Partial));
        assert_eq!(stopped.files_scanned, 0);
    }

    #[test]
    fn unshared_bytes_of_a_fresh_file_is_its_data_or_unknown() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fresh");
        let file = fs::File::create(&path).unwrap();
        std::io::Write::write_all(&mut &file, &payload(5)).unwrap();
        file.sync_all().unwrap();
        // tmpfs answers nothing; ext4, APFS, btrfs and XFS report the
        // unshared data of a file nobody cloned.
        if let Some(bytes) = unshared_bytes(&path) {
            assert!(bytes >= MIN_SHARED_FILE_BYTES, "{bytes}");
        }
    }

    #[test]
    fn the_metric_carries_counters_not_paths() {
        let mut report = OutputShareReport::skipped("reason");
        report.roots = vec!["secret/path".into()];
        let metric = report.metric(PackageImportMethod::Copy);
        assert_eq!(metric["roots"], 1);
        assert_eq!(metric["import_method"], "copy");
        assert_eq!(metric["fallback_reason"], "reason");
        assert!(!metric.to_string().contains("secret/path"));
    }
}
