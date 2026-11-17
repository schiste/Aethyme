//! `[transcripts]` in `<host state>/transcripts/settings.toml`. Archiving is
//! on by default (Codex and Claude Code) when the file is absent or omits
//! `archive`; an explicit `archive = false` turns it off.
//! `aethyme transcripts enable|disable` writes this file and nothing else.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::providers::Provider;

/// The settings file, under `<host state>/transcripts/`.
pub const SETTINGS_FILE: &str = "settings.toml";
/// Days without activity before a Codex transcript qualifies.
pub const DEFAULT_ARCHIVE_AFTER_INACTIVE_DAYS: u32 = 3;
/// Claude Code keeps no transcript open while a session is idle, so its
/// transcripts wait longer before they count as finished.
pub const DEFAULT_ARCHIVE_AFTER_INACTIVE_DAYS_CLAUDE: u32 = 14;
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

/// Where `archive` came from: the built-in default (no settings file, or a
/// file that omits `archive`) or an explicit `archive = true|false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnabledSource {
    Default,
    Settings,
}

impl EnabledSource {
    pub fn label(self) -> &'static str {
        match self {
            EnabledSource::Default => "default",
            EnabledSource::Settings => "settings",
        }
    }
}

/// The host's transcript-archiving settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptsConfig {
    pub archive: bool,
    /// Whether `archive` is the default or set in the settings file.
    pub enabled_source: EnabledSource,
    /// Days without activity before a Codex transcript qualifies
    /// (`archive_after_inactive_days`; the older `archive_after_days` is
    /// still read).
    pub archive_after_inactive_days: u32,
    /// Days without activity before a Claude Code transcript qualifies
    /// (`archive_after_inactive_days_claude`; the older
    /// `archive_after_days_claude` is still read).
    pub archive_after_inactive_days_claude: u32,
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
            archive: true,
            enabled_source: EnabledSource::Default,
            archive_after_inactive_days: DEFAULT_ARCHIVE_AFTER_INACTIVE_DAYS,
            archive_after_inactive_days_claude: DEFAULT_ARCHIVE_AFTER_INACTIVE_DAYS_CLAUDE,
            providers: vec![Provider::Codex, Provider::Claude],
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

fn days(key: &str, value: &toml::Value) -> Result<u32, String> {
    let days = positive_integer(key, value, 1)?;
    if days > MAX_ARCHIVE_AFTER_DAYS as u64 {
        return Err(format!(
            "transcripts.{key} must be at most {MAX_ARCHIVE_AFTER_DAYS}"
        ));
    }
    Ok(days as u32)
}

impl TranscriptsConfig {
    /// Days without activity before `provider`'s transcripts qualify.
    pub fn days_for(&self, provider: Provider) -> u32 {
        match provider {
            Provider::Codex => self.archive_after_inactive_days,
            Provider::Claude => self.archive_after_inactive_days_claude,
        }
    }

    /// Parse a settings file. A file without `[transcripts]` is the default
    /// (enabled). The `archive_after_inactive_days*` names win over the
    /// older `archive_after_days*` names whatever their order.
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
        let mut legacy_days = None;
        let mut legacy_days_claude = None;
        let mut inactive_days = None;
        let mut inactive_days_claude = None;
        for (key, value) in section {
            match key.as_str() {
                "archive" => {
                    config.archive = value
                        .as_bool()
                        .ok_or("transcripts.archive must be true or false")?;
                    config.enabled_source = EnabledSource::Settings;
                }
                "archive_after_days" => legacy_days = Some(days(key, value)?),
                "archive_after_days_claude" => legacy_days_claude = Some(days(key, value)?),
                "archive_after_inactive_days" => inactive_days = Some(days(key, value)?),
                "archive_after_inactive_days_claude" => {
                    inactive_days_claude = Some(days(key, value)?);
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
        if let Some(days) = inactive_days.or(legacy_days) {
            config.archive_after_inactive_days = days;
        }
        if let Some(days) = inactive_days_claude.or(legacy_days_claude) {
            config.archive_after_inactive_days_claude = days;
        }
        Ok(config)
    }

    /// The host's settings; the default (on) when the file does not exist.
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
    /// Written as `archive_after_inactive_days` (the older
    /// `archive_after_days` key is then removed so one value remains).
    pub archive_after_inactive_days: Option<u32>,
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
    if let Some(days) = change.archive_after_inactive_days {
        section.remove("archive_after_days");
        section.insert(
            "archive_after_inactive_days",
            toml_edit::value(i64::from(days)),
        );
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
