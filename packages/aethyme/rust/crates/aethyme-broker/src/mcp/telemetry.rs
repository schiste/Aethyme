//! What `aethyme mcp` did and how long each phase took, under
//! `<host state>/mcp/`.
//!
//! - `events.jsonl`: one line per `connect` or `resolve`: outcome (`exec`,
//!   `fallback`, `error`, `resolved`, `unresolved`), the failure reason code,
//!   the chosen copy, the short key and per-phase milliseconds. Kept to the
//!   newest [`KEEP_EVENTS`] lines once it passes [`MAX_EVENTS_BYTES`].
//!   `status` summarizes it per entry.
//! - `resolved/<record>.json`: the last stable-copy resolution of each entry
//!   (argv and executable identity), so a changed executable is reported.
//!
//! Recording is best effort: a failure to write never stops a server from
//! starting. Neither file holds environment values.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::resolve::{ExecutableIdentity, identity_changes};

pub const EVENTS_FILE: &str = "events.jsonl";
pub const RESOLVED_DIRECTORY: &str = "resolved";
pub const MAX_EVENTS_BYTES: u64 = 1024 * 1024;
pub const KEEP_EVENTS: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub at_ms: u64,
    pub command: String,
    pub name: String,
    pub outcome: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub copy: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub phases_ms: BTreeMap<String, u64>,
    /// A repository entry of the same name was ignored for the host entry.
    #[serde(default)]
    pub shadowed: bool,
    /// The fallback command was run.
    #[serde(default)]
    pub fallback: bool,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn ensure_private_directory(directory: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
}

/// Append one event; trim the file when it has grown past the limit.
pub fn append(directory: &Path, event: &Event) -> std::io::Result<()> {
    ensure_private_directory(directory)?;
    let path = directory.join(EVENTS_FILE);
    let mut line = serde_json::to_vec(event).map_err(std::io::Error::other)?;
    line.push(b'\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    // One write per line: appends from concurrent connects do not interleave.
    file.write_all(&line)?;
    if file.metadata()?.len() > MAX_EVENTS_BYTES {
        trim(directory, &path)?;
    }
    Ok(())
}

fn trim(directory: &Path, path: &Path) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().collect();
    let kept = &lines[lines.len().saturating_sub(KEEP_EVENTS)..];
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    for line in kept {
        temporary.write_all(line.as_bytes())?;
        temporary.write_all(b"\n")?;
    }
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Every readable event, oldest first. Unparsable lines are skipped.
pub fn read_events(directory: &Path) -> Vec<Event> {
    std::fs::read_to_string(directory.join(EVENTS_FILE))
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Counters for one entry name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Counters {
    pub events: u64,
    /// Per outcome: `exec`, `exec_failed`, `fallback`, `error`, `resolved`,
    /// `unresolved`.
    pub outcomes: BTreeMap<String, u64>,
    /// Failures (fallbacks, errors, unresolved) per reason code.
    pub failure_reasons: BTreeMap<String, u64>,
    pub resolve_ms_p50: Option<u64>,
    pub resolve_ms_max: Option<u64>,
    pub last_at_ms: Option<u64>,
    pub last_copy: Option<String>,
    pub last_key: Option<String>,
    /// Connects where a repository entry of the same name was ignored.
    pub shadowed_repository_entries: u64,
}

/// Counters per entry name.
pub fn summarize(events: &[Event]) -> BTreeMap<String, Counters> {
    let mut summary: BTreeMap<String, Counters> = BTreeMap::new();
    let mut resolve_times: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for event in events {
        let counters = summary.entry(event.name.clone()).or_default();
        counters.events += 1;
        *counters.outcomes.entry(event.outcome.clone()).or_default() += 1;
        if event.outcome == "exec_failed" {
            // Recorded after an `exec` event of the same attempt: undo it.
            if let Some(execs) = counters.outcomes.get_mut("exec") {
                *execs = execs.saturating_sub(1);
            }
            counters.events = counters.events.saturating_sub(1);
            if event.fallback {
                *counters.outcomes.entry("fallback".into()).or_default() += 1;
            }
        }
        if event.shadowed {
            counters.shadowed_repository_entries += 1;
        }
        if let Some(reason) = &event.reason {
            *counters.failure_reasons.entry(reason.clone()).or_default() += 1;
        }
        counters.last_at_ms = Some(event.at_ms);
        if event.copy.is_some() {
            counters.last_copy.clone_from(&event.copy);
        }
        if event.key.is_some() {
            counters.last_key.clone_from(&event.key);
        }
        if let Some(milliseconds) = event.phases_ms.get("resolve") {
            resolve_times
                .entry(event.name.clone())
                .or_default()
                .push(*milliseconds);
        }
    }
    for (name, mut times) in resolve_times {
        times.sort_unstable();
        if let Some(counters) = summary.get_mut(&name) {
            counters.resolve_ms_p50 = times.get(times.len() / 2).copied();
            counters.resolve_ms_max = times.last().copied();
        }
    }
    summary
}

/// A remembered stable-copy resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    pub argv: Vec<String>,
    pub identity: ExecutableIdentity,
    pub at_ms: u64,
}

/// How the current resolution compares with the remembered one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub first_seen: bool,
    /// `argv`, `path`, `content`, `shebang`, `interpreter`.
    pub changes: Vec<&'static str>,
    pub previous_at_ms: Option<u64>,
}

/// The file a record is kept in. `record` must be a safe file name stem.
pub fn remembered_path(directory: &Path, record: &str) -> PathBuf {
    directory
        .join(RESOLVED_DIRECTORY)
        .join(format!("{record}.json"))
}

/// Compare with the remembered resolution and, when it differs or is new,
/// remember the current one.
pub fn compare_and_remember(
    directory: &Path,
    record: &str,
    argv: &[String],
    identity: &ExecutableIdentity,
) -> Change {
    let path = remembered_path(directory, record);
    let previous: Option<Remembered> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let change = match &previous {
        None => Change {
            first_seen: true,
            changes: Vec::new(),
            previous_at_ms: None,
        },
        Some(previous) => {
            let mut changes = Vec::new();
            if previous.argv != argv {
                changes.push("argv");
            }
            changes.extend(identity_changes(&previous.identity, identity));
            Change {
                first_seen: false,
                changes,
                previous_at_ms: Some(previous.at_ms),
            }
        }
    };
    if change.first_seen || !change.changes.is_empty() {
        let remembered = Remembered {
            argv: argv.to_vec(),
            identity: identity.clone(),
            at_ms: now_ms(),
        };
        crate::warn_unrecorded(
            "remember the MCP resolution",
            write_remembered(&path, &remembered),
        );
    }
    change
}

fn write_remembered(path: &Path, remembered: &Remembered) -> std::io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent"))?;
    ensure_private_directory(directory)?;
    let bytes = serde_json::to_vec_pretty(remembered).map_err(std::io::Error::other)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&bytes)?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}
