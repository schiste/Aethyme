//! `[transcripts]` in `<host state>/transcripts/settings.toml`: off unless the
//! owner turns it on. `aethyme transcripts enable|disable` writes this file
//! and nothing else.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::providers::Provider;

/// The settings file, under `<host state>/transcripts/`.
pub const SETTINGS_FILE: &str = "settings.toml";
pub const DEFAULT_ARCHIVE_AFTER_DAYS: u32 = 3;
const MAX_ARCHIVE_AFTER_DAYS: i64 = 3650;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
/// Unattended archiving reads at most this many source bytes per second.
pub const DEFAULT_BYTES_PER_SECOND: u64 = 32 * MIB;
/// A configured rate is never slower than this, so the lane always progresses.
pub const MIN_BYTES_PER_SECOND: u64 = 4 * MIB;
/// Below this much free space on the archive volume the lane runs unpaced:
/// the space is needed now.
pub const DEFAULT_FREE_SPACE_FLOOR_BYTES: u64 = 20 * GIB;

/// The host's transcript-archiving settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptsConfig {
    pub archive: bool,
    pub archive_after_days: u32,
    pub providers: Vec<Provider>,
    /// Overrides `$CODEX_HOME` and `~/.codex`.
    pub codex_home: Option<PathBuf>,
    /// Overrides `$CLAUDE_CONFIG_DIR` and `~/.claude`.
    pub claude_home: Option<PathBuf>,
    /// Repositories whose broker databases link sessions to transcripts, in
    /// addition to the ones found in the worktree registry and transcripts.
    pub repositories: Vec<PathBuf>,
    pub bytes_per_second: u64,
    pub free_space_floor_bytes: u64,
}

impl Default for TranscriptsConfig {
    fn default() -> Self {
        Self {
            archive: false,
            archive_after_days: DEFAULT_ARCHIVE_AFTER_DAYS,
            providers: vec![Provider::Codex],
            codex_home: None,
            claude_home: None,
            repositories: Vec::new(),
            bytes_per_second: DEFAULT_BYTES_PER_SECOND,
            free_space_floor_bytes: DEFAULT_FREE_SPACE_FLOOR_BYTES,
        }
    }
}

fn absolute_path(key: &str, value: &toml::Value) -> Result<PathBuf, String> {
    let Some(text) = value.as_str() else {
        return Err(format!("transcripts.{key} must be a path string"));
    };
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return Err(format!("transcripts.{key} must be an absolute path"));
    }
    Ok(path)
}

fn positive_integer(key: &str, value: &toml::Value, minimum: i64) -> Result<u64, String> {
    match value.as_integer() {
        Some(number) if number >= minimum => Ok(number as u64),
        _ => Err(format!(
            "transcripts.{key} must be an integer of at least {minimum}"
        )),
    }
}

impl TranscriptsConfig {
    /// Parse a settings file. A file without `[transcripts]` is the default.
    pub fn parse(text: &str) -> Result<Self, String> {
        let table: toml::Table = toml::from_str(text)
            .map_err(|error| format!("settings are not valid TOML: {error}"))?;
        let mut config = Self::default();
        let Some(section) = table.get("transcripts") else {
            return Ok(config);
        };
        let Some(section) = section.as_table() else {
            return Err("[transcripts] must be a table".into());
        };
        for (key, value) in section {
            match key.as_str() {
                "archive" => {
                    config.archive = value
                        .as_bool()
                        .ok_or("transcripts.archive must be true or false")?;
                }
                "archive_after_days" => {
                    let days = positive_integer(key, value, 1)?;
                    if days > MAX_ARCHIVE_AFTER_DAYS as u64 {
                        return Err(format!(
                            "transcripts.archive_after_days must be at most {MAX_ARCHIVE_AFTER_DAYS}"
                        ));
                    }
                    config.archive_after_days = days as u32;
                }
                "providers" => {
                    let Some(items) = value.as_array() else {
                        return Err("transcripts.providers must be a list".into());
                    };
                    let mut providers = Vec::new();
                    for item in items {
                        let provider = item.as_str().and_then(Provider::parse).ok_or(
                            "transcripts.providers entries must be \"codex\" or \"claude\"",
                        )?;
                        if !providers.contains(&provider) {
                            providers.push(provider);
                        }
                    }
                    config.providers = providers;
                }
                "codex_home" => config.codex_home = Some(absolute_path(key, value)?),
                "claude_home" => config.claude_home = Some(absolute_path(key, value)?),
                "repositories" => {
                    let Some(items) = value.as_array() else {
                        return Err("transcripts.repositories must be a list".into());
                    };
                    config.repositories = items
                        .iter()
                        .map(|item| absolute_path(key, item))
                        .collect::<Result<_, _>>()?;
                }
                "bytes_per_second" => config.bytes_per_second = positive_integer(key, value, 1)?,
                "free_space_floor_bytes" => {
                    config.free_space_floor_bytes = positive_integer(key, value, 0)?;
                }
                other => return Err(format!("unknown setting transcripts.{other}")),
            }
        }
        Ok(config)
    }

    /// The host's settings; the default (off) when the file does not exist.
    pub fn load(host_dir: &Path) -> Result<Self, String> {
        let path = host_dir.join(SETTINGS_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|error| format!("transcript settings: {error}")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("cannot read the transcript settings: {error}")),
        }
    }

    /// The pacing rate, never below [`MIN_BYTES_PER_SECOND`].
    pub fn effective_bytes_per_second(&self) -> u64 {
        self.bytes_per_second.max(MIN_BYTES_PER_SECOND)
    }
}

/// What `enable` or `disable` changes.
#[derive(Debug, Clone, Default)]
pub(crate) struct SettingsChange {
    pub archive: bool,
    pub archive_after_days: Option<u32>,
    pub providers: Option<Vec<Provider>>,
}

/// Apply `change` to the settings file, keeping every other line, and return
/// the resulting settings. The file is replaced atomically.
pub(crate) fn write_settings(
    host_dir: &Path,
    change: &SettingsChange,
) -> Result<TranscriptsConfig, String> {
    let path = host_dir.join(SETTINGS_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("cannot read the transcript settings: {error}")),
    };
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|error| format!("the transcript settings are not valid TOML: {error}"))?;
    if !document.contains_key("transcripts") {
        document.insert(
            "transcripts",
            toml_edit::Item::Table(toml_edit::Table::new()),
        );
    }
    let section = document["transcripts"]
        .as_table_mut()
        .ok_or("[transcripts] must be a table")?;
    section.insert("archive", toml_edit::value(change.archive));
    if let Some(days) = change.archive_after_days {
        section.insert("archive_after_days", toml_edit::value(i64::from(days)));
    }
    if let Some(providers) = &change.providers {
        let mut array = toml_edit::Array::new();
        for provider in providers {
            array.push(provider.label());
        }
        section.insert("providers", toml_edit::value(array));
    }
    let rendered = document.to_string();
    let config = TranscriptsConfig::parse(&rendered)?;
    super::ensure_private_directory(host_dir)
        .map_err(|error| format!("cannot create the transcript state directory: {error}"))?;
    crate::atomic_file::with_synced_temporary(&path, rendered.as_bytes(), |temporary| {
        fs::rename(temporary, &path)
    })
    .map_err(|error| format!("cannot write the transcript settings: {error}"))?;
    super::sync_directory(host_dir);
    Ok(config)
}
