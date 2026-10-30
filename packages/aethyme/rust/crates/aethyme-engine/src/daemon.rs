//! Engine-side daemon: serves read-only redb graph queries over a socket.
//!
//! Why this exists
//! ---------------
//! The daemon used to keep a resident `RepositoryMap` warm to avoid rebuilding
//! it for every `task-localize`, `symbol-batch`, and caller query. Those
//! surfaces now read `.aethyme/graph_store.redb` directly. The daemon remains
//! as stable socket/process plumbing for clients that want a long-lived engine
//! process, but it no longer constructs or owns the legacy in-memory map.
//!
//! Startup opens the redb store read-only and performs a tiny overview read so
//! missing or incompatible stores fail before the socket is advertised. Each
//! request opens a fresh read-only handle, preserving the same read/write
//! boundary as the CLI query commands.
//!
//! Wire protocol (JSON line-delimited over AF_UNIX)
//! ------------------------------------------------
//! Request:
//!   `{"command": "task-localize", "task": "<text>"}\n`
//!   `{"command": "ping"}\n`
//!   `{"command": "shutdown"}\n`
//!
//! Response:
//!   `{"ok": true, "result": <command-specific JSON>}\n`
//!   `{"ok": false, "error": "<message>"}\n`
//!
//! Each TCP-style accept handles exactly one request then closes. Stateless
//! from the client's perspective.
//!
//! Concurrency
//! -----------
//! Single-threaded request handler for v1. At typical agent rates (a few
//! req/s), serial dispatch is fine. Future thread-pool variants should keep
//! using read-only redb handles and avoid reintroducing map-backed query paths.
//!
//! Lifecycle
//! ---------
//! - Daemon binds on start after confirming the redb store is readable.
//! - Idle watcher exits the daemon after `idle_timeout` of inactivity.
//! - Explicit `shutdown` command terminates the loop and removes the socket.
//! - Store updates are explicit: rerun `aethyme-engine-cli index --repo <repo>`
//!   after fragment changes.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::graph::navigation::{
    callers_view_redb, task_anchors_view_redb, task_next_view_redb, task_scope_view_redb,
};
use crate::graph::search::symbol_search_redb;
use crate::model::task::TaskInput;
use crate::store::redb::graph_store::{GraphStore, OverviewV2Limits, ReadOnlyGraphStore};

/// Filename prefix for daemon sockets. Originally a disambiguator: the
/// retired Python daemon (`src/daemon.py`, removed 2026-07-13) shared
/// this `$TMPDIR/aethyme` namespace with its own `aethyme.sock`. The prefix
/// remains stable; the key also includes the full runtime build identity so
/// clients do not attach to a daemon from another build.
const SOCKET_PREFIX: &str = "engine-";
const SOCKET_DIR_NAME: &str = "aethyme";
/// Version of the line-delimited JSON request/response contract.
pub const ENGINE_PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_IDLE_TIMEOUT_SECONDS: u64 = 1800; // 30 min
const RUNTIME_PIN_DIRECTORY: &str = "runtime-pins";
const RUNTIME_UPDATE_LOCK_FILENAME: &str = ".update.lock";
const RUNTIME_PIN_STDIN_ENV: &str = "AETHYME_ENGINE_RUNTIME_PIN_STDIN";
const RUNTIME_PIN_PATH_ENV: &str = "AETHYME_ENGINE_RUNTIME_PIN_PATH";
const VERSION_PROBE_BYTES: usize = 4096;
const VERSION_PROBE_BUDGET: Duration = Duration::from_secs(2);
const MAX_RUNTIME_BINARY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_RUNTIME_VALIDATION_TIME: Duration = Duration::from_secs(12);
const MAX_RUNTIME_VALIDATION_BYTES: u64 = 512 * 1024 * 1024;

/// Stable build identity shared by the router and engine binaries, excluding
/// build time. Keep this in the engine crate so daemon endpoints and readiness
/// checks use the same identity as both front ends.
pub fn runtime_build_identity() -> String {
    let revision = env!("AETHYME_GIT_REVISION");
    let commit = match env!("AETHYME_GIT_COMMIT") {
        "" => "unknown",
        value => value,
    };
    let stable = if revision.is_empty() {
        format!("build_commit={commit}")
    } else {
        format!("build_revision={revision} build_commit={commit}")
    };
    format!("{} ({stable})", env!("CARGO_PKG_VERSION"))
}

/// Compare a bounded `--version` banner to the complete expected source
/// identity. Build dates are intentionally ignored; version, revision and
/// commit must all match.
pub fn version_banner_matches(banner: &str, program: &str, expected: &str) -> bool {
    let Some(first_line) = banner.lines().next() else {
        return false;
    };
    let Some(identity_and_date) = first_line.strip_prefix(program) else {
        return false;
    };
    let identity_and_date = identity_and_date.trim_start();
    identity_and_date == expected
        || identity_and_date.starts_with(&format!("{expected} build_date="))
}

/// Runtime coordination belongs beside the daemon because both updater
/// cleanup and engine startup use the same lock order and pin transfer. The
/// broker owns receipt discovery and bounded candidate selection.
///
/// Shared runtime pin for one immutable installer bundle. The open file's
/// shared flock is released by closing the last descriptor, so a caller may
/// deliberately pass this descriptor to the daemon child.
#[derive(Debug)]
pub struct RuntimeBundlePin {
    file: File,
    path: PathBuf,
    managed_root: PathBuf,
    bundle_name: String,
}

#[derive(Debug)]
pub enum RuntimeBundlePinAttempt {
    Missing,
    Busy,
    Acquired(RuntimeBundlePin),
}

/// Shared or exclusive claim on an installation's persistent update lock.
/// Acquire this before a bundle pin: the updater takes the exclusive update
/// lock before checking bundle pins, so the order is always update then pin.
#[derive(Debug)]
pub struct RuntimeUpdateLock {
    _file: File,
}

impl RuntimeUpdateLock {
    /// Try to keep an updater from switching or pruning while a runtime pair
    /// is resolved and pinned. `None` means an updater currently owns the
    /// exclusive lock.
    pub fn try_shared(managed_root: &Path) -> std::io::Result<Option<Self>> {
        Self::try_lock(managed_root, false)
    }

    /// Try to serialize an updater with runtime resolution and other updates.
    pub fn try_exclusive(managed_root: &Path) -> std::io::Result<Option<Self>> {
        Self::try_lock(managed_root, true)
    }

    fn try_lock(managed_root: &Path, exclusive: bool) -> std::io::Result<Option<Self>> {
        use std::os::unix::fs::OpenOptionsExt;

        let expected_root = managed_root_identity(managed_root)?;
        let path = managed_root.join(RUNTIME_UPDATE_LOCK_FILENAME);
        let mut options = OpenOptions::new();
        options
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .mode(0o600);
        let file = options.open(&path)?;
        validate_update_lock_file(&path, &file)?;
        if managed_root_identity(managed_root)? != expected_root {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "managed runtime root changed while opening update lock",
            ));
        }
        let locked = if exclusive {
            match file.try_lock() {
                Ok(()) => true,
                Err(std::fs::TryLockError::WouldBlock) => false,
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        } else {
            match file.try_lock_shared() {
                Ok(()) => true,
                Err(std::fs::TryLockError::WouldBlock) => false,
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        };
        if !locked {
            return Ok(None);
        }
        if managed_root_identity(managed_root)? != expected_root {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "managed runtime root changed while acquiring update lock",
            ));
        }
        validate_update_lock_file(&path, &file)?;
        Ok(Some(Self { _file: file }))
    }
}

/// Time and aggregate hashing budget shared by a bounded version-pair scan.
pub struct RuntimeValidationBudget {
    deadline: Instant,
    remaining_binary_bytes: u64,
}

impl RuntimeValidationBudget {
    pub fn new(max_elapsed: Duration, max_binary_bytes: u64) -> Self {
        let now = Instant::now();
        let bounded_elapsed = max_elapsed.min(MAX_RUNTIME_VALIDATION_TIME);
        Self {
            deadline: now.checked_add(bounded_elapsed).unwrap_or(now),
            remaining_binary_bytes: max_binary_bytes.min(MAX_RUNTIME_VALIDATION_BYTES),
        }
    }

    fn remaining_time(&self) -> Result<Duration, String> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err("managed runtime validation exceeded its time budget".into())
        } else {
            Ok(remaining)
        }
    }

    fn charge_binary_bytes(&mut self, amount: u64) -> Result<(), String> {
        self.remaining_binary_bytes = self
            .remaining_binary_bytes
            .checked_sub(amount)
            .ok_or_else(|| "managed runtime validation exceeded its byte budget".to_string())?;
        Ok(())
    }

    fn remaining_binary_bytes(&self) -> u64 {
        self.remaining_binary_bytes
    }
}

/// Validated paths from one immutable versioned router/engine pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBundlePaths {
    managed_root: PathBuf,
    bundle_name: String,
    directory: PathBuf,
    router_path: PathBuf,
    engine_path: PathBuf,
    router_sha256: String,
    engine_sha256: String,
}

impl RuntimeBundlePaths {
    pub fn managed_root(&self) -> &Path {
        &self.managed_root
    }

    pub fn bundle_name(&self) -> &str {
        &self.bundle_name
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn router_path(&self) -> &Path {
        &self.router_path
    }

    pub fn engine_path(&self) -> &Path {
        &self.engine_path
    }

    pub fn router_sha256(&self) -> &str {
        &self.router_sha256
    }

    pub fn engine_sha256(&self) -> &str {
        &self.engine_sha256
    }
}

/// Verify one candidate bundle's private paths, full router/engine build
/// identity, and stable executable contents. `Ok(None)` is an ordinary
/// identity mismatch; unsafe or unbounded observations are errors.
pub fn validate_runtime_bundle_pair(
    managed_root: &Path,
    bundle_name: &str,
    expected_identity: &str,
    budget: &mut RuntimeValidationBudget,
) -> Result<Option<RuntimeBundlePaths>, String> {
    validate_bundle_name(bundle_name).map_err(|error| error.to_string())?;
    validate_expected_identity(expected_identity)?;
    let managed_root_before = runtime_directory_identity(managed_root)?;
    let versions = managed_root.join("versions");
    let versions_before = runtime_directory_identity(&versions)?;
    let directory = versions.join(bundle_name);
    let directory_before = runtime_directory_identity(&directory)?;
    if directory.parent() != Some(versions.as_path()) {
        return Err("managed runtime candidate escaped its versions directory".into());
    }

    let router_path = directory.join("aethyme");
    let engine_path = directory.join("aethyme-engine-cli");
    let router_before =
        runtime_executable_identity(&router_path).map_err(|error| error.to_string())?;
    let engine_before =
        runtime_executable_identity(&engine_path).map_err(|error| error.to_string())?;
    if router_before.len > MAX_RUNTIME_BINARY_BYTES || engine_before.len > MAX_RUNTIME_BINARY_BYTES
    {
        return Err("managed runtime binary exceeds the identity budget".into());
    }

    if !probe_version_banner(&router_path, "aethyme", expected_identity, budget)? {
        return Ok(None);
    }
    if !probe_version_banner(
        &engine_path,
        "aethyme-engine-cli",
        expected_identity,
        budget,
    )? {
        return Ok(None);
    }

    let router_after_probe =
        runtime_executable_identity(&router_path).map_err(|error| error.to_string())?;
    let engine_after_probe =
        runtime_executable_identity(&engine_path).map_err(|error| error.to_string())?;
    if router_before != router_after_probe || engine_before != engine_after_probe {
        return Err("managed runtime pair changed during version verification".into());
    }
    let pair_bytes = router_before.length.saturating_add(engine_before.length);
    if pair_bytes > budget.remaining_binary_bytes() {
        return Err("managed runtime validation exceeded its byte budget".into());
    }
    budget.charge_binary_bytes(pair_bytes)?;
    let (router_sha256, _) = runtime_binary_digest(&router_path, router_before, budget)?;
    let (engine_sha256, _) = runtime_binary_digest(&engine_path, engine_before, budget)?;

    if runtime_directory_identity(managed_root)? != managed_root_before
        || runtime_directory_identity(&versions)? != versions_before
        || runtime_directory_identity(&directory)? != directory_before
    {
        return Err("managed runtime directory chain changed during verification".into());
    }

    Ok(Some(RuntimeBundlePaths {
        managed_root: managed_root.to_path_buf(),
        bundle_name: bundle_name.to_string(),
        directory,
        router_path,
        engine_path,
        router_sha256,
        engine_sha256,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimeFileIdentity {
    device: u64,
    inode: u64,
    links: u64,
    length: u64,
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

fn managed_root_identity(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    validate_managed_root(path)?;
    let metadata = fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

fn managed_bundle_location(
    original: &Path,
    resolved: &Path,
) -> Result<Option<(PathBuf, String)>, String> {
    let managed_marker = std::ffi::OsStr::new(".aethyme-managed");
    let mentions_managed_root = original
        .components()
        .chain(resolved.components())
        .any(|component| component.as_os_str() == managed_marker);
    let Some(directory) = resolved.parent() else {
        return if mentions_managed_root {
            Err("managed engine executable has no containing bundle".into())
        } else {
            Ok(None)
        };
    };
    let Some(versions) = directory.parent() else {
        return if mentions_managed_root {
            Err("managed engine executable has no versions directory".into())
        } else {
            Ok(None)
        };
    };
    if versions.file_name().and_then(|name| name.to_str()) != Some("versions") {
        return if mentions_managed_root {
            Err("managed engine executable is not in an immutable version bundle".into())
        } else {
            Ok(None)
        };
    }
    let Some(managed_root) = versions.parent() else {
        return Err("managed engine bundle has no installation root".into());
    };
    if managed_root.file_name().and_then(|name| name.to_str()) != Some(".aethyme-managed")
        || resolved.file_name().and_then(|name| name.to_str()) != Some("aethyme-engine-cli")
    {
        return Err("managed engine executable path is not a recognized runtime bundle".into());
    }
    let bundle_name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("managed engine bundle name is not UTF-8")?;
    validate_bundle_name(bundle_name).map_err(|error| error.to_string())?;
    Ok(Some((managed_root.to_path_buf(), bundle_name.to_string())))
}

fn path_is_managed_install(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == std::ffi::OsStr::new(".aethyme-managed"))
        || path.parent().is_some_and(|parent| {
            fs::symlink_metadata(parent.join(".aethyme-managed")).is_ok()
                || fs::symlink_metadata(parent.join("install-receipt.json")).is_ok()
        })
}

fn acquire_shared_update_lock(managed_root: &Path) -> Result<RuntimeUpdateLock, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(lock) = RuntimeUpdateLock::try_shared(managed_root)
            .map_err(|error| format!("acquire managed runtime shared update lock: {error}"))?
        {
            return Ok(lock);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "managed runtime update is still in progress at {}; retry after it finishes",
                managed_root.join(RUNTIME_UPDATE_LOCK_FILENAME).display()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn validate_update_lock_file(path: &Path, opened: &File) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let named = fs::symlink_metadata(path)?;
    let metadata = opened.metadata()?;
    let uid = unsafe { libc::geteuid() };
    if !named.is_file()
        || !metadata.is_file()
        || named.dev() != metadata.dev()
        || named.ino() != metadata.ino()
        || named.nlink() != 1
        || metadata.nlink() != 1
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
        || fs::canonicalize(path)? != path
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "managed runtime update lock is not a protected unique file",
        ));
    }
    Ok(())
}

fn validate_expected_identity(expected: &str) -> Result<(), String> {
    if expected.is_empty()
        || expected.len() > 512
        || expected.bytes().any(|byte| byte.is_ascii_control())
        || expected.lines().count() != 1
    {
        return Err("managed runtime build identity is invalid or unbounded".into());
    }
    Ok(())
}

fn is_private_directory_metadata(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let uid = unsafe { libc::geteuid() };
    metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o022 == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimeDirectoryIdentity {
    device: u64,
    inode: u64,
    uid: u32,
    mode: u32,
}

fn runtime_directory_identity(path: &Path) -> Result<RuntimeDirectoryIdentity, String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "inspect managed runtime directory {}: {error}",
            path.display()
        )
    })?;
    if !is_private_directory_metadata(&metadata)
        || fs::canonicalize(path).ok().as_deref() != Some(path)
    {
        return Err(format!(
            "managed runtime directory is not a private real directory: {}",
            path.display()
        ));
    }
    Ok(RuntimeDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        uid: metadata.uid(),
        mode: metadata.mode(),
    })
}

fn runtime_executable_identity(path: &Path) -> std::io::Result<RuntimeFileIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path)?;
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o022 != 0
        || fs::canonicalize(path)? != path
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "managed runtime executable is not a protected unique file: {}",
                path.display()
            ),
        ));
    }
    Ok(RuntimeFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        links: metadata.nlink(),
        length: metadata.len(),
        mode: metadata.mode(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

fn runtime_binary_digest(
    path: &Path,
    expected: RuntimeFileIdentity,
    budget: &RuntimeValidationBudget,
) -> Result<(String, u64), String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let file = options
        .open(path)
        .map_err(|error| format!("open managed runtime binary {}: {error}", path.display()))?;
    let before = file.metadata().map_err(|error| error.to_string())?;
    if !same_runtime_file_identity(&before, expected) || before.len() > MAX_RUNTIME_BINARY_BYTES {
        return Err("managed runtime executable changed or exceeds the hash budget".into());
    }
    let mut hash = Sha256::new();
    let mut reader = (&file).take(MAX_RUNTIME_BINARY_BYTES + 1);
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        budget.remaining_time()?;
        let count = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > MAX_RUNTIME_BINARY_BYTES {
            return Err("managed runtime executable grew beyond the hash budget".into());
        }
        hash.update(&buffer[..count]);
    }
    budget.remaining_time()?;
    let after = file.metadata().map_err(|error| error.to_string())?;
    let named = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if total != expected.length
        || !same_runtime_file_identity(&after, expected)
        || !same_runtime_file_identity(&named, expected)
    {
        return Err("managed runtime executable changed during hashing".into());
    }
    Ok((format!("{:x}", hash.finalize()), total))
}

fn same_runtime_file_identity(metadata: &fs::Metadata, expected: RuntimeFileIdentity) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.is_file()
        && metadata.dev() == expected.device
        && metadata.ino() == expected.inode
        && metadata.nlink() == expected.links
        && metadata.len() == expected.length
        && metadata.mode() == expected.mode
        && metadata.mtime() == expected.modified_seconds
        && metadata.mtime_nsec() == expected.modified_nanoseconds
        && metadata.ctime() == expected.changed_seconds
        && metadata.ctime_nsec() == expected.changed_nanoseconds
}

fn probe_version_banner(
    path: &Path,
    program: &str,
    expected_identity: &str,
    budget: &RuntimeValidationBudget,
) -> Result<bool, String> {
    let timeout = budget.remaining_time()?.min(VERSION_PROBE_BUDGET);
    let output = bounded_version_output(path, timeout)?;
    Ok(output.status.success()
        && version_banner_matches(
            &String::from_utf8_lossy(&output.stdout),
            program,
            expected_identity,
        ))
}

struct VersionOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
}

fn bounded_version_output(path: &Path, timeout: Duration) -> Result<VersionOutput, String> {
    let child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove(RUNTIME_PIN_STDIN_ENV)
        .env_remove(RUNTIME_PIN_PATH_ENV)
        .spawn()
        .map_err(|error| format!("run {} --version: {error}", path.display()))?;
    let mut child = VersionProbeChild {
        child,
        reaped: false,
    };
    let stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| "managed runtime version probe has no stdout".to_string())?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take((VERSION_PROBE_BYTES + 1) as u64)
            .read_to_end(&mut bytes);
        let _ = sender.send(result.map(|_| bytes));
    });

    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| "managed runtime version deadline overflow".to_string())?;
    let mut status = None;
    let mut output = None;
    loop {
        if Instant::now() >= deadline {
            return Err("managed runtime version probe exceeded its time budget".into());
        }
        if status.is_none() {
            status = child.child.try_wait().map_err(|error| error.to_string())?;
            child.reaped = status.is_some();
        }
        if let (Some(status), Some(output)) = (status, output.take()) {
            if output.len() > VERSION_PROBE_BYTES {
                return Err("managed runtime version probe exceeded its output budget".into());
            }
            return Ok(VersionOutput {
                status,
                stdout: output,
            });
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok(result) => {
                output = Some(result.map_err(|error| error.to_string())?);
                if output
                    .as_ref()
                    .is_some_and(|bytes| bytes.len() > VERSION_PROBE_BYTES)
                {
                    return Err("managed runtime version probe exceeded its output budget".into());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("managed runtime version output reader disconnected".into());
            }
        }
    }
}

struct VersionProbeChild {
    child: std::process::Child,
    reaped: bool,
}

impl Drop for VersionProbeChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl RuntimeBundlePin {
    /// Pin an immutable bundle by its single safe directory name. Callers
    /// must hold the shared installation update lock until this succeeds.
    pub fn acquire_shared(managed_root: &Path, bundle_name: &str) -> std::io::Result<Self> {
        validate_bundle_name(bundle_name)?;
        let path = open_runtime_pin_file(managed_root, bundle_name, true)?;
        path.file.lock_shared()?;
        Ok(path)
    }

    /// Try the updater's exclusive claim. A busy pin means an active consumer;
    /// a missing pin means no consumer has registered this bundle.
    pub fn try_acquire_exclusive(
        managed_root: &Path,
        bundle_name: &str,
    ) -> std::io::Result<RuntimeBundlePinAttempt> {
        validate_bundle_name(bundle_name)?;
        let Some(pin) = open_runtime_pin_file_if_present(managed_root, bundle_name)? else {
            return Ok(RuntimeBundlePinAttempt::Missing);
        };
        match pin.file.try_lock() {
            Ok(()) => Ok(RuntimeBundlePinAttempt::Acquired(pin)),
            Err(std::fs::TryLockError::WouldBlock) => Ok(RuntimeBundlePinAttempt::Busy),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }

    pub fn bundle_name(&self) -> &str {
        &self.bundle_name
    }

    pub fn managed_root(&self) -> &Path {
        &self.managed_root
    }

    pub fn pin_path(&self) -> &Path {
        &self.path
    }

    pub fn engine_path(&self) -> PathBuf {
        self.managed_root
            .join("versions")
            .join(&self.bundle_name)
            .join("aethyme-engine-cli")
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    /// Remove a stale pin file while holding its exclusive lock and the
    /// installation's exclusive update lock. Keeping the lock file only while
    /// a bundle exists bounds the pin inventory over repeated updates.
    pub fn remove_pin_file(&self) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;

        let named = fs::symlink_metadata(&self.path)?;
        let opened = self.file.metadata()?;
        if !named.is_file()
            || named.dev() != opened.dev()
            || named.ino() != opened.ino()
            || opened.nlink() != 1
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "runtime pin path changed before removal",
            ));
        }
        fs::remove_file(&self.path)
    }

    /// Take the pin file mapped to stdin by the daemon launcher. The marker
    /// alone does not establish the pin: the descriptor's inode, exact path,
    /// executable bundle, and full router/engine identity are all checked.
    /// The child closes the pin on any later exec while retaining it for the
    /// daemon's lifetime.
    pub fn take_inherited_from_environment() -> Result<Option<Self>, String> {
        let Some(pin) = take_inherited_pin_file_from_stdin()? else {
            return Ok(None);
        };
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let executable = fs::canonicalize(&executable).map_err(|error| error.to_string())?;
        let expected_engine = pin.engine_path();
        if executable != expected_engine {
            return Err("inherited runtime pin does not name the running engine bundle".into());
        }
        let mut budget =
            RuntimeValidationBudget::new(Duration::from_secs(8), MAX_RUNTIME_BINARY_BYTES * 2);
        if validate_runtime_bundle_pair(
            &pin.managed_root,
            &pin.bundle_name,
            &runtime_build_identity(),
            &mut budget,
        )?
        .is_none()
        {
            return Err(
                "inherited runtime pin bundle does not match the running engine build".into(),
            );
        }
        Ok(Some(pin))
    }

    /// Pin the exact bundle containing a directly invoked engine executable.
    /// If the public selector now resolves to another build, refuse startup.
    pub fn for_current_executable(expected_identity: &str) -> Result<Option<Self>, String> {
        let executable = std::env::current_exe()
            .map_err(|error| format!("locate engine executable: {error}"))?;
        Self::for_executable(&executable, expected_identity)
    }

    /// Pin the validated managed pair containing `executable`. This also
    /// supports library callers that already have an exact engine path and
    /// must not rediscover it through a mutable selector.
    pub fn for_executable(
        executable: &Path,
        expected_identity: &str,
    ) -> Result<Option<Self>, String> {
        let resolved = match fs::canonicalize(executable) {
            Ok(path) => path,
            Err(error) if path_is_managed_install(executable) => {
                return Err(format!(
                    "resolve managed engine executable {}: {error}",
                    executable.display()
                ));
            }
            Err(_) => return Ok(None),
        };
        let Some((managed_root, bundle_name)) = managed_bundle_location(executable, &resolved)?
        else {
            return Ok(None);
        };
        let _update_lock = acquire_shared_update_lock(&managed_root)?;
        let mut budget =
            RuntimeValidationBudget::new(Duration::from_secs(8), MAX_RUNTIME_BINARY_BYTES * 2);
        let pair = validate_runtime_bundle_pair(
            &managed_root,
            &bundle_name,
            expected_identity,
            &mut budget,
        )?
        .ok_or_else(|| {
            "managed engine bundle does not match its running build identity; start through the installed router so it can resolve a matching pair, or run `aethyme update apply` and retry".to_string()
        })?;
        if pair.engine_path != resolved {
            return Err("current engine path is not the validated versioned runtime binary".into());
        }
        Self::acquire_shared(&managed_root, &bundle_name)
            .map(Some)
            .map_err(|error| format!("pin managed engine bundle: {error}"))
    }
}

fn take_inherited_pin_file_from_stdin() -> Result<Option<RuntimeBundlePin>, String> {
    let marker = std::env::var_os(RUNTIME_PIN_STDIN_ENV);
    let path_value = std::env::var_os(RUNTIME_PIN_PATH_ENV);
    if marker.is_none() && path_value.is_none() {
        return Ok(None);
    }
    if marker.as_deref() != Some(std::ffi::OsStr::new("1")) {
        return Err("inherited runtime pin marker is invalid".into());
    }
    let path_value = path_value.ok_or("inherited runtime pin path is missing")?;
    // SAFETY: the launcher maps the verified pin file to stdin only when
    // it sets the marker above. Taking ownership closes fd 0 when the
    // retained pin is dropped; daemon serve never reads stdin.
    if unsafe { libc::fcntl(0, libc::F_GETFD) } < 0 {
        return Err("inherited runtime pin stdin is not open".into());
    }
    let path = PathBuf::from(path_value);
    // SAFETY: fd 0 is the descriptor supplied by start_detached and the
    // pin guard becomes its sole Rust owner in this child.
    let file = unsafe { File::from_raw_fd(0) };
    set_close_on_exec(file.as_raw_fd(), true).map_err(|error| error.to_string())?;
    let pin = validate_open_runtime_pin_file(file, &path)?;
    Ok(Some(pin))
}

fn validate_bundle_name(bundle_name: &str) -> std::io::Result<()> {
    if bundle_name.len() > 160
        || !bundle_name.starts_with('v')
        || !bundle_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "runtime bundle name is not a bounded safe component",
        ));
    }
    Ok(())
}

fn open_runtime_pin_file(
    managed_root: &Path,
    bundle_name: &str,
    create: bool,
) -> std::io::Result<RuntimeBundlePin> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

    validate_managed_root(managed_root)?;
    let pin_directory = managed_root.join(RUNTIME_PIN_DIRECTORY);
    if create {
        match std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&pin_directory)
        {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let pin_dir_metadata = match fs::symlink_metadata(&pin_directory) {
        Ok(metadata) => metadata,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let uid = unsafe { libc::geteuid() };
    if !pin_dir_metadata.is_dir()
        || pin_dir_metadata.uid() != uid
        || pin_dir_metadata.mode() & 0o022 != 0
        || fs::canonicalize(&pin_directory)? != pin_directory
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "runtime pin directory is not a private real directory",
        ));
    }
    let path = pin_directory.join(format!("{bundle_name}.lock"));
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .mode(0o600);
    if create {
        options.create(true);
    }
    let file = options.open(&path)?;
    validate_open_runtime_pin_file(file, &path)
}

fn open_runtime_pin_file_if_present(
    managed_root: &Path,
    bundle_name: &str,
) -> std::io::Result<Option<RuntimeBundlePin>> {
    let pin_directory = managed_root.join(RUNTIME_PIN_DIRECTORY);
    match fs::symlink_metadata(&pin_directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    match open_runtime_pin_file(managed_root, bundle_name, false) {
        Ok(pin) => Ok(Some(pin)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn validate_open_runtime_pin_file(file: File, path: &Path) -> std::io::Result<RuntimeBundlePin> {
    use std::os::unix::fs::MetadataExt;
    let named = fs::symlink_metadata(path)?;
    let opened = file.metadata()?;
    let uid = unsafe { libc::geteuid() };
    if !named.is_file()
        || !opened.is_file()
        || named.dev() != opened.dev()
        || named.ino() != opened.ino()
        || opened.uid() != uid
        || opened.nlink() != 1
        || opened.mode() & 0o077 != 0
        || fs::canonicalize(path)? != path
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "runtime pin is not a protected unique file",
        ));
    }
    let pin_directory = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "pin has no parent")
    })?;
    let managed_root = pin_directory
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "pin has no root"))?;
    validate_managed_root(managed_root)?;
    if pin_directory.file_name().and_then(|name| name.to_str()) != Some(RUNTIME_PIN_DIRECTORY) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "runtime pin path is outside the managed pin directory",
        ));
    }
    let bundle_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".lock"))
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid pin filename")
        })?;
    validate_bundle_name(bundle_name)?;
    Ok(RuntimeBundlePin {
        file,
        path: path.to_path_buf(),
        managed_root: managed_root.to_path_buf(),
        bundle_name: bundle_name.to_string(),
    })
}

fn validate_managed_root(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path)?;
    let uid = unsafe { libc::geteuid() };
    if path.file_name().and_then(|name| name.to_str()) != Some(".aethyme-managed")
        || !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.mode() & 0o022 != 0
        || fs::canonicalize(path)? != path
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "managed runtime root is not a private real directory",
        ));
    }
    Ok(())
}

fn set_close_on_exec(fd: RawFd, enabled: bool) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let updated = if enabled {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, updated) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Compute the unix socket path for `repo`.
///
/// macOS caps AF_UNIX paths at ~104 bytes, so the default socket lives in
/// `$TMPDIR/aethyme` keyed by a stable hash of the resolved repo path and full
/// runtime build identity. Sandboxed
/// runners can set `AETHYME_ENGINE_SOCKET_DIR` to a writable short path such as
/// `/tmp/aethyme-engine-sockets`; the client and detached daemon both resolve
/// this function, so the override stays in sync across process boundaries.
pub fn socket_path_for(repo: &Path) -> PathBuf {
    socket_path_for_identity(repo, &runtime_build_identity())
}

/// Compute the socket for a specific immutable daemon identity. Readiness
/// checks use this with the caller's embedded identity, while the server uses
/// its own compiled identity through [`socket_path_for`].
pub fn socket_path_for_identity(repo: &Path, identity: &str) -> PathBuf {
    socket_path_in_dir(repo, &socket_dir(), identity)
}

fn socket_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("AETHYME_ENGINE_SOCKET_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        return dir;
    }
    let tmp = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    tmp.join(SOCKET_DIR_NAME)
}

fn socket_path_in_dir(repo: &Path, socket_dir: &Path, identity: &str) -> PathBuf {
    let canonical = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(ENGINE_PROTOCOL_VERSION.to_le_bytes());
    hasher.update([0]);
    hasher.update(identity.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    socket_dir.join(format!("{SOCKET_PREFIX}{hex}.sock"))
}

fn runtime_identity_suffix(identity: &str) -> String {
    let digest = Sha256::digest(identity.as_bytes());
    digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Compute the pidfile path inside the repo's `.aethyme/` directory.
pub fn pidfile_path_for(repo: &Path) -> PathBuf {
    pidfile_path_for_identity(repo, &runtime_build_identity())
}

pub fn pidfile_path_for_identity(repo: &Path, identity: &str) -> PathBuf {
    repo.join(".aethyme").join(format!(
        "engine-daemon-{}.pid",
        runtime_identity_suffix(identity)
    ))
}

/// Compute the logfile path inside the repo's `.aethyme/` directory.
pub fn logfile_path_for(repo: &Path) -> PathBuf {
    logfile_path_for_identity(repo, &runtime_build_identity())
}

pub fn logfile_path_for_identity(repo: &Path, _identity: &str) -> PathBuf {
    repo.join(".aethyme").join("engine-daemon.log")
}

/// Options for [`start_detached`].
#[derive(Default)]
pub struct StartOptions {
    pub idle_timeout: Option<String>,
}

/// Outcome of [`start_detached`].
pub enum StartOutcome {
    /// A live daemon already owns the pidfile.
    AlreadyRunning(i32),
    /// A new server process was spawned (detached, own session). The
    /// caller receives the [`std::process::Child`] handle: a spawned
    /// server that dies on startup becomes a zombie of the *caller*
    /// until reaped, so liveness must be checked with `try_wait()` on
    /// this handle — a `kill(pid, 0)` probe reports zombies as alive.
    Spawned(std::process::Child),
}

/// Spawn a detached engine-daemon server for `repo`, using `serve_exe` as
/// the server binary (it must understand `daemon serve --repo <path>` —
/// i.e. `aethyme-engine-cli`). Shared by both front-end binaries so the
/// pidfile/logfile/setsid lifecycle cannot drift between them.
///
/// Idempotent: returns [`StartOutcome::AlreadyRunning`] when a live pid
/// holds the pidfile; stale pidfiles are cleaned and respawned.
pub fn start_detached(
    repo: &Path,
    serve_exe: &Path,
    opts: &StartOptions,
) -> Result<StartOutcome, String> {
    let pin = RuntimeBundlePin::for_executable(serve_exe, &runtime_build_identity())?;
    if let Some(pin) = pin.as_ref()
        && fs::canonicalize(serve_exe).ok().as_deref() != Some(pin.engine_path().as_path())
    {
        return Err(
            "managed daemon launch must use the engine binary from the validated runtime pair"
                .into(),
        );
    }
    start_detached_with_runtime_pin(repo, serve_exe, opts, pin.as_ref())
}

/// Spawn a detached daemon and, when managed, transfer the exact bundle pin
/// through stdin. The daemon takes ownership before the launcher releases its
/// copy, so updater pruning never sees a pin gap.
pub fn start_detached_with_runtime_pin(
    repo: &Path,
    serve_exe: &Path,
    opts: &StartOptions,
    pin: Option<&RuntimeBundlePin>,
) -> Result<StartOutcome, String> {
    if let Some(pin) = pin
        && fs::canonicalize(serve_exe).ok().as_deref() != Some(pin.engine_path().as_path())
    {
        return Err("daemon executable does not match its runtime bundle pin".into());
    }
    let pidfile = pidfile_path_for(repo);
    if pidfile.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pidfile)
            && let Ok(pid) = pid_str.trim().parse::<i32>()
        {
            // SAFETY: `kill` with signal 0 performs the existence and
            // permission check without delivering a signal, and it takes the
            // pid by value. It dereferences no pointer this crate owns, so
            // `pid` only has to be a valid i32, which `parse` just produced.
            let alive = unsafe { libc::kill(pid, 0) };
            if alive == 0 {
                return Ok(StartOutcome::AlreadyRunning(pid));
            }
        }
        let _ = std::fs::remove_file(&pidfile);
    }

    let aethyme_dir = repo.join(".aethyme");
    std::fs::create_dir_all(&aethyme_dir).map_err(|e| format!("create .aethyme: {e}"))?;

    let log_path = logfile_path_for(repo);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("open logfile {}: {}", log_path.display(), e))?;
    let log_stdout = log_file
        .try_clone()
        .map_err(|e| format!("clone log fd: {e}"))?;
    let log_stderr = log_file
        .try_clone()
        .map_err(|e| format!("clone log fd: {e}"))?;

    let mut cmd = std::process::Command::new(serve_exe);
    cmd.arg("daemon").arg("serve").arg("--repo").arg(repo);
    if let Some(idle) = &opts.idle_timeout {
        cmd.arg("--idle-timeout").arg(idle);
    }
    if let Some(pin) = pin {
        let pin_file = pin
            .file()
            .try_clone()
            .map_err(|error| format!("clone runtime pin for daemon child: {error}"))?;
        cmd.stdin(std::process::Stdio::from(pin_file))
            .env(RUNTIME_PIN_STDIN_ENV, "1")
            .env(RUNTIME_PIN_PATH_ENV, pin.pin_path());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }
    cmd.stdout(std::process::Stdio::from(log_stdout))
        .stderr(std::process::Stdio::from(log_stderr));

    use std::os::unix::process::CommandExt;
    // SAFETY: `pre_exec` is documented as safe to call with a closure that is
    // async-signal-safe, and this one only calls `setsid` and converts errno
    // into an `io::Error` — no allocation, no locking, no Rust destructors
    // running between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    let pid = child.id();
    std::fs::write(&pidfile, pid.to_string()).map_err(|e| format!("write pidfile: {e}"))?;
    Ok(StartOutcome::Spawned(child))
}

/// Outcome of [`wait_until_ready`].
#[derive(PartialEq, Eq, Debug)]
pub enum ReadyOutcome {
    /// The daemon reported the expected protocol and full build identity.
    Ready,
    /// A process answered on the identity-keyed socket with another protocol
    /// or build identity; do not attach to it.
    IdentityMismatch,
    /// The watched server process exited before the socket appeared —
    /// failing fast beats sitting out the timeout (a daemon that dies on
    /// startup, e.g. un-indexed repo, dies within milliseconds).
    ProcessExited,
    /// Deadline passed with the process still alive but no socket.
    TimedOut,
}

/// Block until the daemon for `repo` accepts a socket connection, the
/// watched process dies, or the deadline passes. The socket binds only
/// after the redb store has been opened and smoke-read.
pub fn wait_until_ready(
    repo: &Path,
    watch: Option<&mut std::process::Child>,
    timeout: std::time::Duration,
) -> ReadyOutcome {
    wait_until_ready_for_identity(repo, watch, timeout, &runtime_build_identity())
}

/// Wait for a daemon that proves the requested protocol and full source
/// identity over the identity-specific socket.
pub fn wait_until_ready_for_identity(
    repo: &Path,
    mut watch: Option<&mut std::process::Child>,
    timeout: Duration,
    expected_identity: &str,
) -> ReadyOutcome {
    let socket = socket_path_for_identity(repo, expected_identity);
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Some(matches) = daemon_identity_matches(&socket, expected_identity) {
            return if matches {
                ReadyOutcome::Ready
            } else {
                ReadyOutcome::IdentityMismatch
            };
        }
        // try_wait() reaps: a kill(pid, 0) probe would report the child
        // as alive forever once it zombifies under the waiting caller.
        if let Some(child) = watch.as_deref_mut()
            && matches!(child.try_wait(), Ok(Some(_)))
        {
            return ReadyOutcome::ProcessExited;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    ReadyOutcome::TimedOut
}

fn daemon_identity_matches(socket: &Path, expected_identity: &str) -> Option<bool> {
    let mut stream = UnixStream::connect(socket).ok()?;
    let timeout = Duration::from_millis(500);
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return Some(false);
    }
    if stream.write_all(b"{\"command\":\"identity\"}\n").is_err()
        || stream.shutdown(std::net::Shutdown::Write).is_err()
    {
        return Some(false);
    }
    let mut response = Vec::new();
    if stream
        .take((VERSION_PROBE_BYTES + 1) as u64)
        .read_to_end(&mut response)
        .is_err()
        || response.len() > VERSION_PROBE_BYTES
    {
        return Some(false);
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&response) else {
        return Some(false);
    };
    Some(daemon_identity_response_matches(&value, expected_identity))
}

fn daemon_identity_response_matches(value: &serde_json::Value, expected_identity: &str) -> bool {
    value.get("ok").and_then(serde_json::Value::as_bool) == Some(true)
        && value
            .get("protocol_version")
            .and_then(serde_json::Value::as_u64)
            == Some(ENGINE_PROTOCOL_VERSION as u64)
        && value
            .get("build_identity")
            .and_then(serde_json::Value::as_str)
            == Some(expected_identity)
}

/// Last few lines of the daemon logfile — the diagnostic callers show
/// when startup fails.
pub fn log_tail(repo: &Path, lines: usize) -> String {
    let path = logfile_path_for(repo);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            let start = all.len().saturating_sub(lines);
            all[start..].join("\n")
        }
        Err(_) => format!("(no log at {})", path.display()),
    }
}

/// Server state held across requests.
struct DaemonState {
    repo: PathBuf,
    last_activity: Instant,
}

/// Configurable parameters for daemon lifecycle.
pub struct DaemonConfig {
    pub repo: PathBuf,
    pub idle_timeout: Duration,
}

impl DaemonConfig {
    pub fn new(repo: PathBuf) -> Self {
        Self {
            repo,
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECONDS),
        }
    }

    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }
}

/// Run the daemon for `config.repo` until idle timeout or `shutdown` request.
///
/// Returns when the listen loop exits cleanly (idle, shutdown, or socket
/// error). Redb open/read errors are returned immediately without ever
/// opening the socket.
pub fn serve_forever(config: DaemonConfig) -> Result<(), String> {
    let socket_path = socket_path_for(&config.repo);
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create socket dir {}: {}", parent.display(), e))?;
    }
    if socket_path.exists() {
        // Stale socket from a previous daemon — try connecting; if connection
        // fails, the previous daemon is gone and we can safely unlink.
        match UnixStream::connect(&socket_path) {
            Ok(_) => {
                return Err(format!(
                    "engine daemon already running at {}",
                    socket_path.display()
                ));
            }
            Err(_) => {
                let _ = std::fs::remove_file(&socket_path);
            }
        }
    }

    eprintln!(
        "aethyme-engine-daemon: opening redb graph store for {} ...",
        config.repo.display()
    );
    let open_started = Instant::now();
    let store = open_daemon_store(&config.repo)?;
    let overview = store
        .overview_v2(OverviewV2Limits::default())
        .map_err(|e| format!("overview_v2: {e}"))?;
    let file_count = overview
        .repo
        .as_ref()
        .map(|repo| repo.file_count)
        .unwrap_or(0);
    let function_sample_count = overview.functions.len();
    drop(store);
    eprintln!(
        "aethyme-engine-daemon: redb ready ({} files, {} sampled functions, open {:?})",
        file_count,
        function_sample_count,
        open_started.elapsed()
    );

    let state = Arc::new(Mutex::new(DaemonState {
        repo: config.repo.clone(),
        last_activity: Instant::now(),
    }));

    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| format!("bind {}: {}", socket_path.display(), e))?;
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("set_nonblocking: {e}"))?;
    eprintln!(
        "aethyme-engine-daemon: listening on {} (idle timeout {:?})",
        socket_path.display(),
        config.idle_timeout
    );

    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Idle-watcher thread: shuts the daemon down after inactivity. We wake
    // up periodically rather than blocking on a long sleep so SIGTERM-driven
    // shutdown is responsive.
    {
        let state = Arc::clone(&state);
        let shutdown = Arc::clone(&shutdown);
        let socket_path = socket_path.clone();
        let idle_timeout = config.idle_timeout;
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(15));
                if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let idle = {
                    let s = state.lock().expect("daemon state lock poisoned");
                    s.last_activity.elapsed()
                };
                if idle >= idle_timeout {
                    eprintln!("aethyme-engine-daemon: idle for {:?}, shutting down", idle);
                    shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
                    // Wake the accept() blocking call by connecting to ourself.
                    let _ = UnixStream::connect(&socket_path);
                    break;
                }
            }
        });
    }

    for incoming in listener.incoming() {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let stream = match incoming {
            Ok(s) => s,
            Err(e) => {
                eprintln!("aethyme-engine-daemon: accept error: {e}");
                continue;
            }
        };
        let request_shutdown = handle_request(stream, &state);
        if request_shutdown {
            shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
            break;
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    eprintln!("aethyme-engine-daemon: stopped");
    Ok(())
}

/// Handle one request. Returns `true` if the request was a shutdown command.
fn handle_request(mut stream: UnixStream, state: &Arc<Mutex<DaemonState>>) -> bool {
    // Bound the client's request size so a misbehaving client can't pin us.
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') || buf.len() > 64 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if buf.is_empty() {
        return false;
    }

    let line = String::from_utf8_lossy(&buf).into_owned();
    let line = line.split('\n').next().unwrap_or("").trim();

    let request: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            send_error(&mut stream, &format!("invalid JSON request: {e}"));
            return false;
        }
    };

    let command = request
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    {
        let mut s = state.lock().expect("daemon state lock poisoned");
        s.last_activity = Instant::now();
    }

    match command {
        "identity" => {
            send_ok_value(
                &mut stream,
                serde_json::json!({
                    "ok": true,
                    "protocol_version": ENGINE_PROTOCOL_VERSION,
                    "build_identity": runtime_build_identity(),
                }),
            );
            false
        }
        "ping" => {
            let repo = repo_from_state(state);
            match open_daemon_store(&repo).and_then(|store| daemon_ping_payload(&repo, &store)) {
                Ok(payload) => send_ok_value(&mut stream, payload),
                Err(e) => send_error(&mut stream, &e),
            }
            false
        }
        "shutdown" => {
            send_ok_value(&mut stream, serde_json::json!({"ok": true}));
            true
        }
        "task-localize" => {
            let task_text = request.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let repo = repo_from_state(state);
            let task = TaskInput::from_task_text(task_text);
            match open_daemon_store(&repo).and_then(|store| {
                let anchors = task_anchors_view_redb(&store, &task).map_err(|e| e.to_string())?;
                let scope = task_scope_view_redb(&store, &task).map_err(|e| e.to_string())?;
                let next = task_next_view_redb(&store, &task).map_err(|e| e.to_string())?;
                Ok(crate::json::task_localization_view(&anchors, &scope, &next))
            }) {
                Ok(view) => send_ok_raw(&mut stream, &view),
                Err(e) => send_error(&mut stream, &e),
            }
            false
        }
        "symbol-batch" => {
            // Run the redb-backed V2 matcher for each query.
            // Returns a JSON object keyed by query: {query → [SearchHit, ...]}.
            // Mirrors the `aethyme-engine-cli symbol-batch` shape so a client
            // can route through the daemon or the CLI interchangeably.
            let queries: Vec<String> = request
                .get("queries")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let limit = request.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

            if queries.is_empty() {
                send_error(&mut stream, "missing or empty `queries` array");
                return false;
            }

            let repo = repo_from_state(state);
            match open_daemon_store(&repo).and_then(|store| {
                let mut results: serde_json::Map<String, serde_json::Value> =
                    serde_json::Map::new();
                for query in &queries {
                    let hits =
                        symbol_search_redb(&store, query, limit).map_err(|e| e.to_string())?;
                    let arr: Vec<serde_json::Value> = hits
                        .into_iter()
                        .map(|h| {
                            serde_json::json!({
                                "id": h.id,
                                "name": h.name,
                                "kind": h.kind,
                                "file": h.file,
                                "line": h.line,
                                "score": h.score,
                                "reason": h.reason,
                            })
                        })
                        .collect();
                    results.insert(query.clone(), serde_json::Value::Array(arr));
                }
                Ok(serde_json::json!({
                    "ok": true,
                    "result": serde_json::Value::Object(results),
                }))
            }) {
                Ok(payload) => send_ok_value(&mut stream, payload),
                Err(e) => send_error(&mut stream, &e),
            }
            false
        }
        "callers-of" => {
            // Look up callers through redb relation views. Returns map keyed by symbol id:
            // `{symbol_id → [{ "id": ..., "label": ..., "path": ... },
            // ...]}`. Symbols not found in the graph store map to empty
            // arrays (not errors — common when a symbol-search hit
            // came from a file with no callgraph edges yet).
            let targets: Vec<String> = request
                .get("targets")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if targets.is_empty() {
                send_error(&mut stream, "missing or empty `targets` array");
                return false;
            }
            let repo = repo_from_state(state);
            match open_daemon_store(&repo).and_then(|store| {
                let mut results: serde_json::Map<String, serde_json::Value> =
                    serde_json::Map::new();
                for target in &targets {
                    let view = callers_view_redb(&store, target).map_err(|e| e.to_string())?;
                    let arr: Vec<serde_json::Value> = view
                        .items
                        .into_iter()
                        .map(|item| {
                            serde_json::json!({
                                "id": item.id,
                                "kind": item.kind,
                                "display": item.display,
                                "relation": item.relation,
                                "confidence": item.confidence,
                            })
                        })
                        .collect();
                    results.insert(target.clone(), serde_json::Value::Array(arr));
                }
                Ok(serde_json::json!({
                    "ok": true,
                    "result": serde_json::Value::Object(results),
                }))
            }) {
                Ok(payload) => send_ok_value(&mut stream, payload),
                Err(e) => send_error(&mut stream, &e),
            }
            false
        }
        other => {
            send_error(&mut stream, &format!("unknown command: {other:?}"));
            false
        }
    }
}

fn repo_from_state(state: &Arc<Mutex<DaemonState>>) -> PathBuf {
    let s = state.lock().expect("daemon state lock poisoned");
    s.repo.clone()
}

fn open_daemon_store(repo: &Path) -> Result<ReadOnlyGraphStore, String> {
    GraphStore::open_read_only(repo).map_err(|e| e.to_string())
}

fn daemon_ping_payload(
    repo: &Path,
    store: &ReadOnlyGraphStore,
) -> Result<serde_json::Value, String> {
    let overview = store
        .overview_v2(OverviewV2Limits::default())
        .map_err(|e| e.to_string())?;
    let files = overview
        .repo
        .as_ref()
        .map(|repo| repo.file_count)
        .unwrap_or(0);
    let functions = store
        .functions_under_path("")
        .map_err(|e| e.to_string())?
        .len();
    let edges = store.edge_count().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "ok": true,
        "repo": repo.display().to_string(),
        "backend": "redb",
        "files": files,
        "functions": functions,
        "edges": edges,
    }))
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    ok: bool,
    error: &'a str,
}

fn send_error(stream: &mut UnixStream, message: &str) {
    let env = ErrorEnvelope {
        ok: false,
        error: message,
    };
    if let Ok(s) = serde_json::to_string(&env) {
        let _ = stream.write_all(s.as_bytes());
        let _ = stream.write_all(b"\n");
    }
}

fn send_ok_value(stream: &mut UnixStream, value: serde_json::Value) {
    if let Ok(s) = serde_json::to_string(&value) {
        let _ = stream.write_all(s.as_bytes());
        let _ = stream.write_all(b"\n");
    }
}

/// Send `{"ok": true, "result": <raw_json>}` without re-parsing `raw_json`.
fn send_ok_raw(stream: &mut UnixStream, raw_json: &str) {
    let _ = stream.write_all(br#"{"ok":true,"result":"#);
    let _ = stream.write_all(raw_json.as_bytes());
    let _ = stream.write_all(b"}\n");
}

/// Convenience client: send one JSON request to the daemon and read the
/// response line. Used by the CLI's `daemon status` and by tests.
pub fn send_request(socket: &Path, request: &serde_json::Value) -> Result<String, String> {
    let mut stream =
        UnixStream::connect(socket).map_err(|e| format!("connect {}: {}", socket.display(), e))?;
    let bytes = serde_json::to_vec(request).map_err(|e| format!("encode: {e}"))?;
    stream
        .write_all(&bytes)
        .map_err(|e| format!("write: {e}"))?;
    stream.write_all(b"\n").map_err(|e| format!("write: {e}"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| format!("shutdown write: {e}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| format!("read: {e}"))?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_is_in_aethyme_namespace() {
        let path = socket_path_for(Path::new("/some/repo/path"));
        let parent = path.parent().expect("socket has a parent");
        assert_eq!(
            parent.file_name().and_then(|n| n.to_str()),
            Some(SOCKET_DIR_NAME)
        );
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(SOCKET_PREFIX));
        assert!(name.ends_with(".sock"));
    }

    #[test]
    fn socket_path_is_stable_for_same_repo() {
        let a = socket_path_for(Path::new("/some/repo"));
        let b = socket_path_for(Path::new("/some/repo"));
        assert_eq!(a, b);
    }

    #[test]
    fn socket_path_differs_per_repo() {
        let a = socket_path_for(Path::new("/some/repo"));
        let b = socket_path_for(Path::new("/some/other-repo"));
        assert_ne!(a, b);
    }

    #[test]
    fn socket_path_can_use_explicit_socket_dir() {
        let dir = Path::new("/tmp/aethyme-codex-engine-sockets");
        let path = socket_path_in_dir(Path::new("/some/repo"), dir, "test-build");
        assert_eq!(path.parent(), Some(dir));
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(SOCKET_PREFIX));
        assert!(name.ends_with(".sock"));
    }

    #[test]
    fn daemon_endpoints_are_keyed_by_full_build_identity() {
        let repo = Path::new("/some/repo");
        let first = "0.8.51 (build_revision=first build_commit=aaaa)";
        let second = "0.8.51 (build_revision=second build_commit=bbbb)";
        assert_ne!(
            socket_path_for_identity(repo, first),
            socket_path_for_identity(repo, second)
        );
        assert_ne!(
            pidfile_path_for_identity(repo, first),
            pidfile_path_for_identity(repo, second)
        );
        assert!(pidfile_path_for_identity(repo, first).starts_with(repo.join(".aethyme")));
        assert_eq!(
            logfile_path_for_identity(repo, first),
            Path::new("/some/repo/.aethyme/engine-daemon.log")
        );
    }

    #[test]
    fn daemon_identity_handshake_requires_protocol_and_full_identity() {
        let identity = "0.8.51 (build_revision=abc build_commit=def)";
        let exact = serde_json::json!({
            "ok": true,
            "protocol_version": ENGINE_PROTOCOL_VERSION,
            "build_identity": identity,
        });
        assert!(daemon_identity_response_matches(&exact, identity));
        let wrong_build = serde_json::json!({
            "ok": true,
            "protocol_version": ENGINE_PROTOCOL_VERSION,
            "build_identity": "0.8.51 (build_revision=abc build_commit=xyz)",
        });
        assert!(!daemon_identity_response_matches(&wrong_build, identity));
        let wrong_protocol = serde_json::json!({
            "ok": true,
            "protocol_version": ENGINE_PROTOCOL_VERSION + 1,
            "build_identity": identity,
        });
        assert!(!daemon_identity_response_matches(&wrong_protocol, identity));
    }

    #[test]
    fn version_banner_requires_program_and_full_identity_but_ignores_build_date() {
        let identity = "0.8.51 (build_revision=abc build_commit=def)";
        assert!(version_banner_matches(
            "aethyme 0.8.51 (build_revision=abc build_commit=def) build_date=2026-10-31",
            "aethyme",
            identity
        ));
        assert!(!version_banner_matches(
            "aethyme 0.8.51 (build_revision=abc build_commit=xyz)",
            "aethyme",
            identity
        ));
        assert!(!version_banner_matches(
            "aethyme-engine-cli 0.8.51 (build_revision=abc build_commit=def)",
            "aethyme",
            identity
        ));
    }

    #[test]
    fn daemon_pin_lifetime_is_close_only() {
        let repository = tempfile::tempdir().unwrap();
        let managed_root = repository.path().join(".aethyme-managed");
        std::fs::create_dir(&managed_root).unwrap();
        let pin = RuntimeBundlePin::acquire_shared(&managed_root, "v0.8.51-0123456789ab").unwrap();
        let child_copy = pin.file().try_clone().unwrap();
        drop(pin);

        let exclusive =
            RuntimeBundlePin::try_acquire_exclusive(&managed_root, "v0.8.51-0123456789ab").unwrap();
        assert!(matches!(exclusive, RuntimeBundlePinAttempt::Busy));
        drop(child_copy);
        assert!(matches!(
            RuntimeBundlePin::try_acquire_exclusive(&managed_root, "v0.8.51-0123456789ab").unwrap(),
            RuntimeBundlePinAttempt::Acquired(_)
        ));
    }

    #[test]
    fn shared_update_lock_excludes_exclusive_updater_and_releases_on_drop() {
        let repository = tempfile::tempdir().unwrap();
        let managed_root = repository.path().join(".aethyme-managed");
        std::fs::create_dir(&managed_root).unwrap();

        let shared = RuntimeUpdateLock::try_shared(&managed_root)
            .unwrap()
            .unwrap();
        assert!(
            RuntimeUpdateLock::try_exclusive(&managed_root)
                .unwrap()
                .is_none()
        );
        drop(shared);

        let exclusive = RuntimeUpdateLock::try_exclusive(&managed_root)
            .unwrap()
            .unwrap();
        assert!(
            RuntimeUpdateLock::try_shared(&managed_root)
                .unwrap()
                .is_none()
        );
        drop(exclusive);
        assert!(
            RuntimeUpdateLock::try_shared(&managed_root)
                .unwrap()
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn runtime_pair_validation_requires_both_binaries_to_match_full_identity() {
        use std::os::unix::fs::PermissionsExt;

        fn write_version_script(path: &Path, banner: &str) {
            fs::write(path, format!("#!/bin/sh\nprintf '%s\\n' \"{banner}\"\n")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let temp = tempfile::tempdir().unwrap();
        let managed_root = temp.path().join(".aethyme-managed");
        std::fs::create_dir(&managed_root).unwrap();
        let versions = managed_root.join("versions");
        std::fs::create_dir(&versions).unwrap();
        let bundle_name = format!("v{}-0123456789ab", env!("CARGO_PKG_VERSION"));
        let bundle = versions.join(&bundle_name);
        std::fs::create_dir(&bundle).unwrap();
        let identity = runtime_build_identity();
        write_version_script(
            &bundle.join("aethyme"),
            &format!("aethyme {identity} build_date=test"),
        );
        write_version_script(
            &bundle.join("aethyme-engine-cli"),
            &format!("aethyme-engine-cli {identity} build_date=test"),
        );
        let managed_root = fs::canonicalize(managed_root).unwrap();

        let mut budget = RuntimeValidationBudget::new(Duration::from_secs(5), 1024 * 1024);
        let pair =
            validate_runtime_bundle_pair(&managed_root, &bundle_name, &identity, &mut budget)
                .unwrap()
                .unwrap();
        assert_eq!(pair.bundle_name(), bundle_name);
        assert_eq!(pair.engine_path(), bundle.join("aethyme-engine-cli"));
        assert_eq!(pair.router_sha256().len(), 64);
        assert_eq!(pair.engine_sha256().len(), 64);

        let mut mismatch_budget = RuntimeValidationBudget::new(Duration::from_secs(5), 1024 * 1024);
        assert!(
            validate_runtime_bundle_pair(
                &managed_root,
                &bundle_name,
                "0.8.51 (build_revision=wrong build_commit=wrong)",
                &mut mismatch_budget,
            )
            .unwrap()
            .is_none()
        );
    }

    #[cfg(unix)]
    fn write_test_runtime_binary(path: &Path, program: &str, identity: &str) {
        use std::os::unix::fs::PermissionsExt;

        let banner = format!("{program} {identity} build_date=test");
        fs::write(path, format!("#!/bin/sh\nprintf '%s\\n' \"{banner}\"\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn direct_managed_engine_start_pins_the_exact_matching_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let managed_root = temp.path().join(".aethyme-managed");
        let bundle_name = format!("v{}-0123456789ab", env!("CARGO_PKG_VERSION"));
        let bundle = managed_root.join("versions").join(&bundle_name);
        fs::create_dir_all(&bundle).unwrap();
        let identity = runtime_build_identity();
        write_test_runtime_binary(&bundle.join("aethyme"), "aethyme", &identity);
        let engine_path = bundle.join("aethyme-engine-cli");
        write_test_runtime_binary(&engine_path, "aethyme-engine-cli", &identity);

        let pin = RuntimeBundlePin::for_executable(&engine_path, &identity)
            .unwrap()
            .expect("managed executable should be pinned");

        assert_eq!(pin.bundle_name(), bundle_name);
        assert_eq!(pin.engine_path(), engine_path);
    }

    #[cfg(unix)]
    #[test]
    fn direct_unmanaged_engine_start_keeps_the_unmanaged_path() {
        let temp = tempfile::tempdir().unwrap();
        let engine_path = temp.path().join("aethyme-engine-cli");
        write_test_runtime_binary(
            &engine_path,
            "aethyme-engine-cli",
            &runtime_build_identity(),
        );

        assert!(
            RuntimeBundlePin::for_executable(&engine_path, &runtime_build_identity())
                .unwrap()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_managed_engine_start_fails_closed_for_a_broken_public_selector() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(install_dir.join(".aethyme-managed")).unwrap();
        let engine_path = install_dir.join("aethyme-engine-cli");
        symlink(
            Path::new(".aethyme-managed/current/aethyme-engine-cli"),
            &engine_path,
        )
        .unwrap();

        let error =
            RuntimeBundlePin::for_executable(&engine_path, &runtime_build_identity()).unwrap_err();

        assert!(error.contains("resolve managed engine executable"));
    }

    #[test]
    fn inherited_pin_child_keeps_bundle_busy_after_parent_drops_its_copy() {
        const READY_ENV: &str = "AETHYME_TEST_RUNTIME_PIN_READY";
        const RELEASE_ENV: &str = "AETHYME_TEST_RUNTIME_PIN_RELEASE";
        const CHILD_TEST: &str =
            "inherited_pin_child_keeps_bundle_busy_after_parent_drops_its_copy";

        if let Some(ready_path) = std::env::var_os(READY_ENV) {
            let _pin = take_inherited_pin_file_from_stdin()
                .unwrap()
                .expect("child stdin holds inherited pin");
            let ready_path = PathBuf::from(ready_path);
            let release_path = PathBuf::from(std::env::var_os(RELEASE_ENV).unwrap());
            fs::write(&ready_path, b"ready").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !release_path.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                release_path.exists(),
                "parent did not release child pin test"
            );
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let managed_root = temp.path().join(".aethyme-managed");
        fs::create_dir(&managed_root).unwrap();
        let bundle_name = "v0.8.51-0123456789ab";
        let pin = RuntimeBundlePin::acquire_shared(&managed_root, bundle_name).unwrap();
        let ready_path = temp.path().join("child-ready");
        let release_path = temp.path().join("child-release");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--nocapture")
            .arg(CHILD_TEST)
            .stdin(Stdio::from(pin.file().try_clone().unwrap()))
            .env(RUNTIME_PIN_STDIN_ENV, "1")
            .env(RUNTIME_PIN_PATH_ENV, pin.pin_path())
            .env(READY_ENV, &ready_path)
            .env(RELEASE_ENV, &release_path)
            .spawn()
            .unwrap();
        drop(pin);

        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready_path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready_path.exists(), "child did not take the inherited pin");
        assert!(matches!(
            RuntimeBundlePin::try_acquire_exclusive(&managed_root, bundle_name).unwrap(),
            RuntimeBundlePinAttempt::Busy
        ));

        fs::write(&release_path, b"release").unwrap();
        assert!(child.wait().unwrap().success());
        assert!(matches!(
            RuntimeBundlePin::try_acquire_exclusive(&managed_root, bundle_name).unwrap(),
            RuntimeBundlePinAttempt::Acquired(_)
        ));
    }
}
