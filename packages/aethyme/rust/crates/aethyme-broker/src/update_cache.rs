//! A time-to-live cache in front of release-manifest fetches.
//!
//! `aethyme update check` was an unconditional network round trip. That is
//! exactly right for a command a person types and disqualifying for anything
//! that wants to ask the question on its own: a turn boundary cannot afford a
//! GitHub request, and a laptop on a plane cannot afford one that fails.
//!
//! So the network answer is cached and the cache is what automatic callers
//! read. Two properties make that safe:
//!
//! * **The manifest is cached, never the plan.** A plan also depends on the
//!   version of the binary asking and on how it was installed, and both change
//!   without the network changing. Caching the manifest bytes leaves
//!   [`crate::update::build_update_plan`] to run every time, so the moment you
//!   reinstall, `update check` reports `UpToDate` instead of repeating a
//!   cached verdict until the entry expires.
//! * **A stale entry still beats an error.** When the fetch fails and an
//!   expired entry exists, it is served with its real age attached rather than
//!   discarded. The caller is told, so "you are two versions behind" can never
//!   be printed on the strength of a manifest from last month without saying
//!   so.
//!
//! Nothing here installs anything. `aethyme update` keeps its stance that
//! changing the binaries is explicit and confirmed; this caches an *answer*,
//! which is the part that was never the dangerous half.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::host_state::{default_host_cache_dir, protect_host_state_path};

pub const CACHE_SCHEMA_VERSION: u32 = 1;

/// Long enough that a session-per-hour workflow makes at most a handful of
/// requests a day, short enough that a release published this morning is
/// noticed by this afternoon.
pub const DEFAULT_TTL_SECONDS: i64 = 6 * 60 * 60;

const CACHE_SUBDIRECTORY: &str = "update-manifests";
const TTL_ENVIRONMENT_VARIABLE: &str = "AETHYME_UPDATE_CACHE_TTL_SECONDS";

/// One cached fetch, keyed by the URL it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedManifest {
    pub schema_version: u32,
    pub url: String,
    pub fetched_at_unix_ms: i64,
    /// The manifest verbatim. Release manifests are JSON, so a non-UTF-8 body
    /// is not a manifest and is never cached.
    pub body: String,
}

/// Where a set of manifest bytes came from, which the caller has to be able to
/// say out loud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestSource {
    Network,
    Cache,
    /// The fetch failed and an expired entry was served instead. Carries the
    /// failure so the caller can explain why it is reading old news.
    StaleCacheAfterFailure(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestFetch {
    pub bytes: Vec<u8>,
    /// Age of the bytes. Zero for a live fetch.
    pub age_seconds: i64,
    pub source: ManifestSource,
}

impl ManifestFetch {
    /// A one-line note when the bytes are not live, or `None` when they are.
    ///
    /// Returned rather than printed: the JSON renderers must not grow prose,
    /// and the hook needs the same sentence in a different envelope.
    pub fn freshness_note(&self) -> Option<String> {
        match &self.source {
            ManifestSource::Network => None,
            ManifestSource::Cache => Some(format!(
                "cached release manifest, {}",
                describe_age(self.age_seconds)
            )),
            ManifestSource::StaleCacheAfterFailure(error) => Some(format!(
                "offline: kept the expired cached manifest ({}) because the fetch failed: {error}",
                describe_age(self.age_seconds)
            )),
        }
    }
}

/// Render an age the way a person reads one.
pub fn describe_age(age_seconds: i64) -> String {
    match age_seconds {
        age if age < 90 => "just now".to_string(),
        age if age < 90 * 60 => format!("{}m old", age / 60),
        age if age < 48 * 60 * 60 => format!("{}h old", age / 3600),
        age => format!("{}d old", age / (24 * 3600)),
    }
}

/// The configured lifetime, where zero disables caching entirely.
///
/// An unparseable value falls back to the default rather than failing: a typo
/// in an environment variable must not break `update check`.
pub fn ttl_seconds() -> i64 {
    match std::env::var(TTL_ENVIRONMENT_VARIABLE) {
        Ok(raw) => raw
            .trim()
            .parse::<i64>()
            .unwrap_or(DEFAULT_TTL_SECONDS)
            .max(0),
        Err(_) => DEFAULT_TTL_SECONDS,
    }
}

/// Whether an entry fetched at `fetched_at_unix_ms` is still within `ttl_seconds`.
///
/// A timestamp in the future means the clock moved backwards since the write.
/// That counts as expired: refetching is the recoverable direction, trusting it
/// would pin the entry until the clock caught up.
pub fn is_fresh(fetched_at_unix_ms: i64, now_unix_ms: i64, ttl_seconds: i64) -> bool {
    let age = age_seconds(fetched_at_unix_ms, now_unix_ms);
    age >= 0 && age < ttl_seconds
}

pub fn age_seconds(fetched_at_unix_ms: i64, now_unix_ms: i64) -> i64 {
    (now_unix_ms - fetched_at_unix_ms) / 1000
}

/// Fetch `url`, serving a fresh cached copy instead when one exists.
///
/// `fetch` is injected so the policy above is testable without a network.
pub fn fetch_manifest_cached(
    url: &str,
    now_unix_ms: i64,
    refresh: bool,
    fetch: impl FnOnce(&str) -> Result<Vec<u8>, String>,
) -> Result<ManifestFetch, String> {
    let ttl = ttl_seconds();
    // Disabling the cache disables the stale fallback with it: a zero TTL is a
    // request for live answers only, not for old ones on a bad day.
    let cached = (ttl > 0).then(|| read_entry(url)).flatten();

    if !refresh {
        if let Some(entry) = &cached
            && is_fresh(entry.fetched_at_unix_ms, now_unix_ms, ttl)
        {
            return Ok(ManifestFetch {
                bytes: entry.body.clone().into_bytes(),
                age_seconds: age_seconds(entry.fetched_at_unix_ms, now_unix_ms),
                source: ManifestSource::Cache,
            });
        }
    }

    match fetch(url) {
        Ok(bytes) => {
            if ttl > 0 {
                write_entry(url, &bytes, now_unix_ms);
            }
            Ok(ManifestFetch {
                bytes,
                age_seconds: 0,
                source: ManifestSource::Network,
            })
        }
        // Expired is not worthless. An answer from this morning is a better
        // basis for "are you current?" than refusing to answer at all, as long
        // as its age travels with it.
        Err(error) => match cached {
            Some(entry) => Ok(ManifestFetch {
                bytes: entry.body.clone().into_bytes(),
                age_seconds: age_seconds(entry.fetched_at_unix_ms, now_unix_ms).max(0),
                source: ManifestSource::StaleCacheAfterFailure(error),
            }),
            None => Err(error),
        },
    }
}

/// The cached manifest for `url` if one is present and unexpired.
///
/// The read-only half, for callers that must never touch the network:
/// returns `None` rather than fetching, so a cold cache costs nothing.
pub fn read_fresh(url: &str, now_unix_ms: i64) -> Option<ManifestFetch> {
    let ttl = ttl_seconds();
    if ttl == 0 {
        return None;
    }
    let entry = read_entry(url)?;
    if !is_fresh(entry.fetched_at_unix_ms, now_unix_ms, ttl) {
        return None;
    }
    Some(ManifestFetch {
        bytes: entry.body.clone().into_bytes(),
        age_seconds: age_seconds(entry.fetched_at_unix_ms, now_unix_ms),
        source: ManifestSource::Cache,
    })
}

/// True when nothing usable is cached for `url`, so a caller that only reads
/// knows it will keep getting nothing until someone refreshes.
pub fn needs_refresh(url: &str, now_unix_ms: i64) -> bool {
    ttl_seconds() > 0 && read_fresh(url, now_unix_ms).is_none()
}

/// The file an entry for `url` lives in.
///
/// The URL is hashed rather than escaped: it already encodes the channel and
/// any `AETHYME_RELEASE_BASE_URL` override, so one hash per distinct question
/// is exactly the granularity wanted, and no URL can escape the directory.
pub fn cache_path(url: &str) -> Option<PathBuf> {
    let digest = Sha256::digest(url.as_bytes());
    Some(
        default_host_cache_dir()?
            .join(CACHE_SUBDIRECTORY)
            .join(format!("{digest:x}.json")),
    )
}

/// Record that a refresh for `url` is being attempted, returning whether this
/// caller is the one that should attempt it.
///
/// A failed fetch writes no entry, so without this an offline machine would
/// launch a fresh attempt at every single session start and never stop. The
/// marker expires on the same clock as the entry it is standing in for, so a
/// machine that comes back online retries on the next ordinary boundary.
///
/// Best effort in both directions: if the marker cannot be written, the answer
/// is "go ahead", because failing to throttle is better than never refreshing.
pub fn claim_refresh_attempt(url: &str, now_unix_ms: i64) -> bool {
    let Some(path) = attempt_path(url) else {
        return true;
    };
    if let Ok(raw) = std::fs::read_to_string(&path)
        && let Ok(last) = raw.trim().parse::<i64>()
        && is_fresh(last, now_unix_ms, ttl_seconds())
    {
        return false;
    }
    if let Some(directory) = path.parent() {
        let _ = std::fs::create_dir_all(directory);
        let _ = protect_host_state_path(directory, true);
    }
    let _ = std::fs::write(&path, now_unix_ms.to_string());
    true
}

fn attempt_path(url: &str) -> Option<PathBuf> {
    let digest = Sha256::digest(url.as_bytes());
    Some(
        default_host_cache_dir()?
            .join(CACHE_SUBDIRECTORY)
            .join(format!("{digest:x}.attempt")),
    )
}

fn read_entry(url: &str) -> Option<CachedManifest> {
    let path = cache_path(url)?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let entry: CachedManifest = serde_json::from_str(&raw).ok()?;
    // A schema bump or a hash collision both mean "not the entry I wanted".
    // Both are handled by ignoring it, which degrades to a live fetch.
    (entry.schema_version == CACHE_SCHEMA_VERSION && entry.url == url).then_some(entry)
}

/// Best-effort write. A cache that cannot be written is a slower `update
/// check`, never a failed one, so every error here is dropped on purpose.
fn write_entry(url: &str, bytes: &[u8], now_unix_ms: i64) {
    let Ok(body) = std::str::from_utf8(bytes) else {
        return;
    };
    let Some(path) = cache_path(url) else {
        return;
    };
    let Some(directory) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }
    let _ = protect_host_state_path(directory, true);

    let entry = CachedManifest {
        schema_version: CACHE_SCHEMA_VERSION,
        url: url.to_string(),
        fetched_at_unix_ms: now_unix_ms,
        body: body.to_string(),
    };
    let Ok(encoded) = serde_json::to_vec_pretty(&entry) else {
        return;
    };
    let Ok(temporary) = tempfile::NamedTempFile::new_in(directory) else {
        return;
    };
    if std::fs::write(temporary.path(), &encoded).is_err() {
        return;
    }
    if temporary.persist(&path).is_ok() {
        let _ = protect_host_state_path(&path, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE_MS: i64 = 60 * 1000;

    #[test]
    fn an_entry_inside_the_window_is_fresh_and_one_past_it_is_not() {
        assert!(is_fresh(0, 59 * 1000, 60));
        assert!(!is_fresh(0, 60 * 1000, 60));
        assert!(!is_fresh(0, 10 * 60 * 1000, 60));
    }

    /// A clock that moved backwards must expire the entry rather than pin it.
    #[test]
    fn a_future_timestamp_is_treated_as_expired() {
        assert!(!is_fresh(10 * MINUTE_MS, 0, DEFAULT_TTL_SECONDS));
    }

    #[test]
    fn a_zero_ttl_expires_everything_immediately() {
        assert!(!is_fresh(0, 0, 0));
    }

    #[test]
    fn distinct_urls_do_not_share_an_entry() {
        let stable = cache_path("https://example.test/stable/release-manifest.json");
        let preview = cache_path("https://example.test/preview/release-manifest.json");
        assert!(stable.is_some());
        assert_ne!(stable, preview);
    }

    /// The hash keeps a URL from steering the write out of the cache directory.
    #[test]
    fn a_traversing_url_cannot_escape_the_cache_directory() {
        let path = cache_path("file:///../../etc/passwd").expect("cache path");
        let name = path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .to_string();
        assert!(name.ends_with(".json"), "{name}");
        assert!(!name.contains('/') && !name.contains(".."), "{name}");
        assert!(path.parent().expect("parent").ends_with(CACHE_SUBDIRECTORY));
    }

    #[test]
    fn ages_read_the_way_a_person_reads_them() {
        assert_eq!(describe_age(0), "just now");
        assert_eq!(describe_age(89), "just now");
        assert_eq!(describe_age(10 * 60), "10m old");
        assert_eq!(describe_age(3 * 60 * 60), "3h old");
        assert_eq!(describe_age(5 * 24 * 60 * 60), "5d old");
    }

    /// Live bytes say nothing; anything older explains itself.
    #[test]
    fn only_a_non_live_fetch_carries_a_note() {
        let live = ManifestFetch {
            bytes: Vec::new(),
            age_seconds: 0,
            source: ManifestSource::Network,
        };
        assert_eq!(live.freshness_note(), None);

        let cached = ManifestFetch {
            bytes: Vec::new(),
            age_seconds: 7200,
            source: ManifestSource::Cache,
        };
        assert_eq!(
            cached.freshness_note().as_deref(),
            Some("cached release manifest, 2h old")
        );

        let offline = ManifestFetch {
            bytes: Vec::new(),
            age_seconds: 3 * 24 * 3600,
            source: ManifestSource::StaleCacheAfterFailure("dns failure".into()),
        };
        let note = offline
            .freshness_note()
            .expect("a stale entry must explain itself");
        assert!(note.contains("offline"), "{note}");
        assert!(note.contains("3d old"), "{note}");
        assert!(note.contains("dns failure"), "{note}");
    }

    /// A failing fetch with nothing cached is still an error -- the fallback
    /// must not invent an answer out of an empty cache.
    #[test]
    fn a_failed_fetch_with_a_cold_cache_reports_the_failure() {
        let url = "https://example.invalid/never-cached/release-manifest.json";
        let result = fetch_manifest_cached(url, 0, true, |_| Err("no route to host".into()));
        assert_eq!(result.unwrap_err(), "no route to host");
    }
}
