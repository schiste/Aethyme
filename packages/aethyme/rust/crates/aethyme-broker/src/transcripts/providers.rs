//! Where each agent keeps its transcripts, and where an archived one goes.
//!
//! Codex writes `<codex home>/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl`
//! and keeps archived ones flat in `<codex home>/archived_sessions/` under the
//! same name. Codex reads `.jsonl.zst` natively (`codex unarchive` and
//! `codex resume` both accept it; `.jsonl.gz` they do not), so a Codex archive
//! is `archived_sessions/<same name>.zst`. Claude Code writes
//! `<claude home>/projects/<project-dir>/<uuid>.jsonl` and is not known to
//! read a compressed transcript, so its archive lives under Aethyme's host
//! state, mirroring the project directory, and only `aethyme transcripts
//! restore` brings it back.

use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The suffix an archived transcript gains.
pub(crate) const ARCHIVE_SUFFIX: &str = ".zst";
/// Every temporary file this module writes starts with this, so a run can
/// recognise and remove what an interrupted run left behind.
pub(crate) const TEMP_PREFIX: &str = ".aethyme-transcript-";
/// Directory entries one scan may visit before it stops and says so.
const MAX_SCAN_ENTRIES: u64 = 2_000_000;
/// How much of a transcript's head is read to find its working directory.
const CWD_PROBE_BYTES: u64 = 64 * 1024;
const CWD_PROBE_LINES: usize = 16;

/// An agent whose transcripts can be archived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Codex,
    Claude,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Codex => "codex",
            Provider::Claude => "claude",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "codex" => Some(Provider::Codex),
            "claude" => Some(Provider::Claude),
            _ => None,
        }
    }
}

/// The directories one host resolves for the enabled providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layout {
    /// `None` when Codex is not enabled or no home could be resolved.
    pub codex_home: Option<PathBuf>,
    /// `None` when Claude is not enabled or no home could be resolved.
    pub claude_home: Option<PathBuf>,
    /// `<host state>/transcripts`.
    pub host_dir: PathBuf,
}

impl Layout {
    pub fn codex_sessions(&self) -> Option<PathBuf> {
        self.codex_home.as_ref().map(|home| home.join("sessions"))
    }

    pub fn codex_archived(&self) -> Option<PathBuf> {
        self.codex_home
            .as_ref()
            .map(|home| home.join("archived_sessions"))
    }

    pub fn claude_projects(&self) -> Option<PathBuf> {
        self.claude_home.as_ref().map(|home| home.join("projects"))
    }

    /// Where archived Claude transcripts live: Aethyme's own host state.
    pub fn claude_archive(&self) -> PathBuf {
        self.host_dir.join("claude")
    }

    /// The directory whose open files decide whether `provider`'s
    /// transcripts are in use.
    pub fn open_root(&self, provider: Provider) -> Option<PathBuf> {
        match provider {
            Provider::Codex => self.codex_home.clone(),
            Provider::Claude => self.claude_projects(),
        }
    }
}

/// One transcript found on disk, with where its archive would go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Transcript {
    pub provider: Provider,
    pub path: PathBuf,
    pub agent_session_id: Option<String>,
    pub destination: PathBuf,
    pub bytes: u64,
    /// Milliseconds since the epoch; `None` when the clock value is unreadable.
    pub modified_ms: Option<i64>,
}

/// What one scan found.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    pub transcripts: Vec<Transcript>,
    /// Temporary files an interrupted run left in an archive directory.
    pub leftover_temps: Vec<PathBuf>,
    /// Entries that could not be read (counted, never guessed about).
    pub unreadable: u64,
    /// True when [`MAX_SCAN_ENTRIES`] stopped the walk early.
    pub truncated: bool,
}

/// True for a canonical 8-4-4-4-12 hexadecimal UUID.
pub(crate) fn is_uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn uuid_byte(byte: u8) -> bool {
    byte.is_ascii_hexdigit() || byte == b'-'
}

/// Every UUID in `text` that stands on its own (not part of a longer run of
/// hexadecimal digits and dashes), lowercased, first occurrence first.
pub(crate) fn uuids_in(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found: Vec<String> = Vec::new();
    let mut index = 0;
    while index + 36 <= bytes.len() {
        let window = &bytes[index..index + 36];
        let standalone = (index == 0 || !uuid_byte(bytes[index - 1]))
            && (index + 36 == bytes.len() || !uuid_byte(bytes[index + 36]));
        if standalone
            && let Ok(candidate) = std::str::from_utf8(window)
            && is_uuid(candidate)
        {
            let lowered = candidate.to_ascii_lowercase();
            if !found.contains(&lowered) {
                found.push(lowered);
            }
            index += 36;
            continue;
        }
        index += 1;
    }
    found
}

/// The agent session id a transcript's file name carries, if any.
///
/// Codex: `rollout-<timestamp>-<uuid>.jsonl` (optionally `.zst`). Claude:
/// `<uuid>.jsonl` (optionally `.zst`).
pub(crate) fn session_id_from_file_name(provider: Provider, name: &str) -> Option<String> {
    let name = name.strip_suffix(ARCHIVE_SUFFIX).unwrap_or(name);
    let stem = name.strip_suffix(".jsonl")?;
    match provider {
        Provider::Codex => {
            if !stem.starts_with("rollout-") {
                return None;
            }
            let tail = stem.get(stem.len().checked_sub(36)?..)?;
            let separated = stem.as_bytes().get(stem.len().checked_sub(37)?) == Some(&b'-');
            (separated && is_uuid(tail)).then(|| tail.to_ascii_lowercase())
        }
        Provider::Claude => is_uuid(stem).then(|| stem.to_ascii_lowercase()),
    }
}

/// `archived_sessions/<name>.zst` for a Codex transcript named `<name>`.
pub(crate) fn codex_destination(archived_root: &Path, source: &Path) -> Option<PathBuf> {
    let name = source.file_name()?.to_str()?;
    name.ends_with(".jsonl")
        .then(|| archived_root.join(format!("{name}{ARCHIVE_SUFFIX}")))
}

/// `<archive root>/<project-dir>/<name>.zst` for a Claude transcript at
/// `<projects>/<project-dir>/<name>`; `None` for anything deeper or shallower.
pub(crate) fn claude_destination(
    archive_root: &Path,
    projects_root: &Path,
    source: &Path,
) -> Option<PathBuf> {
    let relative = source.strip_prefix(projects_root).ok()?;
    let mut components = relative.components();
    let project = components.next()?;
    let file = components.next()?;
    if components.next().is_some() {
        return None;
    }
    let name = file.as_os_str().to_str()?;
    if !name.ends_with(".jsonl") {
        return None;
    }
    Some(
        archive_root
            .join(project.as_os_str())
            .join(format!("{name}{ARCHIVE_SUFFIX}")),
    )
}

/// Where Codex itself would keep a live transcript of this name:
/// `sessions/YYYY/MM/DD/<name>` from the `rollout-YYYY-MM-DD…` prefix.
pub(crate) fn codex_session_path_for(sessions_root: &Path, name: &str) -> Option<PathBuf> {
    let rest = name.strip_prefix("rollout-")?;
    let date = rest.get(..10)?;
    let bytes = date.as_bytes();
    let shaped = bytes.iter().enumerate().all(|(index, byte)| match index {
        4 | 7 => *byte == b'-',
        _ => byte.is_ascii_digit(),
    });
    shaped.then(|| {
        sessions_root
            .join(&date[..4])
            .join(&date[5..7])
            .join(&date[8..10])
            .join(name)
    })
}

/// Milliseconds since the epoch of a file's modification time.
pub(crate) fn modified_ms(metadata: &fs::Metadata) -> Option<i64> {
    let modified = metadata.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// The working directory a transcript's first lines record (`cwd` at the top
/// level, or under `payload` for Codex's `session_meta`).
pub(crate) fn transcript_cwd(path: &Path) -> Option<PathBuf> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file.take(CWD_PROBE_BYTES));
    for line in reader.lines().take(CWD_PROBE_LINES) {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let cwd = value
            .get("cwd")
            .or_else(|| value.get("payload").and_then(|payload| payload.get("cwd")))
            .and_then(serde_json::Value::as_str);
        if let Some(cwd) = cwd {
            let cwd = PathBuf::from(cwd);
            if cwd.is_absolute() {
                return Some(cwd);
            }
        }
    }
    None
}

struct Walker<'a> {
    scan: &'a mut Scan,
    visited: u64,
}

impl Walker<'_> {
    /// Entries of `directory`, or `None` when it is absent or unreadable.
    fn entries(&mut self, directory: &Path) -> Option<Vec<(PathBuf, fs::Metadata)>> {
        let reader = match fs::read_dir(directory) {
            Ok(reader) => reader,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(_) => {
                self.scan.unreadable += 1;
                return None;
            }
        };
        let mut entries = Vec::new();
        for entry in reader {
            self.visited += 1;
            if self.visited > MAX_SCAN_ENTRIES {
                self.scan.truncated = true;
                break;
            }
            let Ok(entry) = entry else {
                self.scan.unreadable += 1;
                continue;
            };
            let path = entry.path();
            // Never follow a symbolic link: only real files are archived.
            match fs::symlink_metadata(&path) {
                Ok(metadata) => entries.push((path, metadata)),
                Err(_) => self.scan.unreadable += 1,
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Some(entries)
    }

    fn push(
        &mut self,
        provider: Provider,
        path: PathBuf,
        metadata: &fs::Metadata,
        destination: PathBuf,
    ) {
        let agent_session_id = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| session_id_from_file_name(provider, name));
        self.scan.transcripts.push(Transcript {
            provider,
            agent_session_id,
            destination,
            bytes: metadata.len(),
            modified_ms: modified_ms(metadata),
            path,
        });
    }

    fn note_temp(&mut self, path: &Path, metadata: &fs::Metadata) -> bool {
        let is_temp = metadata.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(TEMP_PREFIX));
        if is_temp {
            self.scan.leftover_temps.push(path.to_path_buf());
        }
        is_temp
    }

    /// Codex's live tree: `sessions/YYYY/MM/DD/rollout-*.jsonl`.
    fn codex_sessions(&mut self, directory: &Path, archived_root: &Path, depth: usize) {
        let Some(entries) = self.entries(directory) else {
            return;
        };
        for (path, metadata) in entries {
            if metadata.is_dir() && depth < 3 {
                self.codex_sessions(&path, archived_root, depth + 1);
            } else if metadata.is_file() && !self.note_temp(&path, &metadata) {
                let rollout = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"));
                if rollout && let Some(destination) = codex_destination(archived_root, &path) {
                    self.push(Provider::Codex, path, &metadata, destination);
                }
            }
        }
    }

    /// Transcripts Codex already archived but left uncompressed.
    fn codex_archived(&mut self, archived_root: &Path) {
        let Some(entries) = self.entries(archived_root) else {
            return;
        };
        for (path, metadata) in entries {
            if !metadata.is_file() || self.note_temp(&path, &metadata) {
                continue;
            }
            let rollout = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"));
            if rollout && let Some(destination) = codex_destination(archived_root, &path) {
                self.push(Provider::Codex, path, &metadata, destination);
            }
        }
    }

    /// Claude Code: `projects/<project-dir>/<uuid>.jsonl`.
    fn claude_projects(&mut self, projects_root: &Path, archive_root: &Path) {
        let Some(projects) = self.entries(projects_root) else {
            return;
        };
        for (project, metadata) in projects {
            if !metadata.is_dir() {
                continue;
            }
            let Some(entries) = self.entries(&project) else {
                continue;
            };
            for (path, metadata) in entries {
                if !metadata.is_file() {
                    continue;
                }
                let named = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        session_id_from_file_name(Provider::Claude, name).is_some()
                    });
                if named
                    && let Some(destination) =
                        claude_destination(archive_root, projects_root, &path)
                {
                    self.push(Provider::Claude, path, &metadata, destination);
                }
            }
        }
    }

    /// Temporary files left in the Claude archive tree.
    fn claude_archive_temps(&mut self, archive_root: &Path) {
        let Some(projects) = self.entries(archive_root) else {
            return;
        };
        for (project, metadata) in projects {
            if metadata.is_dir()
                && let Some(entries) = self.entries(&project)
            {
                for (path, metadata) in entries {
                    self.note_temp(&path, &metadata);
                }
            } else {
                self.note_temp(&project, &metadata);
            }
        }
    }
}

/// Every transcript of the enabled providers. Reads directories only; opens
/// no file.
pub(crate) fn scan(layout: &Layout, providers: &[Provider]) -> Scan {
    let mut scan = Scan::default();
    let mut walker = Walker {
        scan: &mut scan,
        visited: 0,
    };
    for provider in providers {
        match provider {
            Provider::Codex => {
                if let (Some(sessions), Some(archived)) =
                    (layout.codex_sessions(), layout.codex_archived())
                {
                    walker.codex_sessions(&sessions, &archived, 0);
                    walker.codex_archived(&archived);
                }
            }
            Provider::Claude => {
                if let Some(projects) = layout.claude_projects() {
                    let archive = layout.claude_archive();
                    walker.claude_projects(&projects, &archive);
                    walker.claude_archive_temps(&archive);
                }
            }
        }
    }
    scan
}
