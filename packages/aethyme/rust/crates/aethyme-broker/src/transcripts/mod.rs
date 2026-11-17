//! `aethyme transcripts`: opt-in archiving of closed agent transcripts.
//!
//! Agent transcripts grow without bound (tens of GiB on a busy host) and are
//! almost never read once their session is over. When the owner turns this on
//! (`[transcripts] archive = true` in `<host state>/transcripts/settings.toml`),
//! a transcript that is older than `archive_after_days`, open in no process,
//! and whose linked broker sessions are all closed with their checkouts gone,
//! is compressed with zstd and moved: Codex transcripts into Codex's own
//! `archived_sessions/` as `.jsonl.zst` (which Codex unarchives and resumes
//! natively), Claude Code transcripts under Aethyme's host state (restored
//! with `aethyme transcripts restore`). Nothing runs, and no directory is
//! listed, while the setting is off.
//!
//! The lane runs from the existing detached background worker at low
//! priority and a bounded byte rate, or on demand with
//! `aethyme transcripts archive --apply`. It never touches broker databases
//! except to read them, and keeps its own small index beside its settings.

mod agents;
mod archive;
pub mod cli;
mod config;
mod eligibility;
mod index;
mod linkage;
mod open_files;
mod pacing;
mod providers;
mod run;
mod safe_fs;
#[cfg(test)]
mod tests;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use archive::Restored;
pub use config::{SETTINGS_FILE, TranscriptsConfig};
pub use eligibility::SkipReason;
pub use index::ArchiveRow;
pub(crate) use linkage::note_holder_bound;
pub use providers::Provider;
pub use run::{RunReport, StatusReport};
pub(crate) use run::{linkage_degraded_advice, run_worker_lane};

/// Directory under the host state directory that holds everything here.
pub const HOST_DIRECTORY: &str = "transcripts";

/// Where this host keeps its agent homes and Aethyme state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Environment {
    /// `<host state>/transcripts`.
    pub host_dir: PathBuf,
    pub home: Option<PathBuf>,
    /// `$CODEX_HOME`.
    pub codex_home: Option<PathBuf>,
    /// `$CLAUDE_CONFIG_DIR`.
    pub claude_home: Option<PathBuf>,
    /// Whether the host's worktree registry is consulted for repositories.
    pub use_registry: bool,
    /// Free bytes to assume on every archive volume (tests); `None` reads
    /// the volume.
    pub free_space_override: Option<u64>,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

impl Environment {
    pub fn from_process() -> Option<Self> {
        let host_dir = crate::host_state::default_host_state_dir()?.join(HOST_DIRECTORY);
        Some(Self {
            host_dir,
            home: env_path("HOME"),
            codex_home: env_path("CODEX_HOME"),
            claude_home: env_path("CLAUDE_CONFIG_DIR"),
            use_registry: true,
            free_space_override: None,
        })
    }

    /// The directories the enabled providers resolve to.
    pub fn layout(&self, config: &TranscriptsConfig) -> providers::Layout {
        let resolve = |provider: Provider,
                       configured: &Option<PathBuf>,
                       from_env: &Option<PathBuf>,
                       default: &str| {
            config
                .providers
                .contains(&provider)
                .then(|| {
                    configured
                        .clone()
                        .or_else(|| from_env.clone())
                        .or_else(|| self.home.as_ref().map(|home| home.join(default)))
                })
                .flatten()
        };
        providers::Layout {
            codex_home: resolve(
                Provider::Codex,
                &config.codex_home,
                &self.codex_home,
                ".codex",
            ),
            claude_home: resolve(
                Provider::Claude,
                &config.claude_home,
                &self.claude_home,
                ".claude",
            ),
            host_dir: self.host_dir.clone(),
        }
    }
}

static HOST_SETTINGS: std::sync::OnceLock<Option<(Environment, TranscriptsConfig)>> =
    std::sync::OnceLock::new();

/// This host's environment and transcript settings, read once per process.
/// `None` when the host state cannot be resolved or the settings cannot be
/// read (then nothing runs).
pub(crate) fn host_settings() -> Option<&'static (Environment, TranscriptsConfig)> {
    HOST_SETTINGS
        .get_or_init(|| {
            let environment = Environment::from_process()?;
            let config = TranscriptsConfig::load(&environment.host_dir).ok()?;
            Some((environment, config))
        })
        .as_ref()
}

/// Create `directory` (and parents) and restrict it to the owner.
pub(crate) fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    crate::host_state::protect_host_state_path(directory, true)
}

/// Discard the outcome of best-effort cleanup: removing a temporary file,
/// syncing a directory, dropping a stale row a later pass reconciles.
pub(crate) fn best_effort<T, E>(_outcome: Result<T, E>) {}

/// fsync a directory, best effort.
pub(crate) fn sync_directory(directory: &Path) {
    if let Ok(handle) = fs::File::open(directory) {
        best_effort(handle.sync_all());
    }
}

/// Create `path` if absent with mode 0600 (never widening an existing
/// file's mode, always narrowing it to 0600).
pub(crate) fn ensure_private_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    crate::host_state::protect_host_state_path(path, false)
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The lane's single-flight lock: an advisory lock on
/// `<host state>/transcripts/archive.lock`, which the kernel releases when
/// its holder exits however it exits, so a crashed run never wedges the
/// next. The holder's pid and start time are written into the file for
/// diagnosis.
pub(crate) struct LaneLock {
    _lock: crate::file_lock::ExclusiveFileLock,
}

pub(crate) const LOCK_FILE: &str = "archive.lock";

impl LaneLock {
    /// `None` when another process holds it.
    pub fn try_acquire(host_dir: &Path) -> io::Result<Option<Self>> {
        ensure_private_directory(host_dir)?;
        let path = host_dir.join(LOCK_FILE);
        let file = crate::file_lock::open_lock_file(&path)?;
        crate::host_state::protect_host_state_path(&path, false)?;
        let Some(lock) = crate::file_lock::ExclusiveFileLock::try_acquire(file)? else {
            return Ok(None);
        };
        lock.record_holder(&format!(
            "pid={} started_ms={}\n",
            std::process::id(),
            now_ms()
        ))?;
        Ok(Some(Self { _lock: lock }))
    }

    /// The holder line the current or last holder wrote.
    pub fn recorded_holder(host_dir: &Path) -> Option<String> {
        fs::read_to_string(host_dir.join(LOCK_FILE))
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
    }
}
