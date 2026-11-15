//! A per-user record of the wrapper probe's verdict on each Git binary, so
//! that the probe runs once per binary rather than once per process.
//!
//! Every `aethyme` process that runs Git first proves its candidate `git` is
//! not a wrapper that decorates porcelain output or rewrites exit codes
//! (#176, #178): about ten Git processes in a temporary repository. Hooks fire
//! on every agent tool use, so on a busy machine that was 25 Git spawns a
//! second and 150-250 ms on every command, spent re-proving the same files.
//!
//! The record holds acceptances and rejections, each only for exactly the file
//! that was probed under exactly the environment it was probed in:
//!
//! - **Keyed on everything that selects the bytes that run.** The candidate's
//!   absolute PATH spelling; device, inode, size, mode, mtime and ctime (to the
//!   nanosecond) of that path and of every symlink hop to the final file; the
//!   probe's own version; the user id; a digest of `PATH` (a forwarder, script
//!   or binary, resolves what it runs through it); a digest of the dynamic
//!   loader variables (`DYLD_*`, `LD_PRELOAD`, `LD_LIBRARY_PATH`); and for the
//!   macOS `/usr/bin/git` shim, `DEVELOPER_DIR`, the `xcode-select` link and
//!   the selected developer directory's own `git`. A hit needs every field to
//!   match exactly.
//! - **A cached rejection fails safe.** It skips the candidate exactly as a
//!   fresh rejection would, so the worst a stale one can do is pass over a
//!   binary that has since become honest -- and changing that binary changes
//!   its key. A probe that could not run (`Unusable`) is never recorded.
//! - **Bounded in time.** Entries expire after [`CACHE_TTL`]. What a key cannot
//!   see is a wrapper whose behaviour changes while its file and the keyed
//!   environment do not (it reads a config file or another variable), and
//!   shared libraries the binary loads. The expiry bounds how long that can go
//!   unnoticed by commands that cannot delete; anything that deletes on Git
//!   evidence re-proves a cached acceptance first (see
//!   `git::ensure_fresh_git_verdict`).
//! - **Distrusted unless it is plainly ours.** The file must be a regular file
//!   (never a symlink), owned by this user, mode 0600, in a directory owned by
//!   this user that nobody else can write, below a size and entry cap, and
//!   parse completely. Anything else is treated as absent: probe, never trust.
//!
//! `AETHYME_GIT_PROBE_CACHE=off` disables reading and writing it. An
//! environment variable can only ever force a probe here, never supply a
//! verdict.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Bump whenever `porcelain_probe` asks a different question: an acceptance
/// recorded by an older probe proves nothing about the new one.
pub(crate) const PROBE_VERSION: u32 = 1;

/// How long an acceptance is trusted without re-probing.
///
/// A day bounds the residual -- a wrapper whose behaviour changes without its
/// file changing -- to one working day for commands that cannot discard work,
/// while costing one probe (about ten Git spawns) per binary per day instead of
/// one per process: tens of thousands fewer spawns a day on a busy machine.
/// Shorter windows buy little, because every destructive command probes fresh
/// regardless; longer ones stretch the residual for no measurable saving.
pub(crate) const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// `off`, `0`, `false` or `no` disables the cache for this process.
pub(crate) const CACHE_ENV: &str = "AETHYME_GIT_PROBE_CACHE";

const CACHE_DIRECTORY: &str = "git-probe";
const CACHE_FILE: &str = "verdicts.json";
const CACHE_SCHEMA: u32 = 2;
const MAX_ENTRIES: usize = 32;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_SYMLINK_HOPS: usize = 40;
/// An acceptance dated further ahead than this is not one this host wrote
/// under a sane clock; treat the file as invalid rather than trust it longer.
const FUTURE_SKEW: Duration = Duration::from_secs(5 * 60);

/// Variables a test binary or `cargo run` always has. A process started by the
/// test harness must never read or write the developer's real cache: tests
/// that exercise the cache name their own file.
const TEST_ENVIRONMENT_KEYS: &[&str] = &[
    "CARGO_MANIFEST_DIR",
    "CARGO_BIN_EXE_aethyme",
    "AETHYME_TEST_DB_SUFFIX",
];

/// Where this process keeps accepted verdicts, if anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CacheLocation {
    Disabled,
    File(PathBuf),
}

impl CacheLocation {
    /// The per-user location: `<host cache dir>/git-probe/verdicts.json`,
    /// unless the cache is switched off or this is a test process.
    pub(crate) fn for_process() -> Self {
        let disabled_by_env = std::env::var(CACHE_ENV).is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "off" | "0" | "false" | "no"
            )
        });
        let test_process = cfg!(test)
            || TEST_ENVIRONMENT_KEYS
                .iter()
                .any(|key| std::env::var_os(key).is_some());
        if disabled_by_env || test_process || !cfg!(unix) {
            return Self::Disabled;
        }
        match crate::host_state::default_host_cache_dir() {
            Some(dir) => Self::File(dir.join(CACHE_DIRECTORY).join(CACHE_FILE)),
            None => Self::Disabled,
        }
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> Option<&Path> {
        match self {
            Self::Disabled => None,
            Self::File(path) => Some(path),
        }
    }
}

/// The metadata of one path on the way from a PATH entry to the file that runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileIdentity {
    pub path: String,
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mode: u32,
    pub mtime_sec: i64,
    pub mtime_nsec: i64,
    pub ctime_sec: i64,
    pub ctime_nsec: i64,
    /// The link text, for a symlink hop.
    pub link: Option<String>,
}

/// Everything an accepted verdict is about. Equality is the whole test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProgramKey {
    pub probe_version: u32,
    pub uid: u32,
    /// The candidate as found on PATH, absolute.
    pub candidate: String,
    /// The candidate, each symlink hop, and the final file, in order.
    pub chain: Vec<FileIdentity>,
    /// State outside the file that redirects what it runs, as name/value.
    pub selectors: Vec<(String, String)>,
}

/// What the probe concluded about one key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CachedVerdict {
    Accepted,
    /// Caught decorating output or rewriting exit codes; `reason` is the
    /// operator-facing explanation, already naming the candidate.
    Rejected {
        reason: String,
    },
}

/// Why a lookup did not produce a verdict. Each is a telemetry counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissReason {
    Disabled,
    Absent,
    Expired,
    KeyMismatch,
    Invalid,
    /// The candidate could not be described (a stat failed), so nothing can
    /// be looked up or recorded for it.
    Unkeyable,
}

impl MissReason {
    pub(crate) fn counter(self) -> &'static str {
        match self {
            Self::Disabled => "git_probe_cache_miss_disabled",
            Self::Absent => "git_probe_cache_miss_absent",
            Self::Expired => "git_probe_cache_miss_expired",
            Self::KeyMismatch => "git_probe_cache_miss_key_mismatch",
            Self::Invalid => "git_probe_cache_miss_invalid",
            Self::Unkeyable => "git_probe_cache_miss_unkeyable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lookup {
    Hit(CachedVerdict),
    Miss(MissReason),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    key: ProgramKey,
    verdict: CachedVerdict,
    recorded_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheFile {
    schema: u32,
    entries: Vec<CacheEntry>,
}

/// Describe `candidate` for the cache, or `None` when any path on its way to
/// the final file cannot be stat'ed.
pub(crate) fn program_key(candidate: &Path, path_env: Option<&OsStr>) -> Option<ProgramKey> {
    program_key_with(candidate, path_env, &SystemSelectors::current())
}

/// The process-independent inputs [`program_key`] reads, injectable for tests.
#[derive(Debug, Clone, Default)]
pub(crate) struct SystemSelectors {
    pub developer_dir: Option<String>,
    pub xcode_select_link: Option<String>,
    /// Dynamic-loader variables that change what any binary executes, as
    /// name/value, in a fixed order.
    pub loader_env: Vec<(String, String)>,
}

/// Loader variables keyed for every candidate: cheap to read, and each can
/// make an honest binary run other code.
const LOADER_ENV: &[&str] = &[
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
    "DYLD_FALLBACK_FRAMEWORK_PATH",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
];

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

impl SystemSelectors {
    fn current() -> Self {
        Self {
            developer_dir: std::env::var("DEVELOPER_DIR")
                .ok()
                .filter(|value| !value.is_empty()),
            xcode_select_link: if cfg!(target_os = "macos") {
                std::fs::read_link("/var/db/xcode_select_link")
                    .ok()
                    .map(|path| path.to_string_lossy().into_owned())
            } else {
                None
            },
            loader_env: LOADER_ENV
                .iter()
                .filter_map(|name| {
                    std::env::var_os(name)
                        .map(|value| ((*name).to_string(), value.to_string_lossy().into_owned()))
                })
                .collect(),
        }
    }
}

#[cfg(unix)]
pub(crate) fn program_key_with(
    candidate: &Path,
    path_env: Option<&OsStr>,
    system: &SystemSelectors,
) -> Option<ProgramKey> {
    if !candidate.is_absolute() {
        return None;
    }
    let chain = identity_chain(candidate)?;
    let target = chain.last()?.path.clone();
    let mut selectors = Vec::new();
    // The macOS `/usr/bin/git` is a shim that runs the developer directory's
    // git, chosen by `DEVELOPER_DIR` or `xcode-select`. Neither changes the
    // shim's file, so both -- and the git they select -- are part of the key.
    if cfg!(target_os = "macos") && target == "/usr/bin/git" {
        let developer = system
            .developer_dir
            .clone()
            .or_else(|| system.xcode_select_link.clone())
            .unwrap_or_default();
        selectors.push((
            "DEVELOPER_DIR".to_string(),
            system.developer_dir.clone().unwrap_or_default(),
        ));
        selectors.push((
            "xcode_select_link".to_string(),
            system.xcode_select_link.clone().unwrap_or_default(),
        ));
        if !developer.is_empty() {
            let selected = Path::new(&developer).join("usr/bin/git");
            match identity_chain(&selected) {
                Some(selected_chain) => {
                    for identity in selected_chain {
                        selectors.push(("developer_git".to_string(), describe(&identity)));
                    }
                }
                None => selectors.push(("developer_git".to_string(), "missing".to_string())),
            }
        }
    }
    // Any forwarder -- a script or a binary -- resolves what it runs through
    // PATH, so the same file can run a different git under a different PATH.
    // Keyed for every candidate: the cost is one probe when PATH changes.
    let path_bytes = path_env.map_or(&[][..], |value| {
        use std::os::unix::ffi::OsStrExt as _;
        value.as_bytes()
    });
    selectors.push(("PATH_sha256".to_string(), sha256_hex(path_bytes)));
    let loader = system
        .loader_env
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("\0");
    selectors.push((
        "loader_env_sha256".to_string(),
        sha256_hex(loader.as_bytes()),
    ));
    Some(ProgramKey {
        probe_version: PROBE_VERSION,
        uid: effective_uid(),
        candidate: candidate.to_string_lossy().into_owned(),
        chain,
        selectors,
    })
}

#[cfg(not(unix))]
pub(crate) fn program_key_with(
    _candidate: &Path,
    _path_env: Option<&OsStr>,
    _system: &SystemSelectors,
) -> Option<ProgramKey> {
    None
}

#[cfg(unix)]
fn describe(identity: &FileIdentity) -> String {
    format!(
        "{} {}:{} {} {:o} {}.{} {}.{} {}",
        identity.path,
        identity.dev,
        identity.ino,
        identity.size,
        identity.mode,
        identity.mtime_sec,
        identity.mtime_nsec,
        identity.ctime_sec,
        identity.ctime_nsec,
        identity.link.as_deref().unwrap_or("")
    )
}

/// `path` and every symlink hop to a non-symlink, each `lstat`ed.
#[cfg(unix)]
fn identity_chain(path: &Path) -> Option<Vec<FileIdentity>> {
    use std::os::unix::fs::MetadataExt as _;

    let mut chain = Vec::new();
    let mut current = path.to_path_buf();
    for _ in 0..=MAX_SYMLINK_HOPS {
        let metadata = std::fs::symlink_metadata(&current).ok()?;
        let link = if metadata.file_type().is_symlink() {
            Some(std::fs::read_link(&current).ok()?)
        } else {
            None
        };
        chain.push(FileIdentity {
            path: current.to_string_lossy().into_owned(),
            dev: metadata.dev(),
            ino: metadata.ino(),
            size: metadata.size(),
            mode: metadata.mode(),
            mtime_sec: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime_sec: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
            link: link
                .as_ref()
                .map(|target| target.to_string_lossy().into_owned()),
        });
        let Some(link) = link else {
            return metadata.is_file().then_some(chain);
        };
        current = if link.is_absolute() {
            link
        } else {
            current.parent()?.join(link)
        };
    }
    None
}

pub(crate) fn effective_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and dereferences no pointers.
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn unix_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
        u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
    })
}

/// The unexpired verdict `location` holds for exactly `key`, if any.
pub(crate) fn lookup(location: &CacheLocation, key: &ProgramKey, now: SystemTime) -> Lookup {
    let CacheLocation::File(path) = location else {
        return Lookup::Miss(MissReason::Disabled);
    };
    let file = match read_cache(path) {
        Ok(Some(file)) => file,
        Ok(None) => return Lookup::Miss(MissReason::Absent),
        Err(()) => return Lookup::Miss(MissReason::Invalid),
    };
    let now_ms = unix_ms(now);
    let mut same_candidate = false;
    for entry in &file.entries {
        if entry.key.candidate != key.candidate {
            continue;
        }
        same_candidate = true;
        if entry.key != *key {
            continue;
        }
        let skew = u64::try_from(FUTURE_SKEW.as_millis()).unwrap_or(u64::MAX);
        if entry.recorded_at_ms > now_ms.saturating_add(skew) {
            return Lookup::Miss(MissReason::Invalid);
        }
        let ttl = u64::try_from(CACHE_TTL.as_millis()).unwrap_or(u64::MAX);
        if now_ms.saturating_sub(entry.recorded_at_ms) >= ttl {
            return Lookup::Miss(MissReason::Expired);
        }
        return Lookup::Hit(entry.verdict.clone());
    }
    Lookup::Miss(if same_candidate {
        MissReason::KeyMismatch
    } else {
        MissReason::Absent
    })
}

/// Record the probe's verdict on exactly `key`, replacing any earlier entry for
/// the same candidate under the same keyed environment. Best effort: a cache
/// that cannot be written only costs the next process a probe.
pub(crate) fn record(
    location: &CacheLocation,
    key: &ProgramKey,
    verdict: CachedVerdict,
    now: SystemTime,
) {
    let CacheLocation::File(path) = location else {
        return;
    };
    let mut entries = match read_cache(path) {
        Ok(Some(file)) => file.entries,
        Ok(None) | Err(()) => Vec::new(),
    };
    entries.retain(|entry| {
        entry.key.candidate != key.candidate || entry.key.selectors != key.selectors
    });
    entries.push(CacheEntry {
        key: key.clone(),
        verdict,
        recorded_at_ms: unix_ms(now),
    });
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.recorded_at_ms));
    entries.truncate(MAX_ENTRIES);
    note_write(write_cache(path, entries));
}

/// Drop every verdict recorded for `candidate`. Called when a probe of it could
/// not run, so nothing older speaks for it. Writes only when there was
/// something to drop.
pub(crate) fn forget(location: &CacheLocation, candidate: &Path) {
    let CacheLocation::File(path) = location else {
        return;
    };
    let Ok(Some(file)) = read_cache(path) else {
        return;
    };
    let candidate = candidate.to_string_lossy();
    let before = file.entries.len();
    let entries: Vec<CacheEntry> = file
        .entries
        .into_iter()
        .filter(|entry| entry.key.candidate != candidate)
        .collect();
    if entries.len() != before {
        note_write(write_cache(path, entries));
    }
}

/// Writes are best effort -- a cache that cannot be written only costs the
/// next process a probe -- but a failure is counted, not dropped.
fn note_write(result: std::io::Result<()>) {
    if result.is_err() {
        crate::phase_telemetry::add("git_probe_cache_write_failed", Duration::ZERO);
    }
}

/// `Ok(None)` when there is no file; `Err` when there is one we must not
/// believe.
#[cfg(unix)]
fn read_cache(path: &Path) -> Result<Option<CacheFile>, ()> {
    read_cache_owned_by(path, effective_uid())
}

/// [`read_cache`] for an expected owner, so a test can prove an owner
/// mismatch is refused without being root.
#[cfg(unix)]
fn read_cache_owned_by(path: &Path, uid: u32) -> Result<Option<CacheFile>, ()> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let Some(parent) = path.parent() else {
        return Err(());
    };
    match std::fs::symlink_metadata(parent) {
        Ok(metadata)
            if metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o022 == 0 => {}
        Ok(_) => return Err(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    }
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let metadata = file.metadata().map_err(|_| ())?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() > MAX_FILE_BYTES
    {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(());
    }
    let parsed: CacheFile = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if parsed.schema != CACHE_SCHEMA || parsed.entries.len() > MAX_ENTRIES {
        return Err(());
    }
    Ok(Some(parsed))
}

#[cfg(not(unix))]
fn read_cache(_path: &Path) -> Result<Option<CacheFile>, ()> {
    Ok(None)
}

#[cfg(unix)]
fn write_cache(path: &Path, entries: Vec<CacheEntry>) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache path has no parent"))?;
    if !parent.exists() {
        std::fs::create_dir_all(parent)?;
        crate::host_state::protect_host_state_path(parent, true)?;
    }
    let metadata = std::fs::symlink_metadata(parent)?;
    if !metadata.is_dir() || metadata.uid() != effective_uid() || metadata.mode() & 0o022 != 0 {
        return Err(std::io::Error::other(
            "git probe cache directory is not private to this user",
        ));
    }
    let bytes = serde_json::to_vec(&CacheFile {
        schema: CACHE_SCHEMA,
        entries,
    })
    .map_err(std::io::Error::other)?;
    // A private temporary in the same directory, renamed over the target:
    // readers see the old file or the new one, never a torn write.
    crate::atomic_file::with_synced_temporary(path, &bytes, |temporary| {
        std::fs::rename(temporary, path)
    })
}

#[cfg(not(unix))]
fn write_cache(_path: &Path, _entries: Vec<CacheEntry>) -> std::io::Result<()> {
    Ok(())
}

/// Prefix of every probe directory, so leftovers can be recognised.
pub(crate) const PROBE_DIRECTORY_PREFIX: &str = "aethyme-git-probe-";

/// A probe directory older than this cannot belong to a running probe: the
/// probe takes well under a second, and Git commands it runs time out long
/// before an hour.
pub(crate) const STALE_PROBE_DIRECTORY_AGE: Duration = Duration::from_secs(60 * 60);

/// At most this many leftovers are removed per sweep.
pub(crate) const MAX_PROBE_DIRECTORIES_PER_SWEEP: usize = 200;

/// At most this many temporary-directory entries are examined per sweep.
const MAX_TEMP_ENTRIES_EXAMINED: usize = 50_000;

/// What one sweep of the temporary directory did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProbeDirectorySweep {
    /// `aethyme-git-probe-*` directories removed.
    pub removed: usize,
    /// Legacy `.tmp*` probe directories removed (recognised by exact shape).
    pub legacy_removed: usize,
    /// Matching directories whose removal failed.
    pub failed: usize,
    /// Whether the sweep stopped at a bound with entries left unexamined.
    pub truncated: bool,
}

/// Remove probe directories that a killed process left in the system
/// temporary directory. Bounded, best effort, and never touches a directory
/// whose shape is not exactly a probe's. Only this process's temporary
/// directory (`$TMPDIR`) is swept: probes run under another `TMPDIR` leave
/// their leftovers until a worker with that `TMPDIR` runs.
pub(crate) fn sweep_stale_probe_directories() -> ProbeDirectorySweep {
    let started = std::time::Instant::now();
    let sweep = sweep_probe_directories_in(
        &std::env::temp_dir(),
        SystemTime::now(),
        STALE_PROBE_DIRECTORY_AGE,
        MAX_PROBE_DIRECTORIES_PER_SWEEP,
    );
    crate::phase_telemetry::add("git_probe_dirs_sweep", started.elapsed());
    for _ in 0..sweep.removed + sweep.legacy_removed {
        crate::phase_telemetry::add("git_probe_dirs_removed", Duration::ZERO);
    }
    sweep
}

#[cfg(unix)]
pub(crate) fn sweep_probe_directories_in(
    temp: &Path,
    now: SystemTime,
    min_age: Duration,
    max_removals: usize,
) -> ProbeDirectorySweep {
    let mut sweep = ProbeDirectorySweep::default();
    let Ok(entries) = std::fs::read_dir(temp) else {
        return sweep;
    };
    for (examined, entry) in entries.flatten().enumerate() {
        if examined >= MAX_TEMP_ENTRIES_EXAMINED
            || sweep.removed + sweep.legacy_removed + sweep.failed >= max_removals
        {
            sweep.truncated = true;
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let legacy = if name.starts_with(PROBE_DIRECTORY_PREFIX) {
            false
        } else if name.starts_with(".tmp") {
            true
        } else {
            continue;
        };
        let path = entry.path();
        if !is_stale_probe_directory(&path, legacy, now, min_age) {
            continue;
        }
        // The shared pinned, no-follow tree removal: paced by the unattended
        // pacer the worker installs, and refused unless Git is freshly proven.
        match crate::gc::remove_condemned_tree(&path, None, None) {
            Ok(crate::gc::TreeRemoval::Complete) if legacy => sweep.legacy_removed += 1,
            Ok(crate::gc::TreeRemoval::Complete) => sweep.removed += 1,
            Ok(_) | Err(_) => sweep.failed += 1,
        }
    }
    sweep
}

#[cfg(not(unix))]
pub(crate) fn sweep_probe_directories_in(
    _temp: &Path,
    _now: SystemTime,
    _min_age: Duration,
    _max_removals: usize,
) -> ProbeDirectorySweep {
    ProbeDirectorySweep::default()
}

/// A real directory owned by this user, untouched for `min_age`, holding
/// nothing but what `porcelain_probe` creates. A prefixed directory may hold
/// a subset (a probe killed while preparing); a legacy `.tmp*` directory has
/// no distinctive name, so it must hold exactly the three entries, each of
/// exactly the kind the probe makes, and `repo` nothing but `.git` and the
/// probe file.
#[cfg(unix)]
fn is_stale_probe_directory(path: &Path, legacy: bool, now: SystemTime, min_age: Duration) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    let uid = effective_uid();
    let owned_dir = |path: &Path| {
        std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_dir() && metadata.uid() == uid)
    };
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_dir() || metadata.uid() != uid {
        return false;
    }
    let old_enough = |modified: std::io::Result<SystemTime>| {
        modified
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= min_age)
    };
    if !old_enough(metadata.modified()) {
        return false;
    }
    let Some(children) = child_names(path, 4) else {
        return false;
    };
    const SHAPE: [&str; 3] = ["empty-gitconfig", "empty-template", "repo"];
    if legacy {
        if children != SHAPE {
            return false;
        }
    } else if !children.iter().all(|name| SHAPE.contains(&name.as_str())) {
        return false;
    }
    for name in &children {
        let child = path.join(name);
        let Ok(child_metadata) = std::fs::symlink_metadata(&child) else {
            return false;
        };
        if child_metadata.uid() != uid || !old_enough(child_metadata.modified()) {
            return false;
        }
        let shaped = match name.as_str() {
            "empty-gitconfig" => child_metadata.is_file() && child_metadata.len() == 0,
            "empty-template" => {
                owned_dir(&child) && child_names(&child, 1).is_some_and(|c| c.is_empty())
            }
            "repo" => {
                owned_dir(&child)
                    && child_names(&child, 2).is_some_and(|inner| {
                        inner
                            .iter()
                            .all(|entry| entry == ".git" || entry == "probe.txt")
                    })
                    && is_probe_git_directory(&child.join(".git"), uid)
            }
            _ => false,
        };
        if !shaped {
            return false;
        }
    }
    true
}

/// What `git init --template=<empty>` and the probe's two commits leave in
/// `.git`. Absent is fine (a probe killed before `init`); anything else --
/// hooks, a worktree link, a symlink -- is not a probe's repository.
#[cfg(unix)]
fn is_probe_git_directory(git: &Path, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    const PROBE_GIT_ENTRIES: [&str; 8] = [
        "COMMIT_EDITMSG",
        "HEAD",
        "config",
        "index",
        "logs",
        "objects",
        "refs",
        "description",
    ];
    let metadata = match std::fs::symlink_metadata(git) {
        Ok(metadata) => metadata,
        Err(error) => return error.kind() == std::io::ErrorKind::NotFound,
    };
    if !metadata.is_dir() || metadata.uid() != uid {
        return false;
    }
    let Some(entries) = child_names(git, PROBE_GIT_ENTRIES.len()) else {
        return false;
    };
    entries.iter().all(|name| {
        PROBE_GIT_ENTRIES.contains(&name.as_str())
            && std::fs::symlink_metadata(git.join(name))
                .is_ok_and(|entry| !entry.file_type().is_symlink() && entry.uid() == uid)
    })
}

/// Sorted names of `dir`'s entries, or `None` when it cannot be read or holds
/// more than `limit`.
#[cfg(unix)]
fn child_names(dir: &Path, limit: usize) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).ok()? {
        let name = entry.ok()?.file_name().into_string().ok()?;
        names.push(name);
        if names.len() > limit {
            return None;
        }
    }
    names.sort();
    Some(names)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn executable(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn key_for(path: &Path) -> ProgramKey {
        program_key_with(
            path,
            Some(OsStr::new("/usr/bin:/bin")),
            &SystemSelectors::default(),
        )
        .expect("key")
    }

    fn location(dir: &Path) -> CacheLocation {
        CacheLocation::File(dir.join("git-probe").join("verdicts.json"))
    }

    #[test]
    fn a_recorded_acceptance_hits_for_exactly_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("git");
        executable(&bin, "binary-ish");
        let cache = location(dir.path());
        let key = key_for(&bin);
        let now = SystemTime::now();
        assert_eq!(lookup(&cache, &key, now), Lookup::Miss(MissReason::Absent));
        record(&cache, &key, CachedVerdict::Accepted, now);
        assert_eq!(
            lookup(&cache, &key, now),
            Lookup::Hit(CachedVerdict::Accepted)
        );
        let mode = std::fs::metadata(cache.path().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the cache file is private");
    }

    #[test]
    fn replacing_the_binary_changes_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("git");
        executable(&bin, "one");
        let cache = location(dir.path());
        let now = SystemTime::now();
        record(&cache, &key_for(&bin), CachedVerdict::Accepted, now);
        // A new file at the same path: new inode (and size), so new key.
        let staged = dir.path().join("git.new");
        executable(&staged, "two two");
        std::fs::rename(&staged, &bin).unwrap();
        assert_eq!(
            lookup(&cache, &key_for(&bin), now),
            Lookup::Miss(MissReason::KeyMismatch)
        );
    }

    #[test]
    fn an_in_place_rewrite_of_the_same_size_changes_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("git");
        executable(&bin, "aaaa");
        let before = key_for(&bin);
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&bin, "bbbb").unwrap();
        assert_ne!(
            before,
            key_for(&bin),
            "mtime/ctime must catch same-size rewrites"
        );
    }

    #[test]
    fn retargeting_a_symlink_hop_changes_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one");
        let two = dir.path().join("two");
        executable(&one, "same");
        executable(&two, "same");
        let link = dir.path().join("git");
        std::os::unix::fs::symlink(&one, &link).unwrap();
        let before = key_for(&link);
        assert_eq!(
            before.chain.len(),
            2,
            "the hop and its target are both keyed"
        );
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&two, &link).unwrap();
        assert_ne!(before, key_for(&link));
    }

    #[test]
    fn a_different_path_winner_is_a_different_entry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        std::fs::create_dir(dir.path().join("b")).unwrap();
        let first = dir.path().join("a/git");
        let second = dir.path().join("b/git");
        executable(&first, "x");
        executable(&second, "x");
        let cache = location(dir.path());
        let now = SystemTime::now();
        record(&cache, &key_for(&first), CachedVerdict::Accepted, now);
        assert_eq!(
            lookup(&cache, &key_for(&second), now),
            Lookup::Miss(MissReason::Absent),
            "another binary never borrows a verdict"
        );
    }

    #[test]
    fn every_candidate_is_keyed_on_path_and_the_loader_environment() {
        let dir = tempfile::tempdir().unwrap();
        let system = SystemSelectors::default();
        for (name, body) in [
            ("script-git", "#!/bin/sh\nexec git \"$@\"\n"),
            ("bin-git", "\x7fELF"),
        ] {
            let path = dir.path().join(name);
            executable(&path, body);
            let a = program_key_with(&path, Some(OsStr::new("/a:/b")), &system).unwrap();
            let b = program_key_with(&path, Some(OsStr::new("/b:/a")), &system).unwrap();
            assert_ne!(a, b, "{name}: a forwarder resolves through PATH");
            let preload = SystemSelectors {
                loader_env: vec![("LD_PRELOAD".into(), "/tmp/evil.so".into())],
                ..SystemSelectors::default()
            };
            let c = program_key_with(&path, Some(OsStr::new("/a:/b")), &preload).unwrap();
            assert_ne!(a, c, "{name}: a preloaded library changes what runs");
        }
    }

    #[test]
    fn uid_and_probe_version_are_part_of_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("git");
        executable(&bin, "x");
        let cache = location(dir.path());
        let now = SystemTime::now();
        let key = key_for(&bin);
        assert_eq!(key.uid, effective_uid());
        assert_eq!(key.probe_version, PROBE_VERSION);
        for other in [
            ProgramKey {
                uid: key.uid.wrapping_add(1),
                ..key.clone()
            },
            ProgramKey {
                probe_version: PROBE_VERSION + 1,
                ..key.clone()
            },
        ] {
            std::fs::remove_dir_all(dir.path().join("git-probe")).ok();
            record(&cache, &other, CachedVerdict::Accepted, now);
            assert_eq!(
                lookup(&cache, &key, now),
                Lookup::Miss(MissReason::KeyMismatch),
                "an entry for another user or an older probe proves nothing"
            );
        }
    }

    #[test]
    fn a_rejection_is_recorded_and_returned() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("git");
        executable(&bin, "x");
        let cache = location(dir.path());
        let now = SystemTime::now();
        let rejected = CachedVerdict::Rejected {
            reason: "rewrites exit codes".into(),
        };
        record(&cache, &key_for(&bin), rejected.clone(), now);
        assert_eq!(lookup(&cache, &key_for(&bin), now), Lookup::Hit(rejected));
        record(&cache, &key_for(&bin), CachedVerdict::Accepted, now);
        assert_eq!(
            lookup(&cache, &key_for(&bin), now),
            Lookup::Hit(CachedVerdict::Accepted),
            "a later probe replaces the entry either way"
        );
    }

    #[test]
    fn size_entry_and_owner_caps_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let cache = location(dir.path());
        let now = SystemTime::now();
        // More candidates than the cap: the oldest are dropped, never an
        // oversized file written.
        for index in 0..(MAX_ENTRIES + 5) {
            let bin = dir.path().join(format!("git-{index}"));
            executable(&bin, "x");
            record(
                &cache,
                &key_for(&bin),
                CachedVerdict::Accepted,
                now + Duration::from_millis(index as u64),
            );
        }
        let path = cache.path().unwrap().to_path_buf();
        let file = read_cache(&path).unwrap().unwrap();
        assert_eq!(file.entries.len(), MAX_ENTRIES);
        // An owner mismatch is refused.
        assert!(read_cache_owned_by(&path, effective_uid().wrapping_add(1)).is_err());
        // A hand-written file over the entry cap is refused.
        let mut over = file.clone();
        over.entries.push(over.entries[0].clone());
        std::fs::write(&path, serde_json::to_vec(&over).unwrap()).unwrap();
        assert!(read_cache(&path).is_err());
        // A file over the size cap is refused before parsing.
        std::fs::write(&path, vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(read_cache(&path).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_xcode_shim_is_keyed_on_its_selectors() {
        let shim = Path::new("/usr/bin/git");
        let Some(canonical) = identity_chain(shim) else {
            return;
        };
        if canonical.last().map(|identity| identity.path.as_str()) != Some("/usr/bin/git") {
            return;
        }
        let path = Some(OsStr::new("/usr/bin"));
        let base = SystemSelectors::default();
        let developer = SystemSelectors {
            developer_dir: Some("/Library/Developer/CommandLineTools".into()),
            ..SystemSelectors::default()
        };
        let xcode = SystemSelectors {
            xcode_select_link: Some("/Applications/Xcode.app/Contents/Developer".into()),
            ..SystemSelectors::default()
        };
        let keys =
            [&base, &developer, &xcode].map(|system| program_key_with(shim, path, system).unwrap());
        assert_ne!(keys[0], keys[1]);
        assert_ne!(keys[0], keys[2]);
        assert_ne!(keys[1], keys[2]);
    }

    #[test]
    fn expiry_and_future_dates_force_a_probe() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("git");
        executable(&bin, "x");
        let cache = location(dir.path());
        let key = key_for(&bin);
        let now = SystemTime::now();
        record(&cache, &key, CachedVerdict::Accepted, now);
        assert_eq!(
            lookup(&cache, &key, now + CACHE_TTL),
            Lookup::Miss(MissReason::Expired)
        );
        assert_eq!(
            lookup(&cache, &key, now - Duration::from_secs(3600)),
            Lookup::Miss(MissReason::Invalid)
        );
    }

    #[test]
    fn a_cache_file_with_the_wrong_mode_or_content_is_never_believed() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("git");
        executable(&bin, "x");
        let cache = location(dir.path());
        let key = key_for(&bin);
        let now = SystemTime::now();
        record(&cache, &key, CachedVerdict::Accepted, now);
        let path = cache.path().unwrap().to_path_buf();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(lookup(&cache, &key, now), Lookup::Miss(MissReason::Invalid));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            lookup(&cache, &key, now),
            Lookup::Hit(CachedVerdict::Accepted)
        );

        // A group-writable directory could have had the file swapped.
        let parent = path.parent().unwrap();
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert_eq!(lookup(&cache, &key, now), Lookup::Miss(MissReason::Invalid));
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();

        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(lookup(&cache, &key, now), Lookup::Miss(MissReason::Invalid));

        // A symlink to an otherwise valid file is not the file.
        record(&cache, &key, CachedVerdict::Accepted, now);
        let real = parent.join("elsewhere.json");
        std::fs::rename(&path, &real).unwrap();
        std::os::unix::fs::symlink(&real, &path).unwrap();
        assert_eq!(lookup(&cache, &key, now), Lookup::Miss(MissReason::Invalid));
    }

    #[test]
    fn forgetting_a_candidate_drops_only_its_entry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        let first = dir.path().join("git");
        let second = dir.path().join("a/git");
        executable(&first, "x");
        executable(&second, "y");
        let cache = location(dir.path());
        let now = SystemTime::now();
        record(&cache, &key_for(&first), CachedVerdict::Accepted, now);
        record(&cache, &key_for(&second), CachedVerdict::Accepted, now);
        forget(&cache, &first);
        assert_eq!(
            lookup(&cache, &key_for(&first), now),
            Lookup::Miss(MissReason::Absent)
        );
        assert_eq!(
            lookup(&cache, &key_for(&second), now),
            Lookup::Hit(CachedVerdict::Accepted)
        );
    }

    #[test]
    fn the_test_process_never_uses_the_real_cache() {
        assert_eq!(CacheLocation::for_process(), CacheLocation::Disabled);
    }

    fn probe_shape(root: &Path, with_repo_contents: bool) {
        std::fs::create_dir(root).unwrap();
        std::fs::create_dir(root.join("empty-template")).unwrap();
        std::fs::write(root.join("empty-gitconfig"), b"").unwrap();
        std::fs::create_dir(root.join("repo")).unwrap();
        if with_repo_contents {
            std::fs::create_dir(root.join("repo/.git")).unwrap();
            std::fs::write(root.join("repo/probe.txt"), b"after\n").unwrap();
        }
    }

    #[test]
    fn the_sweep_removes_only_stale_directories_of_the_probe_shape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        probe_shape(&root.join("aethyme-git-probe-abc123"), true);
        probe_shape(&root.join("aethyme-git-probe-partial"), false);
        std::fs::remove_dir(root.join("aethyme-git-probe-partial/repo")).unwrap();
        probe_shape(&root.join(".tmpLEGACY"), true);
        probe_shape(&root.join(".tmpEMPTYREPO"), false);
        // Same prefix, one extra entry: not a probe directory.
        probe_shape(&root.join(".tmpEXTRA"), true);
        std::fs::write(root.join(".tmpEXTRA/notes.txt"), b"keep me").unwrap();
        // Same names, wrong kind: a non-empty template.
        probe_shape(&root.join(".tmpWRONGKIND"), true);
        std::fs::write(root.join(".tmpWRONGKIND/empty-template/hook"), b"").unwrap();
        // Right names, but its repository has hooks: not a probe's `.git`.
        probe_shape(&root.join(".tmpHOOKS"), true);
        std::fs::create_dir(root.join(".tmpHOOKS/repo/.git/hooks")).unwrap();
        // Unrelated `.tmp*` directory and file.
        std::fs::create_dir(root.join(".tmpOTHER")).unwrap();
        std::fs::write(root.join(".tmpOTHER/data"), b"x").unwrap();
        std::fs::write(root.join(".tmpFILE"), b"x").unwrap();
        // A legacy directory missing one of the three entries.
        probe_shape(&root.join(".tmpMISSING"), true);
        std::fs::remove_file(root.join(".tmpMISSING/empty-gitconfig")).unwrap();
        // A symlink named like a probe directory, pointing at a probe shape.
        probe_shape(&root.join("target-shape"), true);
        std::os::unix::fs::symlink(
            root.join("target-shape"),
            root.join("aethyme-git-probe-link"),
        )
        .unwrap();

        // Young: nothing goes. Directory times cannot be set without another
        // crate, so the sweep takes `now` and the test moves the clock.
        let fresh =
            sweep_probe_directories_in(root, SystemTime::now(), STALE_PROBE_DIRECTORY_AGE, 100);
        assert_eq!(fresh, ProbeDirectorySweep::default());

        let later = SystemTime::now() + Duration::from_secs(2 * 3600);
        let sweep = sweep_probe_directories_in(root, later, STALE_PROBE_DIRECTORY_AGE, 100);
        assert_eq!(sweep.removed, 2, "{sweep:?}");
        assert_eq!(sweep.legacy_removed, 2, "{sweep:?}");
        assert_eq!(sweep.failed, 0);
        for gone in [
            "aethyme-git-probe-abc123",
            "aethyme-git-probe-partial",
            ".tmpLEGACY",
            ".tmpEMPTYREPO",
        ] {
            assert!(!root.join(gone).exists(), "{gone} should be removed");
        }
        for kept in [
            ".tmpEXTRA/notes.txt",
            ".tmpWRONGKIND/empty-template/hook",
            ".tmpOTHER/data",
            ".tmpFILE",
            ".tmpMISSING/repo",
            ".tmpHOOKS/repo/.git/hooks",
            "target-shape/repo/probe.txt",
        ] {
            assert!(root.join(kept).exists(), "{kept} must never be touched");
        }
        assert!(
            std::fs::symlink_metadata(root.join("aethyme-git-probe-link")).is_ok(),
            "a symlink is never followed or removed"
        );
    }

    #[test]
    fn the_sweep_is_bounded_per_run() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..5 {
            probe_shape(
                &temp.path().join(format!("aethyme-git-probe-{index}")),
                true,
            );
        }
        let later = SystemTime::now() + Duration::from_secs(2 * 3600);
        let sweep = sweep_probe_directories_in(temp.path(), later, STALE_PROBE_DIRECTORY_AGE, 3);
        assert_eq!(sweep.removed, 3);
        assert!(sweep.truncated);
    }
}
