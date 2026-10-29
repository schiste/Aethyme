//! One loader for the human-authored `.aethyme` configuration files (#1223).
//!
//! Every configuration stem (`config`, `gates`, `prepare`, `broker`,
//! `environments`, and host config) may be written as `<stem>.toml`,
//! `<stem>.yaml` or `<stem>.yml`. Exactly one may exist: two present is an
//! error naming both, because a stale twin silently shadowing the real policy
//! is exactly the confusion the broker exists to prevent.
//!
//! YAML is parsed with a strict YAML 1.2 subset and converted into the same
//! [`toml::Value`] the subsystems already consume, so no subsystem's
//! extraction logic changes with the format. Both formats keep a key path to
//! `line:col` map, so an error a subsystem raises against a key path is
//! reported as `file:line:col (key.path)` either way.
//!
//! The strict YAML subset:
//!
//! - YAML 1.2 core schema: `on`, `yes` and `no` are strings, `010` is decimal;
//! - duplicate keys are an error;
//! - `null` (`~`, `null`, an empty value) is an error: TOML has no null, an
//!   omitted key means "unset";
//! - tags (`!foo`, `!!str`) and merge keys (`<<`) are refused;
//! - anchors and aliases are allowed, under an expansion cap;
//! - keys must be strings;
//! - integers outside `i64` are refused, the same range as TOML;
//! - only one document: multi-document streams are refused.

mod locate;
mod yaml;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

pub use locate::Location;

/// Directory, relative to a checkout root, holding the configuration files.
pub const CONFIG_DIR: &str = ".aethyme";

/// The human-authored configuration stems this loader serves. Generated JSON
/// (`.aethyme/generated/*.json`, `.aethyme/overrides/*.json`) is out of scope.
pub const CONFIG_STEMS: &[&str] = &["config", "gates", "prepare", "broker", "environments"];

/// Accepted file extensions, in the order candidates are reported.
pub const CONFIG_EXTENSIONS: [&str; 3] = ["toml", "yaml", "yml"];

/// Cap on how many times one YAML anchor may be expanded. Profiles that share
/// fragments stay far below it; an alias bomb trips it in milliseconds.
pub const YAML_ALIAS_EXPANSION_CAP: usize = 256;

/// Cap on the total number of YAML nodes after alias expansion.
pub const YAML_NODE_CAP: usize = 100_000;

/// The source format of a configuration file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConfigFormat {
    Toml,
    Yaml,
}

impl ConfigFormat {
    /// The format a file extension selects, if it is a configuration one.
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "toml" => Some(Self::Toml),
            "yaml" | "yml" => Some(Self::Yaml),
            _ => None,
        }
    }

    /// The format a path's extension selects.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        Self::from_extension(path.extension()?.to_str()?)
    }

    /// Stable lowercase name (`toml` or `yaml`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toml => "toml",
            Self::Yaml => "yaml",
        }
    }

    /// The extension a newly written file of this format uses.
    #[must_use]
    pub fn extension(self) -> &'static str {
        self.as_str()
    }
}

impl fmt::Display for ConfigFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The candidate file names for a stem: `<stem>.toml`, `<stem>.yaml`,
/// `<stem>.yml`.
#[must_use]
pub fn candidate_file_names(stem: &str) -> [String; 3] {
    CONFIG_EXTENSIONS.map(|extension| format!("{stem}.{extension}"))
}

/// The candidate repository-relative paths for a stem under [`CONFIG_DIR`],
/// for example `.aethyme/gates.toml`, `.aethyme/gates.yaml`,
/// `.aethyme/gates.yml`.
#[must_use]
pub fn candidate_relpaths(stem: &str) -> [String; 3] {
    CONFIG_EXTENSIONS.map(|extension| format!("{CONFIG_DIR}/{stem}.{extension}"))
}

/// The repository-relative path a stem is written to in `format`.
#[must_use]
pub fn relpath_for(stem: &str, format: ConfigFormat) -> String {
    format!("{CONFIG_DIR}/{stem}.{}", format.extension())
}

/// When `relpath` is one of a stem's configuration twins
/// (`.aethyme/<stem>.{toml,yaml,yml}`), the stem it configures.
#[must_use]
pub fn config_stem_of(relpath: &str) -> Option<&'static str> {
    let file = relpath.strip_prefix(CONFIG_DIR)?.strip_prefix('/')?;
    let (stem, extension) = file.rsplit_once('.')?;
    ConfigFormat::from_extension(extension)?;
    CONFIG_STEMS
        .iter()
        .copied()
        .find(|candidate| *candidate == stem)
}

/// Whether `relpath` is any twin of `stem`.
#[must_use]
pub fn is_config_twin(relpath: &str, stem: &str) -> bool {
    config_stem_of(relpath) == Some(stem)
        || candidate_relpaths(stem)
            .iter()
            .any(|candidate| candidate == relpath)
}

/// Whether any twin of `stem` exists as a file in `dir`.
#[must_use]
pub fn config_exists(dir: &Path, stem: &str) -> bool {
    candidate_file_names(stem)
        .iter()
        .any(|name| dir.join(name).is_file())
}

/// The twins of `stem` present as files in `dir`.
#[must_use]
pub fn present_config_paths(dir: &Path, stem: &str) -> Vec<PathBuf> {
    candidate_file_names(stem)
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| path.is_file())
        .collect()
}

/// The single configuration file for `stem` in `dir`, if any. Two twins
/// present is [`ConfigSourceError::Ambiguous`].
pub fn resolve_config_path(dir: &Path, stem: &str) -> Result<Option<PathBuf>, ConfigSourceError> {
    let mut present = present_config_paths(dir, stem);
    match present.len() {
        0 => Ok(None),
        1 => Ok(present.pop()),
        _ => Err(ConfigSourceError::Ambiguous {
            stem: stem.to_string(),
            paths: present,
        }),
    }
}

/// Like [`resolve_config_path`], but when nothing exists returns the path a
/// new file of the default (TOML) format would take. Useful for messages and
/// for writers that create the file.
pub fn config_path_or_default(dir: &Path, stem: &str) -> Result<PathBuf, ConfigSourceError> {
    Ok(resolve_config_path(dir, stem)?.unwrap_or_else(|| dir.join(format!("{stem}.toml"))))
}

/// Raw configuration text with its resolved path and format, before parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigText {
    pub path: PathBuf,
    pub format: ConfigFormat,
    pub text: String,
}

impl ConfigText {
    /// Parse the text in its format.
    pub fn parse(&self) -> Result<ConfigDocument, ConfigSourceError> {
        parse_config_text(&self.path, self.format, &self.text)
    }

    /// The configuration as TOML text, for subsystems that parse text: TOML
    /// passes through byte-for-byte (so its own diagnostics keep their
    /// positions); YAML is validated against the strict subset and rendered
    /// from the same `toml::Value`, so it parses back to an identical value.
    pub fn toml_text(&self) -> Result<std::borrow::Cow<'_, str>, ConfigSourceError> {
        match self.format {
            ConfigFormat::Toml => Ok(std::borrow::Cow::Borrowed(&self.text)),
            ConfigFormat::Yaml => Ok(std::borrow::Cow::Owned(self.parse()?.to_toml_string()?)),
        }
    }

    /// Canonical bytes of the normalised value (see
    /// [`ConfigDocument::normalized_bytes`]).
    pub fn normalized_bytes(&self) -> Result<Vec<u8>, ConfigSourceError> {
        Ok(self.parse()?.normalized_bytes())
    }
}

/// Read the single configuration file for `stem` from `dir` without parsing.
pub fn read_config_text(dir: &Path, stem: &str) -> Result<Option<ConfigText>, ConfigSourceError> {
    let Some(path) = resolve_config_path(dir, stem)? else {
        return Ok(None);
    };
    let format = ConfigFormat::from_path(&path).unwrap_or(ConfigFormat::Toml);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(ConfigText { path, format, text })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ConfigSourceError::Read {
            path,
            message: error.to_string(),
        }),
    }
}

/// The single configuration file for `stem` in `dir` as TOML text (see
/// [`ConfigText::toml_text`]), with the path it was read from.
pub fn read_config_toml(
    dir: &Path,
    stem: &str,
) -> Result<Option<(PathBuf, String)>, ConfigSourceError> {
    let Some(text) = read_config_text(dir, stem)? else {
        return Ok(None);
    };
    let toml = text.toml_text()?.into_owned();
    Ok(Some((text.path, toml)))
}

/// Load and parse the single configuration file for `stem` from `dir`
/// (normally `<checkout>/.aethyme`). `Ok(None)` when no twin exists.
pub fn load_config(dir: &Path, stem: &str) -> Result<Option<ConfigDocument>, ConfigSourceError> {
    read_config_text(dir, stem)?
        .map(|text| text.parse())
        .transpose()
}

/// Read the configuration text for `stem` through `read`, which receives each
/// candidate path (`<dir_label>/<stem>.<ext>`, for example
/// `.aethyme/config.yaml`) and returns its content when present. This is the
/// entry point for reads that are not plain filesystem reads, such as a file
/// at a Git commit.
pub fn read_config_text_with<E, F>(
    dir_label: &str,
    stem: &str,
    read: F,
) -> Result<Option<ConfigText>, E>
where
    E: From<ConfigSourceError>,
    F: FnMut(&str) -> Result<Option<String>, E>,
{
    try_read_config_text_with(dir_label, stem, read)?.map_err(E::from)
}

/// [`read_config_text_with`] for readers whose error type does not convert
/// from [`ConfigSourceError`]: the outer result is the reader's, the inner
/// one the resolution's.
pub fn try_read_config_text_with<E, F>(
    dir_label: &str,
    stem: &str,
    mut read: F,
) -> Result<Result<Option<ConfigText>, ConfigSourceError>, E>
where
    F: FnMut(&str) -> Result<Option<String>, E>,
{
    let mut found = Vec::new();
    for name in candidate_file_names(stem) {
        let relpath = if dir_label.is_empty() {
            name
        } else {
            format!("{}/{name}", dir_label.trim_end_matches('/'))
        };
        if let Some(text) = read(&relpath)? {
            found.push((relpath, text));
        }
    }
    if found.len() > 1 {
        return Ok(Err(ConfigSourceError::Ambiguous {
            stem: stem.to_string(),
            paths: found
                .into_iter()
                .map(|(path, _)| PathBuf::from(path))
                .collect(),
        }));
    }
    Ok(Ok(found.pop().map(|(relpath, text)| {
        let path = PathBuf::from(relpath);
        let format = ConfigFormat::from_path(&path).unwrap_or(ConfigFormat::Toml);
        ConfigText { path, format, text }
    })))
}

/// [`read_config_text_with`] followed by parsing.
pub fn load_config_with<E, F>(
    dir_label: &str,
    stem: &str,
    read: F,
) -> Result<Option<ConfigDocument>, E>
where
    E: From<ConfigSourceError>,
    F: FnMut(&str) -> Result<Option<String>, E>,
{
    match read_config_text_with(dir_label, stem, read)? {
        Some(text) => Ok(Some(text.parse()?)),
        None => Ok(None),
    }
}

/// Parse configuration text in `format`. `path` is used for diagnostics only.
pub fn parse_config_text(
    path: impl AsRef<Path>,
    format: ConfigFormat,
    text: &str,
) -> Result<ConfigDocument, ConfigSourceError> {
    let path = path.as_ref().to_path_buf();
    let (value, spans) = match format {
        ConfigFormat::Toml => locate::parse_toml(&path, text)?,
        ConfigFormat::Yaml => yaml::parse_yaml(&path, text)?,
    };
    Ok(ConfigDocument {
        format,
        path,
        value,
        spans,
    })
}

/// Parse configuration text, choosing the format from `path`'s extension
/// (TOML when the extension is not a configuration one).
pub fn parse_config_text_at(
    path: impl AsRef<Path>,
    text: &str,
) -> Result<ConfigDocument, ConfigSourceError> {
    let path = path.as_ref();
    parse_config_text(
        path,
        ConfigFormat::from_path(path).unwrap_or(ConfigFormat::Toml),
        text,
    )
}

/// Key path to source location. Paths use dotted keys with `[i]` for array
/// items, for example `review.rules[0].name`; keys that are not bare TOML
/// keys are double-quoted, as TOML prints them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigSpans {
    locations: BTreeMap<String, Location>,
}

impl ConfigSpans {
    pub(crate) fn insert(&mut self, key_path: String, location: Location) {
        self.locations.entry(key_path).or_insert(location);
    }

    /// The location of `key_path`. A path without array indices (the form
    /// serde errors print) matches the first indexed path it abbreviates.
    #[must_use]
    pub fn get(&self, key_path: &str) -> Option<Location> {
        if let Some(location) = self.locations.get(key_path) {
            return Some(*location);
        }
        self.locations
            .iter()
            .filter(|(candidate, _)| strip_indices(candidate) == key_path)
            .map(|(_, location)| *location)
            .min()
    }

    /// Number of recorded paths.
    #[must_use]
    pub fn len(&self) -> usize {
        self.locations.len()
    }

    /// Whether no path was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }
}

fn strip_indices(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_index = false;
    let mut in_quote = false;
    for character in path.chars() {
        match character {
            '"' if !in_index => {
                in_quote = !in_quote;
                out.push(character);
            }
            '[' if !in_quote => in_index = true,
            ']' if in_index => in_index = false,
            _ if in_index => {}
            _ => out.push(character),
        }
    }
    out
}

/// Append a key segment to a display key path.
#[must_use]
pub fn join_key_path(parent: &str, key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let segment = if bare {
        key.to_string()
    } else {
        format!("\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\""))
    };
    if parent.is_empty() {
        segment
    } else {
        format!("{parent}.{segment}")
    }
}

/// Append an array index to a display key path.
#[must_use]
pub fn join_index_path(parent: &str, index: usize) -> String {
    format!("{parent}[{index}]")
}

/// A parsed configuration file in either format.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigDocument {
    pub format: ConfigFormat,
    pub path: PathBuf,
    /// The configuration as the value model every subsystem consumes.
    pub value: toml::Value,
    pub spans: ConfigSpans,
}

impl ConfigDocument {
    /// The top-level table. Parsing guarantees the top level is a table.
    #[must_use]
    pub fn table(&self) -> &toml::Table {
        self.value
            .as_table()
            .expect("configuration documents are always tables")
    }

    /// The source location of `key_path`, when known.
    #[must_use]
    pub fn location(&self, key_path: &str) -> Option<Location> {
        self.spans.get(key_path)
    }

    /// An error located at `key_path`, reported as
    /// `file:line:col (key.path): message`.
    #[must_use]
    pub fn error_at(&self, key_path: &str, message: impl Into<String>) -> ConfigSourceError {
        ConfigSourceError::Invalid {
            path: self.path.clone(),
            location: self.location(key_path),
            key_path: (!key_path.is_empty()).then(|| key_path.to_string()),
            code: "config_invalid",
            message: message.into(),
        }
    }

    /// Deserialize the whole document into `T`. A type error names its key
    /// path and that path's `file:line:col`, in either format.
    pub fn deserialize<T: serde::de::DeserializeOwned>(&self) -> Result<T, ConfigSourceError> {
        self.deserialize_value(&self.value, "")
    }

    /// Deserialize the value at `key_path` (one top-level key, or a dotted
    /// path) into `T`; `Ok(None)` when absent.
    pub fn deserialize_at<T: serde::de::DeserializeOwned>(
        &self,
        key_path: &[&str],
    ) -> Result<Option<T>, ConfigSourceError> {
        let mut value = &self.value;
        let mut display = String::new();
        for key in key_path {
            match value.get(key) {
                Some(next) => value = next,
                None => return Ok(None),
            }
            display = join_key_path(&display, key);
        }
        self.deserialize_value(value, &display).map(Some)
    }

    fn deserialize_value<T: serde::de::DeserializeOwned>(
        &self,
        value: &toml::Value,
        prefix: &str,
    ) -> Result<T, ConfigSourceError> {
        value.clone().try_into::<T>().map_err(|error| {
            let rendered = error.to_string();
            let (message, key) = match rendered.split_once("\nin `") {
                Some((message, rest)) => (
                    message.trim().to_string(),
                    rest.trim_end().trim_end_matches('`').to_string(),
                ),
                None => (rendered.trim().to_string(), String::new()),
            };
            let key = match (prefix.is_empty(), key.is_empty()) {
                (true, _) => key,
                (false, true) => prefix.to_string(),
                (false, false) => format!("{prefix}.{key}"),
            };
            let mut error = self.error_at(&key, message);
            if let ConfigSourceError::Invalid { code, .. } = &mut error {
                *code = "config_type";
            }
            error
        })
    }

    /// Canonical bytes of the normalised value: identical for a TOML file and
    /// its YAML translation, so digests over configuration content do not
    /// change when only the format does.
    #[must_use]
    pub fn normalized_bytes(&self) -> Vec<u8> {
        normalized_bytes(&self.value)
    }

    /// The document rendered as TOML text (for consumers that still take
    /// text). Comments and formatting are not preserved.
    pub fn to_toml_string(&self) -> Result<String, ConfigSourceError> {
        // Fail closed: a value TOML cannot represent is refused, never
        // rendered as an empty (policy-free) document.
        toml::to_string(&self.value).map_err(|error| ConfigSourceError::Invalid {
            path: self.path.clone(),
            location: None,
            key_path: None,
            code: "config_unrepresentable",
            message: format!("the configuration cannot be represented as TOML: {error}"),
        })
    }
}

/// Canonical, format-independent bytes for a configuration value: tables with
/// sorted keys, arrays in order, scalars tagged by type.
#[must_use]
pub fn normalized_bytes(value: &toml::Value) -> Vec<u8> {
    let mut out = String::from("aethyme-config-normalized/v1\n");
    write_normalized(value, &mut out);
    out.into_bytes()
}

fn write_normalized(value: &toml::Value, out: &mut String) {
    use std::fmt::Write as _;
    match value {
        toml::Value::String(text) => {
            let _ = write!(out, "s{}:{text}", text.len());
        }
        toml::Value::Integer(number) => {
            let _ = write!(out, "i{number};");
        }
        toml::Value::Float(number) => {
            let _ = write!(out, "f{:016x};", number.to_bits());
        }
        toml::Value::Boolean(flag) => {
            let _ = write!(out, "b{};", u8::from(*flag));
        }
        toml::Value::Datetime(datetime) => {
            let text = datetime.to_string();
            let _ = write!(out, "d{}:{text}", text.len());
        }
        toml::Value::Array(items) => {
            let _ = write!(out, "a{}[", items.len());
            for item in items {
                write_normalized(item, out);
            }
            out.push(']');
        }
        toml::Value::Table(table) => {
            let mut keys: Vec<&String> = table.keys().collect();
            keys.sort();
            let _ = write!(out, "t{}{{", keys.len());
            for key in keys {
                let _ = write!(out, "k{}:{key}", key.len());
                write_normalized(&table[key.as_str()], out);
            }
            out.push('}');
        }
    }
}

/// Why a configuration file could not be resolved, read or parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigSourceError {
    /// More than one twin of a stem exists.
    Ambiguous { stem: String, paths: Vec<PathBuf> },
    /// The resolved file exists but could not be read.
    Read { path: PathBuf, message: String },
    /// The file is not valid configuration.
    Invalid {
        path: PathBuf,
        location: Option<Location>,
        key_path: Option<String>,
        /// Stable machine code, for example `yaml_null` or `yaml_duplicate_key`.
        code: &'static str,
        message: String,
    },
}

impl ConfigSourceError {
    /// Stable machine code for the error.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Ambiguous { .. } => "config_ambiguous_format",
            Self::Read { .. } => "config_read_failed",
            Self::Invalid { code, .. } => code,
        }
    }

    /// The file the error is about (the first twin for an ambiguity).
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Ambiguous { paths, .. } => paths.first().map(PathBuf::as_path),
            Self::Read { path, .. } | Self::Invalid { path, .. } => Some(path),
        }
    }
}

impl fmt::Display for ConfigSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ambiguous { stem, paths } => {
                let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                write!(
                    f,
                    "{} {} exist for the `{stem}` configuration; keep exactly one \
                     (Aethyme never silently prefers one format over another)",
                    names.join(" and "),
                    if names.len() == 2 { "both" } else { "all" }
                )
            }
            Self::Read { path, message } => write!(f, "read {}: {message}", path.display()),
            Self::Invalid {
                path,
                location,
                key_path,
                message,
                ..
            } => {
                write!(f, "{}", path.display())?;
                if let Some(location) = location {
                    write!(f, ":{}:{}", location.line, location.column)?;
                }
                if let Some(key_path) = key_path {
                    write!(f, " ({key_path})")?;
                }
                write!(f, ": {message}")
            }
        }
    }
}

impl std::error::Error for ConfigSourceError {}

#[cfg(test)]
mod tests;
