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

/// Whether a runtime's probe output keys the prepared state.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeIdentityState {
    /// The probe ran to a zero exit; its output digest is in the key.
    Verified,
    /// The probe could not start, failed or timed out. The key falls back to
    /// the executable identity plus a per-run nonce, so no other run shares
    /// it and the next preparation probes again.
    Unverified,
}

/// One runtime the preparation declares: the executable its probe resolves
/// to (canonical path, size, modification time) and, once a preparation has
/// run the probe, a digest of what it printed.
///
/// The prepared state is keyed on the probe *output*: a runtime reached
/// through another interpreter (a Python probe launched through Node) is
/// judged by its output like any other. The executable identity only lets a
/// status read or a storage sweep, which never run a probe, find the output a
/// preparation recorded for that exact executable.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RuntimeIdentity {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_ns: Option<u64>,
    /// Absent until a preparation has run the probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_identity: Option<RuntimeIdentityState>,
    /// SHA-256 of the probe's standard output, when verified. Raw output is
    /// never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_sha256: Option<String>,
    /// A version-shaped extract of the first output line: at most 64
    /// characters from `[A-Za-z0-9 ._+:/()-]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_summary: Option<String>,
    /// Why the probe is unverified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverified_reason: Option<String>,
}

impl RuntimeIdentity {
    /// Whether `other` resolves the same executable (path, size, mtime).
    pub(crate) fn same_executable(&self, other: &Self) -> bool {
        self.executable == other.executable
            && self.size == other.size
            && self.modified_ns == other.modified_ns
    }

    pub(crate) fn verified(&self) -> bool {
        self.runtime_identity == Some(RuntimeIdentityState::Verified) && self.probe_sha256.is_some()
    }
}

/// How long one runtime probe may run before its process group is killed.
pub(crate) const RUNTIME_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// The most standard output read from one probe; more is a failure.
const RUNTIME_PROBE_MAX_OUTPUT: usize = 64 * 1024;
/// After the probe exits, how long its output pipe may stay open (held by a
/// leftover grandchild) before the probe counts as failed.
const RUNTIME_PROBE_PIPE_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

/// What a successful probe produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeProbeOutput {
    pub(crate) sha256: String,
    pub(crate) summary: Option<String>,
}

/// A version-shaped extract of `output`'s first non-empty line, so a probe
/// that prints a secret never puts it in a record, a report or a metric.
pub(crate) fn probe_summary(output: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(output);
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let summary = line
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || " ._+:/()-".contains(*character))
        .take(64)
        .collect::<String>();
    (!summary.trim().is_empty()).then(|| summary.trim().to_owned())
}

/// Run one declared runtime probe: execute exactly `executable` (the
/// canonical file the preparation digest resolved, never re-resolved through
/// `PATH`) with `command[0]` as its `argv[0]` and `command[1..]` as its
/// arguments, in `root`, with the broker's environment, which is the
/// environment preparation steps inherit. Standard input is closed and
/// standard error discarded. The probe runs in its own process group, which
/// is killed at `timeout`. Any non-zero exit, timeout, oversized output, or
/// an output pipe still held open after exit is a failure (`Err(reason)`).
pub(crate) fn run_runtime_probe(
    root: &Path,
    executable: &Path,
    command: &[String],
    timeout: std::time::Duration,
) -> Result<RuntimeProbeOutput, String> {
    use sha2::{Digest, Sha256};
    use std::process::{Command, Stdio};

    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| "the probe command is empty".to_owned())?;
    if !executable.is_absolute() || !is_executable_file(executable) {
        return Err(format!(
            "{} is not an executable regular file",
            executable.display()
        ));
    }
    let mut builder = Command::new(executable);
    builder
        .args(arguments)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        builder.arg0(program).process_group(0);
    }
    #[cfg(not(unix))]
    let _program = program;
    let mut child = builder
        .spawn()
        .map_err(|error| format!("the probe could not start: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "the probe's output pipe is unavailable".to_owned())?;
    let outcome = collect_probe_output(&mut child, stdout, timeout);
    match outcome {
        Ok((Some(0), output)) => {
            let mut hasher = Sha256::new();
            hasher.update(&output);
            Ok(RuntimeProbeOutput {
                sha256: format!("{:x}", hasher.finalize()),
                summary: probe_summary(&output),
            })
        }
        Ok((status, _)) => Err(match status {
            Some(code) => format!("the probe exited with status {code}"),
            None => "the probe was terminated by a signal".to_owned(),
        }),
        Err(reason) => Err(reason),
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// Read the probe's output without a helper thread: the pipe is
/// non-blocking and polled until the probe exits and the pipe closes, both
/// within `timeout`; anything else kills the process group.
#[cfg(unix)]
fn collect_probe_output(
    child: &mut std::process::Child,
    stdout: std::process::ChildStdout,
    timeout: std::time::Duration,
) -> Result<(Option<i32>, Vec<u8>), String> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let fd = stdout.as_raw_fd();
    // SAFETY: `fd` is the open read end of the probe's stdout pipe, owned by
    // `stdout` for the whole function.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    let set = if flags < 0 {
        -1
    } else {
        // SAFETY: as above; F_SETFL only changes this descriptor's flags.
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) }
    };
    if set < 0 {
        kill_probe(child);
        return Err("the probe's output pipe could not be made non-blocking".into());
    }
    let mut stdout = stdout;
    let deadline = std::time::Instant::now() + timeout;
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut status = None;
    let mut exited_at = None;
    loop {
        let mut closed = false;
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(read) => {
                    output.extend_from_slice(&buffer[..read]);
                    if output.len() > RUNTIME_PROBE_MAX_OUTPUT {
                        kill_probe(child);
                        return Err("the probe printed more than 64 KiB".into());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    kill_probe(child);
                    return Err(format!("the probe's output could not be read: {error}"));
                }
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    status = Some(exit.code());
                    exited_at = Some(std::time::Instant::now());
                }
                Ok(None) => {}
                Err(error) => {
                    kill_probe(child);
                    return Err(format!("the probe could not be waited on: {error}"));
                }
            }
        }
        let now = std::time::Instant::now();
        match (status, closed) {
            (Some(code), true) => return Ok((code, output)),
            (Some(_), false)
                if exited_at.is_some_and(|at| now >= at + RUNTIME_PROBE_PIPE_GRACE) =>
            {
                kill_probe(child);
                return Err("a process left by the probe kept its output open".into());
            }
            _ if now >= deadline => {
                kill_probe(child);
                return Err(format!(
                    "the probe did not finish within {} ms",
                    timeout.as_millis()
                ));
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
}

#[cfg(not(unix))]
fn collect_probe_output(
    child: &mut std::process::Child,
    mut stdout: std::process::ChildStdout,
    _timeout: std::time::Duration,
) -> Result<(Option<i32>, Vec<u8>), String> {
    use std::io::Read;
    let mut output = Vec::new();
    stdout
        .read_to_end(&mut output)
        .map_err(|error| format!("the probe's output could not be read: {error}"))?;
    let status = child
        .wait()
        .map_err(|error| format!("the probe could not be waited on: {error}"))?;
    Ok((status.code(), output))
}

/// Kill the probe's process group (Unix) and the probe, then reap it.
fn kill_probe(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        if let Ok(pid) = libc::pid_t::try_from(child.id()) {
            // SAFETY: kill(2) with a negative pid signals the probe's own
            // process group, created for it by `process_group(0)`.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    if let Err(error) = child.kill()
        && error.kind() != io::ErrorKind::InvalidInput
    {
        eprintln!("warning: aethyme could not stop a runtime probe: {error}");
    }
    if let Err(error) = child.wait() {
        eprintln!("warning: aethyme could not reap a runtime probe: {error}");
    }
}

/// Where a checkout keeps its runtime-probe cache: inside its own Git
/// directory (`.git/` or the linked worktree's `gitdir`), so it is never part
/// of the working tree and goes away with the checkout.
pub(crate) fn probe_cache_path(root: &Path) -> Option<PathBuf> {
    const FILE: &str = "aethyme-runtime-probes.json";
    let dot_git = root.join(".git");
    let metadata = fs::symlink_metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        return Some(dot_git.join(FILE));
    }
    if !metadata.is_file() || metadata.len() > 4096 {
        return None;
    }
    let text = fs::read_to_string(&dot_git).ok()?;
    let gitdir = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir: "))?;
    let gitdir = Path::new(gitdir.trim());
    let gitdir = if gitdir.is_absolute() {
        gitdir.to_path_buf()
    } else {
        root.join(gitdir)
    };
    fs::symlink_metadata(&gitdir)
        .ok()
        .filter(fs::Metadata::is_dir)
        .map(|_| gitdir.join(FILE))
}

/// One cached probe result: the output digest and extract a preparation got
/// from exactly this executable and command.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CachedProbe {
    pub(crate) sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<String>,
    pub(crate) recorded_at_ms: i64,
}

/// The cache key for a probe: runtime name, full command, and the resolved
/// executable's canonical path, size and modification time.
pub(crate) fn probe_cache_key(
    name: &str,
    command: &[String],
    identity: &RuntimeIdentity,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    for argument in command {
        hasher.update([0]);
        hasher.update(argument.as_bytes());
    }
    hasher.update([1]);
    if let Some(executable) = &identity.executable {
        hasher.update(executable.to_string_lossy().as_bytes());
    }
    hasher.update(identity.size.unwrap_or(0).to_le_bytes());
    hasher.update(identity.modified_ns.unwrap_or(0).to_le_bytes());
    format!("{:x}", hasher.finalize())
}

/// Parse a probe cache file's bytes; anything unreadable is an empty cache.
pub(crate) fn parse_probe_cache(bytes: &[u8]) -> std::collections::BTreeMap<String, CachedProbe> {
    serde_json::from_slice(bytes).unwrap_or_default()
}

/// Replace the checkout's probe cache atomically.
pub(crate) fn write_probe_cache(
    path: &Path,
    entries: &std::collections::BTreeMap<String, CachedProbe>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("the probe cache path has no parent"))?;
    let bytes = serde_json::to_vec(entries).map_err(io::Error::other)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
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

/// Prefix of a shared store renamed away by a rotation. Nothing references a
/// retired store, so it is removed without the repository lease.
pub(crate) const RETIRED_PACKAGE_STORE_PREFIX: &str = "packages-retired-";

/// Byte cap on a repository's shared package store when its own
/// `[shared_cache] max_bytes` is not known (storage sweeps).
pub(crate) const DEFAULT_PACKAGE_STORE_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;

pub(crate) fn is_retired_store_name(name: &str) -> bool {
    name.strip_prefix(RETIRED_PACKAGE_STORE_PREFIX)
        .is_some_and(|rest| {
            !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Rename `<repository>/packages` to a retired name. The caller holds the
/// repository's exclusive preparation-cache lease, so no preparation is
/// using it. Returns the retired path, or `None` when there is no store.
pub(crate) fn retire_shared_store(repository_dir: &Path) -> io::Result<Option<PathBuf>> {
    let store = repository_dir.join(SHARED_PACKAGE_STORE_DIR);
    match fs::symlink_metadata(&store) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(io::Error::other(
                "the shared package store is not a real directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let retired = repository_dir.join(format!(
        "{RETIRED_PACKAGE_STORE_PREFIX}{nanos}-{}",
        std::process::id()
    ));
    fs::rename(&store, &retired)?;
    Ok(Some(retired))
}

/// Whether the store at `path` holds more than `cap` logical bytes: `Some`
/// when a bounded no-follow walk decided, `None` when it stopped at
/// `deadline` or `max_entries` first.
pub(crate) fn store_exceeds(
    path: &Path,
    cap: u64,
    deadline: std::time::Instant,
    max_entries: usize,
) -> Option<bool> {
    let mut stack = vec![path.to_path_buf()];
    let mut bytes = 0_u64;
    let mut entries = 0_usize;
    while let Some(directory) = stack.pop() {
        let Ok(children) = fs::read_dir(&directory) else {
            continue;
        };
        for child in children.flatten() {
            entries += 1;
            if entries > max_entries || std::time::Instant::now() >= deadline {
                return None;
            }
            let Ok(metadata) = fs::symlink_metadata(child.path()) else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(child.path());
            } else {
                bytes = bytes.saturating_add(metadata.len());
                if bytes > cap {
                    return Some(true);
                }
            }
        }
    }
    Some(false)
}

/// Whether the volume holding `path` is below the broker's free-space or
/// free-inode reserve.
pub(crate) fn volume_under_pressure(path: &Path) -> bool {
    crate::disk_headroom::available_headroom(path).is_some_and(|headroom| {
        crate::disk_headroom::refusal_with_headroom(
            Some(headroom.bytes),
            headroom.inodes,
            crate::DEFAULT_GATE_HEADROOM_BYTES,
            crate::disk_headroom::MIN_GATE_HEADROOM_INODES,
        )
        .is_some()
    })
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

    #[cfg(unix)]
    #[test]
    fn a_runtime_probe_is_judged_by_its_output_and_bounded() {
        let root = tempfile::tempdir().unwrap();
        let shell = fs::canonicalize("/bin/sh").unwrap();
        let probe = |script: &str, timeout_ms: u64| {
            run_runtime_probe(
                root.path(),
                &shell,
                &["sh".into(), "-c".into(), script.into()],
                std::time::Duration::from_millis(timeout_ms),
            )
        };
        let first = probe("echo v1.2.3", 5_000).unwrap();
        assert_eq!(first.summary.as_deref(), Some("v1.2.3"));
        assert_eq!(probe("echo v1.2.3", 5_000).unwrap().sha256, first.sha256);
        assert_ne!(probe("echo v1.2.4", 5_000).unwrap().sha256, first.sha256);
        // Failures never produce an output digest.
        assert!(
            probe("echo v1.2.3; exit 3", 5_000)
                .unwrap_err()
                .contains("status 3")
        );
        assert!(
            probe("sleep 30", 200)
                .unwrap_err()
                .contains("did not finish")
        );
        assert!(
            probe("(sleep 30 &) ; echo started", 5_000)
                .unwrap_err()
                .contains("kept its output open")
        );
        assert!(
            run_runtime_probe(
                root.path(),
                &root.path().join("missing"),
                &["missing".into()],
                RUNTIME_PROBE_TIMEOUT
            )
            .is_err()
        );
    }

    #[test]
    fn a_probe_summary_keeps_only_a_version_shaped_extract() {
        assert_eq!(
            probe_summary(b"\n  v24.15.0\nsecond").as_deref(),
            Some("v24.15.0")
        );
        assert_eq!(
            probe_summary(b"token=abc$%^&*\"'secret").as_deref(),
            Some("tokenabcsecret")
        );
        assert_eq!(probe_summary(&[b'9'; 200]).map(|s| s.len()), Some(64));
        assert_eq!(probe_summary(b"$$$"), None);
    }

    #[test]
    fn the_probe_cache_lives_in_the_checkout_git_directory() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(probe_cache_path(root.path()), None);
        fs::create_dir(root.path().join(".git")).unwrap();
        assert_eq!(
            probe_cache_path(root.path()),
            Some(root.path().join(".git/aethyme-runtime-probes.json"))
        );
        let linked = tempfile::tempdir().unwrap();
        let gitdir = root.path().join(".git/worktrees/linked");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(
            linked.path().join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        let path = probe_cache_path(linked.path()).unwrap();
        assert_eq!(path, gitdir.join("aethyme-runtime-probes.json"));
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            "key".to_string(),
            CachedProbe {
                sha256: "abc".into(),
                summary: Some("v1".into()),
                recorded_at_ms: 1,
            },
        );
        write_probe_cache(&path, &entries).unwrap();
        assert_eq!(parse_probe_cache(&fs::read(&path).unwrap()), entries);
        assert!(parse_probe_cache(b"not json").is_empty());
    }

    #[test]
    fn the_cache_key_changes_with_the_executable_and_the_command() {
        let identity = RuntimeIdentity {
            name: "node".into(),
            executable: Some(PathBuf::from("/usr/bin/node")),
            size: Some(1),
            modified_ns: Some(2),
            runtime_identity: None,
            probe_sha256: None,
            probe_summary: None,
            unverified_reason: None,
        };
        let command = vec!["node".to_string(), "--version".to_string()];
        let key = probe_cache_key("node", &command, &identity);
        let mut reinstalled = identity.clone();
        reinstalled.modified_ns = Some(3);
        assert_ne!(probe_cache_key("node", &command, &reinstalled), key);
        assert_ne!(
            probe_cache_key("node", &["node".to_string(), "-v".to_string()], &identity),
            key
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_rotation_retires_the_shared_store_and_bounds_its_measurement() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let store = repository
            .join(SHARED_PACKAGE_STORE_DIR)
            .join("linux-x86_64/pnpm-store");
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("file"), vec![0_u8; 1000]).unwrap();
        let shared = repository.join(SHARED_PACKAGE_STORE_DIR);
        let far = std::time::Instant::now() + std::time::Duration::from_secs(30);
        assert_eq!(store_exceeds(&shared, 999, far, 1_000), Some(true));
        assert_eq!(store_exceeds(&shared, 1_000, far, 1_000), Some(false));
        assert_eq!(store_exceeds(&shared, 1_000, far, 1), None);
        let retired = retire_shared_store(&repository).unwrap().unwrap();
        assert!(is_retired_store_name(
            retired.file_name().unwrap().to_str().unwrap()
        ));
        assert!(!shared.exists());
        assert!(retired.join("linux-x86_64/pnpm-store/file").is_file());
        assert_eq!(retire_shared_store(&repository).unwrap(), None);
        assert!(!is_retired_store_name("packages-retired-"));
        assert!(!is_retired_store_name("packages"));
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
