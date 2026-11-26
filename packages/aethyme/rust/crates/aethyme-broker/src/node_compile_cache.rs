//! One host-wide Node.js compile cache for the commands the broker runs.
//!
//! Node.js (since 22.1, stable in 24.15) keeps V8 code cache on disk for
//! every CommonJS, ES and TypeScript module it compiles when
//! `NODE_COMPILE_CACHE` names a directory; a later run that loads the same
//! modules from the same paths deserializes them instead of compiling them.
//! Agents run the same hooks, gates and install scripts thousands of times a
//! day, and each run compiled them from scratch. So every gate, preparation
//! step and guarded exec command the broker runs for a session gets
//! `NODE_COMPILE_CACHE` pointing at one directory under the host cache
//! directory (never per session, so runs share it):
//!
//! `<host cache>/node-compile-cache/d<days since the epoch>`
//!
//! What Node.js documents, and what this relies on: entries from different
//! Node.js versions are stored apart in one base directory, an entry is
//! invalidated when the module's absolute path changes, a cache that cannot
//! be used is skipped without failing the process, entries are written when
//! the process exits, and removing the directory is the documented cleanup
//! (it is recreated on next use). Node.js never prunes it. The cache is keyed
//! by absolute path, so it pays for repeated runs in one checkout (a gate
//! run again, a hook on every commit) and for tools at a fixed path; two
//! session worktrees hold separate entries.
//!
//! Node.js never removes stale entries, so the directory gets a new
//! subdirectory each day and the storage ledger classifies
//! `node-compile-cache` as a tool cache whose daily children go once their
//! UTC day is over and they have not changed for an hour, whatever the
//! tool-cache budget ([`past_day`]); today's directory is never removed.
//! Nothing records which process uses a directory, but no command is pointed
//! at a past day's: only a process started before midnight still writes
//! there, when it exits. Deleting one is always safe; the next run
//! recompiles (Node.js recreates a missing directory).
//!
//! The variable is set only when the caller's environment does not already
//! name one (a declared step environment still wins, being applied later),
//! and a repository turns it off in `.aethyme/config.toml`:
//!
//! ```toml
//! [node]
//! compile_cache = false
//! ```

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The variable Node.js reads.
pub(crate) const NODE_COMPILE_CACHE_ENV: &str = "NODE_COMPILE_CACHE";
/// The cache's entry in the host cache directory.
pub(crate) const CACHE_DIRECTORY: &str = "node-compile-cache";
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// Whether the repository of each session resolved in this process wants
/// the cache, for commands spawned where only the session id is at hand.
static ENABLED_FOR_SESSIONS: Mutex<BTreeMap<i64, bool>> = Mutex::new(BTreeMap::new());
/// Each repository's resolved policy, read once per process.
static POLICY_BY_ROOT: Mutex<BTreeMap<PathBuf, bool>> = Mutex::new(BTreeMap::new());

/// `[node]` in `.aethyme/config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct NodePolicy {
    /// Give commands the broker runs a shared `NODE_COMPILE_CACHE`
    /// (default true).
    pub(crate) compile_cache: bool,
}

impl Default for NodePolicy {
    fn default() -> Self {
        Self {
            compile_cache: true,
        }
    }
}

impl NodePolicy {
    fn from_config_text(text: &str) -> Result<Self, String> {
        let value = text
            .parse::<toml::Value>()
            .map_err(|error| format!(".aethyme/config.toml is invalid: {error}"))?;
        let Some(section) = value.get("node") else {
            return Ok(Self::default());
        };
        section
            .clone()
            .try_into()
            .map_err(|error| format!("[node] is invalid: {error}"))
    }
}

/// Whether `main_root`'s repository wants the cache, read once per process
/// under the usual configuration trust rule. An unreadable or invalid file
/// keeps the default (counted).
fn enabled_for_repository(main_root: &Path) -> bool {
    let mut cached = POLICY_BY_ROOT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(enabled) = cached.get(main_root) {
        return *enabled;
    }
    let _span = crate::phase_telemetry::span("node_compile_cache_policy");
    let loaded = crate::merge::repository_config_checked(main_root)
        .map_err(|error| error.to_string())
        .and_then(|config| match config {
            Some((text, _)) => NodePolicy::from_config_text(&text),
            None => Ok(NodePolicy::default()),
        });
    let enabled = loaded
        .unwrap_or_else(|_| {
            crate::phase_telemetry::add("node_compile_cache_policy_invalid", Duration::ZERO);
            NodePolicy::default()
        })
        .compile_cache;
    cached.insert(main_root.to_path_buf(), enabled);
    enabled
}

/// Resolve `main_root`'s policy and remember it for the commands this
/// process spawns for `session_id`.
pub(crate) fn remember_for_session(session_id: i64, main_root: &Path) {
    let enabled = enabled_for_repository(main_root);
    ENABLED_FOR_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session_id, enabled);
}

/// The variable to set on a command run for `session_id`, or `None` when
/// the repository opted out, its policy was never resolved in this process,
/// the caller already set one, or the directory is unavailable (each
/// counted).
pub(crate) fn environment_for_session(session_id: i64) -> Option<(String, String)> {
    let enabled = ENABLED_FOR_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&session_id)
        .copied();
    match enabled {
        Some(true) => {}
        Some(false) => {
            crate::phase_telemetry::add("node_compile_cache_opted_out", Duration::ZERO);
            return None;
        }
        None => {
            crate::phase_telemetry::add("node_compile_cache_unresolved", Duration::ZERO);
            return None;
        }
    }
    environment_with(
        std::env::var_os(NODE_COMPILE_CACHE_ENV),
        crate::host_state::default_host_cache_dir(),
        SystemTime::now(),
    )
}

fn environment_with(
    inherited: Option<OsString>,
    host_cache: Option<PathBuf>,
    now: SystemTime,
) -> Option<(String, String)> {
    if inherited.is_some_and(|value| !value.is_empty()) {
        crate::phase_telemetry::add("node_compile_cache_caller_set", Duration::ZERO);
        return None;
    }
    let Some(host_cache) = host_cache else {
        crate::phase_telemetry::add("node_compile_cache_no_host_cache", Duration::ZERO);
        return None;
    };
    let directory = cache_directory(&host_cache, now);
    let created = {
        let _span = crate::phase_telemetry::span("node_compile_cache_prepare");
        create_private_directory(&directory)
    };
    if created.is_err() {
        crate::phase_telemetry::add("node_compile_cache_create_failed", Duration::ZERO);
        return None;
    }
    crate::phase_telemetry::add("node_compile_cache_set", Duration::ZERO);
    Some((
        NODE_COMPILE_CACHE_ENV.to_owned(),
        directory.to_string_lossy().into_owned(),
    ))
}

/// Today's subdirectory: a new one each UTC day, so the ledger can evict
/// whole days least recently used first.
fn cache_directory(host_cache: &Path, now: SystemTime) -> PathBuf {
    let days = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() / SECONDS_PER_DAY);
    host_cache.join(CACHE_DIRECTORY).join(format!("d{days}"))
}

/// Whether the cache directory child `name` (`d<days since the epoch>`) is
/// for a UTC day before the one `now_ms` falls in: `Some(false)` for today's
/// or a later day's, `None` when `name` is not a day directory's name.
pub(crate) fn past_day(name: &str, now_ms: u64) -> Option<bool> {
    let digits = name.strip_prefix('d')?;
    if digits.is_empty() || digits.len() > 12 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let day: u64 = digits.parse().ok()?;
    Some(day < now_ms / (SECONDS_PER_DAY * 1_000))
}

#[cfg(unix)]
fn create_private_directory(directory: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
}

#[cfg(not(unix))]
fn create_private_directory(directory: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(days: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(days * SECONDS_PER_DAY + 3_600)
    }

    #[test]
    fn the_policy_defaults_on_and_a_repository_can_turn_it_off() {
        assert!(NodePolicy::from_config_text("").unwrap().compile_cache);
        assert!(
            NodePolicy::from_config_text("[command_temp]\nenabled = false\n")
                .unwrap()
                .compile_cache
        );
        assert!(
            !NodePolicy::from_config_text("[node]\ncompile_cache = false\n")
                .unwrap()
                .compile_cache
        );
        assert!(NodePolicy::from_config_text("[node]\ncompile = false\n").is_err());
    }

    #[test]
    fn commands_get_one_host_wide_directory_per_day() {
        let host = tempfile::tempdir().unwrap();
        let (key, value) =
            environment_with(None, Some(host.path().to_path_buf()), day(20_415)).unwrap();
        assert_eq!(key, NODE_COMPILE_CACHE_ENV);
        let expected = host.path().join(CACHE_DIRECTORY).join("d20415");
        assert_eq!(PathBuf::from(&value), expected);
        assert!(expected.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&expected).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        // The same day gives the same directory to every command.
        let (_, again) =
            environment_with(None, Some(host.path().to_path_buf()), day(20_415)).unwrap();
        assert_eq!(again, value);
        let (_, next) =
            environment_with(None, Some(host.path().to_path_buf()), day(20_416)).unwrap();
        assert_eq!(
            PathBuf::from(next),
            host.path().join(CACHE_DIRECTORY).join("d20416")
        );
    }

    #[test]
    fn only_a_past_days_directory_is_a_past_day() {
        let now = 20_783 * SECONDS_PER_DAY * 1_000 + 5_000;
        assert_eq!(past_day("d20782", now), Some(true));
        assert_eq!(past_day("d1", now), Some(true));
        assert_eq!(past_day("d20783", now), Some(false), "today's");
        assert_eq!(past_day("d20784", now), Some(false), "a later day's");
        // The directory this module creates for `now` is today's.
        let today = cache_directory(Path::new("/cache"), UNIX_EPOCH + Duration::from_millis(now));
        let name = today.file_name().unwrap().to_str().unwrap();
        assert_eq!(past_day(name, now), Some(false));
        for other in [
            "d",
            "x20782",
            "d-1",
            "d20782x",
            "d+5",
            "v22.1.0",
            "d9999999999999",
        ] {
            assert_eq!(past_day(other, now), None, "{other}");
        }
    }

    #[test]
    fn a_caller_set_cache_or_a_missing_host_cache_is_left_alone() {
        let host = tempfile::tempdir().unwrap();
        assert!(
            environment_with(
                Some(OsString::from("/elsewhere")),
                Some(host.path().to_path_buf()),
                day(1),
            )
            .is_none()
        );
        assert!(!host.path().join(CACHE_DIRECTORY).exists());
        assert!(environment_with(None, None, day(1)).is_none());
        // An empty value is unset.
        assert!(
            environment_with(
                Some(OsString::new()),
                Some(host.path().to_path_buf()),
                day(1)
            )
            .is_some()
        );
    }

    #[test]
    fn a_session_gets_the_cache_only_once_its_repository_allowed_it() {
        let session = -8_123_456;
        assert!(environment_for_session(session).is_none());
        ENABLED_FOR_SESSIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session, false);
        assert!(environment_for_session(session).is_none());
    }
}
