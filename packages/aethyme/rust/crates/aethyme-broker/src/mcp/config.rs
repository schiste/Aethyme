//! MCP service entries: `<host state>/mcp/services.toml` for the host and an
//! optional `.aethyme/mcp.toml` per repository.
//!
//! ```toml
//! schema = 1
//!
//! [[servers]]
//! name = "example-server"
//! mode = "shared"              # or "direct" (the default): one per agent
//! transport = "stdio"          # or "http"
//! argv = ["pnpm", "exec", "example-server", "--stdio"]
//! cwd = "/srv/example"         # host file: absolute; repository file: relative to the main checkout
//! env_file = ".env.staging"    # relative to cwd; KEY=VALUE lines, never a shell
//! env_allow = ["PATH", "HOME"] # names passed through from the caller
//! source = "/srv/example/src"  # its content identity is part of the key
//! flavour = "staging"
//! ```
//!
//! Every path must name a stable copy. An entry whose `cwd`, `source`,
//! `env_file` or absolute `argv` element lies inside a broker session worktree
//! is refused: sessions are cleaned up, and the configuration would break with
//! them. Run from a session worktree, `connect` finds that worktree's copy by
//! itself (see [`super::key`]).
//!
//! `argv` is visible to every local user in `ps`; keep credentials in
//! `env_file`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::checkouts::{Checkouts, canonical_or_lexical};

/// The host file, relative to the host state directory.
pub const HOST_FILE: &str = "mcp/services.toml";
/// The repository file, relative to a checkout's top level.
pub const REPOSITORY_FILE: &str = ".aethyme/mcp.toml";
/// The only schema this build reads.
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServicesFile {
    schema: u32,
    #[serde(default)]
    servers: Vec<RawEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    name: String,
    #[serde(default)]
    mode: Mode,
    #[serde(default)]
    transport: Transport,
    argv: Vec<String>,
    cwd: Option<String>,
    env_file: Option<String>,
    #[serde(default)]
    env_allow: Vec<String>,
    source: Option<String>,
    flavour: Option<String>,
}

/// Whether agents may share one server (`shared`, through the hub once it
/// exists) or each get their own (`direct`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Shared,
    #[default]
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Stdio,
    Http,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Host,
    Repository,
}

/// The repository a repository-scoped entry belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryAnchor {
    /// The main checkout: the stable copy every relative path is anchored to.
    pub main_root: PathBuf,
    /// Host-unique repository identity (the broker's repository key).
    pub identity: String,
}

/// A validated entry. Every path is absolute; `argv[0]` is resolved later,
/// against the caller's `PATH`, by [`super::resolve`].
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub scope: Scope,
    pub mode: Mode,
    pub transport: Transport,
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env_file: Option<PathBuf>,
    pub env_allow: Vec<String>,
    pub source: Option<PathBuf>,
    pub flavour: String,
    pub repository: Option<RepositoryAnchor>,
    pub config_path: PathBuf,
}

/// One refused file or entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigError {
    pub file: PathBuf,
    pub entry: Option<String>,
    pub field: Option<&'static str>,
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.file.display())?;
        if let Some(entry) = &self.entry {
            write!(f, ": [{entry}]")?;
        }
        if let Some(field) = self.field {
            write!(f, " {field}")?;
        }
        write!(f, ": {} ({})", self.message, self.code)
    }
}

/// A file that was looked for, and whether it was found.
#[derive(Debug, Clone, Serialize)]
pub struct FileStatus {
    pub scope: Scope,
    pub path: PathBuf,
    pub present: bool,
}

/// Everything loaded for one caller: valid entries, refusals, and the files.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Loaded {
    pub entries: Vec<Entry>,
    pub errors: Vec<ConfigError>,
    pub files: Vec<FileStatus>,
}

impl Loaded {
    /// The entry called `name`: the repository's when both scopes define it.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.name == name && entry.scope == Scope::Repository)
            .or_else(|| self.entries.iter().find(|entry| entry.name == name))
    }

    /// Refusals that concern `name` (or whole files).
    pub fn errors_for<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a ConfigError> {
        self.errors
            .iter()
            .filter(move |error| error.entry.as_deref().is_none_or(|entry| entry == name))
    }
}

/// Where the files are, for one caller.
pub struct Sources {
    /// `<host state>/mcp/services.toml`, when the host state directory exists.
    pub host_file: Option<PathBuf>,
    /// The caller's checkout (`.aethyme/mcp.toml` is read from it) with its
    /// repository anchor, when the caller is inside one.
    pub repository: Option<(PathBuf, RepositoryAnchor)>,
}

impl Sources {
    /// The real host and the checkout containing `caller_cwd`.
    pub fn discover(caller_cwd: &Path) -> Self {
        let host_file = crate::host_state::default_host_state_dir().map(|dir| dir.join(HOST_FILE));
        let repository = crate::GitRepo::discover(caller_cwd)
            .ok()
            .and_then(|repository| {
                let checkout = repository.root().to_path_buf();
                let common_dir = repository.git_common_dir().ok()?;
                let main_root = repository.main_root().ok()?;
                let identity = crate::host_state::repository_key(&main_root, Some(&common_dir));
                Some((
                    checkout,
                    RepositoryAnchor {
                        main_root: canonical_or_lexical(&main_root),
                        identity,
                    },
                ))
            });
        Self {
            host_file,
            repository,
        }
    }
}

/// Read and validate both files.
pub fn load(sources: &Sources, checkouts: &dyn Checkouts) -> Loaded {
    let mut loaded = Loaded::default();
    if let Some(path) = &sources.host_file {
        load_file(&mut loaded, Scope::Host, path, None, checkouts);
    }
    if let Some((checkout, anchor)) = &sources.repository {
        let path = checkout.join(REPOSITORY_FILE);
        load_file(
            &mut loaded,
            Scope::Repository,
            &path,
            Some(anchor),
            checkouts,
        );
    }
    loaded
}

fn load_file(
    loaded: &mut Loaded,
    scope: Scope,
    path: &Path,
    anchor: Option<&RepositoryAnchor>,
    checkouts: &dyn Checkouts,
) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            loaded.files.push(FileStatus {
                scope,
                path: path.to_path_buf(),
                present: false,
            });
            return;
        }
        Err(error) => {
            loaded.files.push(FileStatus {
                scope,
                path: path.to_path_buf(),
                present: true,
            });
            loaded
                .errors
                .push(file_error(path, "unreadable", error.to_string()));
            return;
        }
    };
    loaded.files.push(FileStatus {
        scope,
        path: path.to_path_buf(),
        present: true,
    });
    let (entries, errors) = parse_and_validate(&text, path, scope, anchor, checkouts);
    loaded.entries.extend(entries);
    loaded.errors.extend(errors);
}

fn file_error(path: &Path, code: &'static str, message: String) -> ConfigError {
    ConfigError {
        file: path.to_path_buf(),
        entry: None,
        field: None,
        code,
        message,
    }
}

/// Parse one file and validate each entry. Invalid entries are left out and
/// reported; valid ones are kept, so one bad entry does not disable the rest.
pub fn parse_and_validate(
    text: &str,
    path: &Path,
    scope: Scope,
    anchor: Option<&RepositoryAnchor>,
    checkouts: &dyn Checkouts,
) -> (Vec<Entry>, Vec<ConfigError>) {
    let file: ServicesFile = match toml::from_str(text) {
        Ok(file) => file,
        Err(error) => {
            let line = error
                .span()
                .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1);
            let message = match line {
                Some(line) => format!("line {line}: {}", error.message()),
                None => error.message().to_string(),
            };
            return (Vec::new(), vec![file_error(path, "invalid_toml", message)]);
        }
    };
    if file.schema != SCHEMA {
        return (
            Vec::new(),
            vec![file_error(
                path,
                "unsupported_schema",
                format!(
                    "schema {} is not supported; this build reads schema {SCHEMA}",
                    file.schema
                ),
            )],
        );
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut errors = Vec::new();
    for raw in file.servers {
        if entries.iter().any(|entry| entry.name == raw.name) {
            errors.push(ConfigError {
                file: path.to_path_buf(),
                entry: Some(raw.name.clone()),
                field: Some("name"),
                code: "duplicate_name",
                message: "another entry in this file has the same name".into(),
            });
            continue;
        }
        match validate_entry(raw, path, scope, anchor, checkouts) {
            Ok(entry) => entries.push(entry),
            Err(error) => errors.push(*error),
        }
    }
    (entries, errors)
}

fn validate_entry(
    raw: RawEntry,
    path: &Path,
    scope: Scope,
    anchor: Option<&RepositoryAnchor>,
    checkouts: &dyn Checkouts,
) -> Result<Entry, Box<ConfigError>> {
    let name = raw.name.clone();
    let refuse = |field: &'static str, code: &'static str, message: String| {
        Box::new(ConfigError {
            file: path.to_path_buf(),
            entry: Some(name.clone()),
            field: Some(field),
            code,
            message,
        })
    };
    let name_ok = !raw.name.is_empty()
        && raw.name.len() <= 64
        && raw
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !name_ok {
        return Err(refuse(
            "name",
            "invalid_name",
            "use 1-64 characters from A-Z, a-z, 0-9, '-', '_' and '.'".into(),
        ));
    }
    if raw.argv.is_empty() || raw.argv[0].is_empty() {
        return Err(refuse(
            "argv",
            "empty_argv",
            "argv needs at least the program".into(),
        ));
    }
    let base = match (scope, anchor) {
        (Scope::Repository, Some(anchor)) => Some(anchor.main_root.as_path()),
        _ => None,
    };
    let anchored = |field: &'static str, value: &str, relative_to: Option<&Path>| {
        let candidate = PathBuf::from(value);
        if candidate.is_absolute() {
            return Ok(canonical_or_lexical(&candidate));
        }
        match relative_to {
            Some(base) => Ok(canonical_or_lexical(&base.join(candidate))),
            None => Err(refuse(
                field,
                "relative_path",
                format!(
                    "{value:?} is relative; host entries need absolute paths (`~` is not expanded)"
                ),
            )),
        }
    };
    let cwd = raw
        .cwd
        .as_deref()
        .map(|value| anchored("cwd", value, base))
        .transpose()?;
    let env_file = raw
        .env_file
        .as_deref()
        .map(|value| anchored("env_file", value, cwd.as_deref().or(base)))
        .transpose()?;
    let source = raw
        .source
        .as_deref()
        .map(|value| anchored("source", value, cwd.as_deref().or(base)))
        .transpose()?;
    for variable in &raw.env_allow {
        if !super::envfile::valid_name(variable) {
            return Err(refuse(
                "env_allow",
                "invalid_env_name",
                format!("{variable:?} is not an environment variable name"),
            ));
        }
    }
    let flavour = raw.flavour.clone().unwrap_or_default();
    if flavour.len() > 64 || flavour.chars().any(char::is_control) {
        return Err(refuse(
            "flavour",
            "invalid_flavour",
            "use at most 64 printable characters".into(),
        ));
    }
    if let Some(path) = &source
        && !path.exists()
    {
        return Err(refuse(
            "source",
            "missing_source",
            format!("{} does not exist", path.display()),
        ));
    }

    // Stable copies only: a session worktree disappears with its session.
    let mut stable_paths: Vec<(&'static str, PathBuf)> = Vec::new();
    stable_paths.extend(cwd.clone().map(|path| ("cwd", path)));
    stable_paths.extend(source.clone().map(|path| ("source", path)));
    stable_paths.extend(env_file.clone().map(|path| ("env_file", path)));
    stable_paths.extend(
        raw.argv
            .iter()
            .filter(|element| Path::new(element).is_absolute())
            .map(|element| ("argv", PathBuf::from(element))),
    );
    for (field, path) in stable_paths {
        if let Some(worktree) = checkouts.session_worktree(&path) {
            return Err(refuse(
                field,
                "session_worktree_path",
                format!(
                    "{} is inside the broker session worktree {}; sessions are cleaned up when \
                     they finish, so the entry would break. Point it at a stable copy (the main \
                     checkout or another directory no session owns); `aethyme mcp connect` run \
                     from a session worktree picks that worktree's copy by itself",
                    path.display(),
                    worktree.root.display()
                ),
            ));
        }
    }

    Ok(Entry {
        name: raw.name,
        scope,
        mode: raw.mode,
        transport: raw.transport,
        argv: raw.argv,
        cwd,
        env_file,
        env_allow: raw.env_allow,
        source,
        flavour,
        repository: match scope {
            Scope::Repository => anchor.cloned(),
            Scope::Host => None,
        },
        config_path: path.to_path_buf(),
    })
}
