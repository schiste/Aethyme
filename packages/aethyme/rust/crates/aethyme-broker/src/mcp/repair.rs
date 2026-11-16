//! `aethyme mcp repair`: point agent configuration entries for managed
//! servers at `aethyme mcp connect`.
//!
//! It scans the Codex TOML configuration (`$CODEX_HOME/config.toml`, else
//! `~/.codex/config.toml`), the Claude JSON user configuration
//! (`~/.claude.json`, its top-level `mcpServers` and each project's
//! `mcpServers`) and, run inside a repository, that repository's
//! `.mcp.json`, for stdio server entries whose command matches a managed
//! entry ([`super::managed::MatchSet::managed_match`]) and is not already
//! `aethyme mcp connect`. `--apply` rewrites only those entries to
//! `aethyme mcp connect <name> --fallback -- <original command and args>`,
//! keeping every other key (the `env` map included) untouched.
//!
//! These files hold credentials, and one of them is rewritten often by its
//! own application, so writing is careful:
//! - nothing from the files is printed, logged or put in `--json` except
//!   server names and the command's program basename; a parse error names a
//!   line or byte offset only;
//! - a file that does not parse is reported and never written;
//! - formatting and key order are kept: TOML through a format-preserving
//!   editor, JSON by replacing only the edited values' byte ranges in the
//!   file's own indentation;
//! - before a write, the original goes to a 0600 backup under
//!   `<host state>/mcp/repair-backups/` (never beside the configuration),
//!   keeping the newest [`KEEP_BACKUPS`] per file;
//! - compare-and-swap: size, modification time and SHA-256 are recorded at
//!   read; the new content goes to a temporary file in the same directory
//!   with the original's mode, the original is checked unchanged right
//!   before the `rename`, and on a change the attempt is discarded and
//!   retried, at most [`ATTEMPTS`] times, then reported `contended` with
//!   nothing written;
//! - a symlinked configuration is followed only when its target is inside
//!   `HOME`;
//! - a repository's tracked `.mcp.json` is never written (that is a commit
//!   the owner makes): its matches are reported only.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::json_span::{self, Edit, Node};
use super::managed::{ManagedMatch, MatchSet, RepairMode, Route};
use super::resolve::is_executable_file;
use super::telemetry::{self, Event};

/// Attempts before a write is reported `contended`.
pub const ATTEMPTS: usize = 3;
/// Backups kept per configuration file.
pub const KEEP_BACKUPS: usize = 5;
/// Backups, under `<host state>/mcp/`.
pub const BACKUP_DIRECTORY: &str = "repair-backups";
/// Host settings, under `<host state>/mcp/`: `[mcp] repair_on_session_start`.
pub const SETTINGS_FILE: &str = "settings.toml";
/// The session-start repair's budget.
pub const SESSION_START_BUDGET: Duration = Duration::from_secs(2);

/// Which configuration a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// Codex: `[mcp_servers.<name>]` with `command` and `args`.
    CodexToml,
    /// Claude user configuration: `mcpServers`, and `projects.*.mcpServers`.
    ClaudeUser,
    /// A repository's `.mcp.json`: `mcpServers`.
    ProjectJson,
}

impl TargetKind {
    pub fn agent(self) -> &'static str {
        match self {
            TargetKind::CodexToml => "codex",
            TargetKind::ClaudeUser | TargetKind::ProjectJson => "claude",
        }
    }

    fn label(self) -> &'static str {
        match self {
            TargetKind::CodexToml => "codex",
            TargetKind::ClaudeUser => "claude",
            TargetKind::ProjectJson => "project",
        }
    }
}

/// One configuration file to scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Target {
    pub kind: TargetKind,
    pub path: PathBuf,
    /// Whether repair may write it (a tracked repository file never is).
    pub writable: bool,
    #[serde(skip)]
    pub not_writable_reason: Option<&'static str>,
}

/// Where the configurations are.
#[derive(Debug, Clone, Default)]
pub struct Locations {
    pub home: Option<PathBuf>,
    pub codex: Option<PathBuf>,
    pub claude: Option<PathBuf>,
    /// `(checkout root, .mcp.json)` when run inside a repository.
    pub project: Option<(PathBuf, PathBuf)>,
}

impl Locations {
    /// From the environment: `HOME`, `CODEX_HOME`, and the checkout holding
    /// `cwd`. `codex`/`claude` override the derived paths.
    pub fn discover(cwd: Option<&Path>, codex: Option<PathBuf>, claude: Option<PathBuf>) -> Self {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let codex = codex.or_else(|| {
            std::env::var_os("CODEX_HOME")
                .filter(|value| !value.is_empty())
                .map(|dir| PathBuf::from(dir).join("config.toml"))
                .or_else(|| home.as_ref().map(|home| home.join(".codex/config.toml")))
        });
        let claude = claude.or_else(|| home.as_ref().map(|home| home.join(".claude.json")));
        let project = cwd
            .and_then(super::config::repository_anchor)
            .map(|(checkout, _)| {
                let file = checkout.join(".mcp.json");
                (checkout, file)
            });
        Self {
            home,
            codex,
            claude,
            project,
        }
    }

    /// The targets for `agent` (`codex`, `claude`, or every one).
    pub fn targets(&self, agent: Option<&str>) -> Vec<Target> {
        let mut targets = Vec::new();
        let wants = |name: &str| agent.is_none_or(|agent| agent == name);
        if wants("codex")
            && let Some(path) = &self.codex
        {
            targets.push(Target {
                kind: TargetKind::CodexToml,
                path: path.clone(),
                writable: true,
                not_writable_reason: None,
            });
        }
        if wants("claude") {
            if let Some(path) = &self.claude {
                targets.push(Target {
                    kind: TargetKind::ClaudeUser,
                    path: path.clone(),
                    writable: true,
                    not_writable_reason: None,
                });
            }
            if let Some((checkout, path)) = &self.project
                && path.exists()
            {
                let tracked = is_tracked(checkout, ".mcp.json");
                targets.push(Target {
                    kind: TargetKind::ProjectJson,
                    path: path.clone(),
                    writable: !tracked,
                    not_writable_reason: tracked.then_some("tracked_repository_file"),
                });
            }
        }
        targets
    }
}

/// Whether `relative` is tracked in the checkout; `true` when Git cannot
/// tell (never write what might be a tracked file).
fn is_tracked(checkout: &Path, relative: &str) -> bool {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(checkout)
        .args(["ls-files", "--error-unmatch", "--", relative])
        .env("GIT_OPTIONAL_LOCKS", "0");
    match crate::bounded_output::output_limited(&mut command, Duration::from_secs(2), 64 * 1024) {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) => {
            output.status.success() || output.status.code() != Some(1)
        }
        _ => true,
    }
}

/// One matching server entry. Only names leave this module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub agent: &'static str,
    pub file: PathBuf,
    /// `user`, or `project #<n>` for the n-th project of a Claude user
    /// configuration (its key is a path from the file and is not shown).
    pub scope: String,
    pub server: String,
    /// The managed entry it matched.
    pub entry: String,
    /// The program's basename.
    pub program: String,
    /// `package` or `executable`, and its name (from the managed list).
    pub matched_kind: &'static str,
    pub matched: String,
    pub repair: RepairMode,
    /// `would_repair`, `repaired`, `report_only`, `contended`. `reason`
    /// says why an entry is only reported (`tracked_repository_file`,
    /// `repair_report`, `explicit_apply_only`, `interactive_arguments`,
    /// `arguments_differ`, `version_differs`, `arguments_not_suffix`) or
    /// that a connect entry's `aethyme` path is gone (`stale_aethyme_path`).
    pub state: &'static str,
    pub reason: Option<&'static str>,
    #[serde(skip)]
    location: Location,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Location {
    /// The Claude project key, when the entry is a project's.
    project: Option<String>,
    server: String,
    argv: Vec<String>,
    /// The `args` a repair writes (after the absolute `aethyme` command).
    new_args: Vec<String>,
}

/// What happened to one file.
#[derive(Debug, Clone, Serialize)]
pub struct FileReport {
    pub agent: &'static str,
    pub kind: TargetKind,
    pub file: PathBuf,
    /// `absent`, `scanned`, `repaired`, `parse_error`, `unreadable`,
    /// `refused_symlink`, `contended`, `write_failed`, `budget_exhausted`.
    pub state: &'static str,
    pub detail: Option<String>,
    pub writable: bool,
    pub servers: usize,
    pub already_managed: usize,
    pub findings: Vec<Finding>,
    pub backup: Option<PathBuf>,
    pub attempts: usize,
    pub phases_ms: BTreeMap<&'static str, u64>,
}

/// The whole run.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub mode: &'static str,
    pub auto: bool,
    pub managed_entries: usize,
    pub files: Vec<FileReport>,
    pub matched: usize,
    pub repaired: usize,
    pub phases_ms: BTreeMap<&'static str, u64>,
}

/// How to run.
#[derive(Debug, Clone)]
pub struct Options {
    pub apply: bool,
    /// Only entries with `repair = "auto"` are written.
    pub auto: bool,
    pub deadline: Option<Instant>,
    /// Backups go here (`<host state>/mcp/repair-backups`).
    pub backup_dir: PathBuf,
    /// The absolute `aethyme` a repaired entry runs (the stable install
    /// path, see [`super::intercept::aethyme_target`]).
    pub aethyme: PathBuf,
}

/// The most one file can take (three attempts with their backoff and
/// I/O); a file is not started with less than this left of a deadline.
pub const WORST_CASE_PER_FILE: Duration = Duration::from_millis(600);
/// Temporary files of an interrupted repair older than this are removed.
const STALE_TEMPORARY: Duration = Duration::from_secs(60);
const TEMPORARY_PREFIX: &str = ".aethyme-repair-";

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Scan, and with `options.apply` repair, every target.
pub fn run(set: &MatchSet, targets: &[Target], locations: &Locations, options: &Options) -> Report {
    run_with_hook(set, targets, locations, options, &mut |_, _| {})
}

/// [`run`] with a hook called right before each compare-and-swap check
/// (tests use it to change the file under the writer).
pub fn run_with_hook(
    set: &MatchSet,
    targets: &[Target],
    locations: &Locations,
    options: &Options,
    hook: &mut dyn FnMut(&Path, usize),
) -> Report {
    let started = Instant::now();
    let mut files = Vec::new();
    for target in targets {
        if options
            .deadline
            .is_some_and(|deadline| Instant::now() + WORST_CASE_PER_FILE > deadline)
        {
            files.push(empty_report(target, "budget_exhausted", None));
            continue;
        }
        files.push(process(set, target, locations, options, hook));
    }
    let matched = files.iter().map(|file| file.findings.len()).sum();
    let repaired = files
        .iter()
        .flat_map(|file| &file.findings)
        .filter(|finding| finding.state == "repaired")
        .count();
    let mut phases = BTreeMap::new();
    phases.insert("total", elapsed_ms(started));
    Report {
        mode: if options.apply { "apply" } else { "check" },
        auto: options.auto,
        managed_entries: set.entries.len(),
        files,
        matched,
        repaired,
        phases_ms: phases,
    }
}

fn empty_report(target: &Target, state: &'static str, detail: Option<String>) -> FileReport {
    FileReport {
        agent: target.kind.agent(),
        kind: target.kind,
        file: target.path.clone(),
        state,
        detail,
        writable: target.writable,
        servers: 0,
        already_managed: 0,
        findings: Vec::new(),
        backup: None,
        attempts: 0,
        phases_ms: BTreeMap::new(),
    }
}

/// A file's state at read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    size: u64,
    mtime_s: i64,
    mtime_nsec: i64,
    sha256: [u8; 32],
}

fn fingerprint(path: &Path) -> std::io::Result<(Vec<u8>, Fingerprint, u32)> {
    let bytes = std::fs::read(path)?;
    let metadata = std::fs::metadata(path)?;
    Ok((
        bytes.clone(),
        Fingerprint {
            size: metadata.len(),
            mtime_s: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            sha256: digest32(&bytes),
        },
        metadata.permissions().mode() & 0o7777,
    ))
}

fn digest32(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(bytes));
    out
}

/// The file to read and replace: the path itself, or a symlink's target
/// when that is inside `HOME`.
fn real_path(path: &Path, home: Option<&Path>) -> Result<PathBuf, &'static str> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "absent")?;
    if !metadata.file_type().is_symlink() {
        return Ok(path.to_path_buf());
    }
    let target = path.canonicalize().map_err(|_| "refused_symlink")?;
    let home = home
        .and_then(|home| home.canonicalize().ok())
        .ok_or("refused_symlink")?;
    if !target.starts_with(&home) || !target.is_file() {
        return Err("refused_symlink");
    }
    // A target inside a Git checkout (dotfiles repositories) would turn a
    // repair into an uncommitted change there.
    if target
        .ancestors()
        .skip(1)
        .any(|directory| directory.join(".git").exists())
    {
        return Err("refused_symlink_into_repository");
    }
    Ok(target)
}

/// Remove temporary files an interrupted repair left beside `real`.
fn sweep_temporaries(real: &Path) {
    let Some(directory) = real.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(TEMPORARY_PREFIX))
            && entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > STALE_TEMPORARY);
        if stale {
            crate::warn_unrecorded(
                "remove a stale repair temporary file",
                std::fs::remove_file(entry.path()),
            );
        }
    }
}

/// One server entry found in a file.
#[derive(Debug, Clone)]
struct Server {
    location: Location,
    scope: String,
}

fn servers_of(kind: TargetKind, text: &str) -> Result<Vec<Server>, String> {
    match kind {
        TargetKind::CodexToml => toml_servers(text),
        TargetKind::ClaudeUser | TargetKind::ProjectJson => json_servers(kind, text),
    }
}

fn toml_line(text: &str, error: &toml_edit::TomlError) -> String {
    match error.span() {
        Some(span) => format!(
            "invalid TOML at line {}",
            text[..span.start.min(text.len())].matches('\n').count() + 1
        ),
        None => "invalid TOML".into(),
    }
}

fn toml_servers(text: &str) -> Result<Vec<Server>, String> {
    let document: toml_edit::DocumentMut = text.parse().map_err(|error| toml_line(text, &error))?;
    let Some(servers) = document
        .get("mcp_servers")
        .and_then(toml_edit::Item::as_table_like)
    else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for (name, item) in servers.iter() {
        let Some(server) = item.as_table_like() else {
            continue;
        };
        let Some(command) = server.get("command").and_then(toml_edit::Item::as_str) else {
            continue;
        };
        let mut argv = vec![command.to_string()];
        if let Some(args) = server.get("args") {
            let Some(array) = args.as_array() else {
                continue;
            };
            let words: Option<Vec<String>> = array
                .iter()
                .map(|value| value.as_str().map(str::to_string))
                .collect();
            let Some(words) = words else {
                continue;
            };
            argv.extend(words);
        }
        found.push(Server {
            location: Location {
                project: None,
                server: name.to_string(),
                argv,
                new_args: Vec::new(),
            },
            scope: "user".into(),
        });
    }
    Ok(found)
}

fn json_document(text: &str) -> Result<Node, String> {
    // The span reader is strict JSON; serde_json double-checks validity
    // (its messages carry only a line and column).
    serde_json::from_str::<serde::de::IgnoredAny>(text)
        .map_err(|error| format!("invalid JSON at line {}", error.line()))?;
    json_span::parse(text).map_err(|error| format!("invalid JSON ({error})"))
}

/// The stdio server entries of an `mcpServers` object.
fn json_server_entries(map: &Node, project: Option<&str>, scope: &str, out: &mut Vec<Server>) {
    let members = map.members();
    for (index, member) in members.iter().enumerate() {
        // JSON readers keep the last of duplicate keys; so does repair.
        if members[index + 1..]
            .iter()
            .any(|later| later.key == member.key)
        {
            continue;
        }
        let server = &member.value;
        if server
            .get("type")
            .and_then(|member| member.value.as_str())
            .is_some_and(|kind| kind != "stdio")
        {
            continue;
        }
        let Some(command) = server
            .get("command")
            .and_then(|member| member.value.as_str())
        else {
            continue;
        };
        let mut argv = vec![command.to_string()];
        if let Some(args) = server.get("args") {
            let Some(words) = args.value.string_array() else {
                continue;
            };
            argv.extend(words);
        } else if command.contains(char::is_whitespace) {
            // A whole command line in `command`: read only when it is one
            // simple command.
            let Some(words) = super::managed::simple_command(command) else {
                continue;
            };
            argv = words;
        }
        out.push(Server {
            location: Location {
                project: project.map(str::to_string),
                server: member.key.clone(),
                argv,
                new_args: Vec::new(),
            },
            scope: scope.to_string(),
        });
    }
}

fn json_servers(kind: TargetKind, text: &str) -> Result<Vec<Server>, String> {
    let root = json_document(text)?;
    let mut found = Vec::new();
    if let Some(map) = root.get("mcpServers") {
        json_server_entries(&map.value, None, "user", &mut found);
    }
    if kind == TargetKind::ClaudeUser
        && let Some(projects) = root.get("projects")
    {
        for (index, project) in projects.value.members().iter().enumerate() {
            if let Some(map) = project.value.get("mcpServers") {
                json_server_entries(
                    &map.value,
                    Some(&project.key),
                    &format!("project #{}", index + 1),
                    &mut found,
                );
            }
        }
    }
    Ok(found)
}

fn program_basename(argv: &[String]) -> String {
    argv.first()
        .map(|program| program.rsplit('/').next().unwrap_or(program).to_string())
        .unwrap_or_default()
}

fn is_connect(argv: &[String]) -> bool {
    program_basename(argv) == "aethyme"
        && argv.get(1).map(String::as_str) == Some("mcp")
        && argv.get(2).map(String::as_str) == Some("connect")
}

/// The rewritten text: each change's `command` becomes `aethyme` and its
/// `args` the change's arguments; nothing else is touched.
fn rewrite(
    kind: TargetKind,
    text: &str,
    changes: &[Location],
    aethyme: &str,
) -> Result<String, String> {
    match kind {
        TargetKind::CodexToml => rewrite_toml(text, changes, aethyme),
        TargetKind::ClaudeUser | TargetKind::ProjectJson => rewrite_json(text, changes, aethyme),
    }
}

fn set_toml_value(table: &mut dyn toml_edit::TableLike, key: &str, new: toml_edit::Value) {
    match table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        Some(value) => {
            let decor = value.decor().clone();
            *value = new;
            *value.decor_mut() = decor;
        }
        None => {
            table.insert(key, toml_edit::Item::Value(new));
        }
    }
}

fn rewrite_toml(text: &str, changes: &[Location], aethyme: &str) -> Result<String, String> {
    let mut document: toml_edit::DocumentMut =
        text.parse().map_err(|error| toml_line(text, &error))?;
    for location in changes {
        let server = document
            .get_mut("mcp_servers")
            .and_then(toml_edit::Item::as_table_like_mut)
            .and_then(|servers| servers.get_mut(&location.server))
            .and_then(toml_edit::Item::as_table_like_mut)
            .ok_or("a server entry disappeared")?;
        set_toml_value(server, "command", toml_edit::Value::from(aethyme));
        let mut args = toml_edit::Array::new();
        for word in &location.new_args {
            args.push(word.as_str());
        }
        set_toml_value(server, "args", toml_edit::Value::Array(args));
    }
    Ok(document.to_string())
}

fn find_json_server<'a>(root: &'a Node, location: &Location) -> Option<&'a Node> {
    let map = match &location.project {
        Some(project) => root
            .get("projects")?
            .value
            .get(project)?
            .value
            .get("mcpServers")?,
        None => root.get("mcpServers")?,
    };
    map.value.get(&location.server).map(|member| &member.value)
}

fn rewrite_json(text: &str, changes: &[Location], aethyme: &str) -> Result<String, String> {
    let root = json_document(text)?;
    let unit = json_span::indent_unit(text).to_string();
    // A file written with CRLF line ends keeps them.
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut edits = Vec::new();
    for location in changes {
        let server = find_json_server(&root, location).ok_or("a server entry disappeared")?;
        let command = server.get("command").ok_or("a server entry changed")?;
        let args = &location.new_args;
        edits.push(Edit {
            start: command.value.start,
            end: command.value.end,
            text: json_span::quote(aethyme),
        });
        let object_multiline = text[server.start..command.key_start].contains('\n');
        match server.get("args") {
            Some(existing) => {
                let multiline = text[existing.value.start..existing.value.end].contains('\n');
                let indent = json_span::line_indent(text, existing.key_start);
                edits.push(Edit {
                    start: existing.value.start,
                    end: existing.value.end,
                    text: json_span::render_string_array(args, multiline, indent, &unit, newline),
                });
            }
            None => {
                let indent = json_span::line_indent(text, command.key_start);
                let array =
                    json_span::render_string_array(args, object_multiline, indent, &unit, newline);
                let inserted = if object_multiline {
                    format!(",{newline}{indent}\"args\": {array}")
                } else {
                    format!(", \"args\": {array}")
                };
                edits.push(Edit {
                    start: command.value.end,
                    end: command.value.end,
                    text: inserted,
                });
            }
        }
    }
    json_span::apply(text, edits)
}

/// Plan one file's findings against its current text.
fn plan(
    set: &MatchSet,
    target: &Target,
    text: &str,
    options: &Options,
) -> Result<(Vec<Finding>, usize, usize), String> {
    let servers = servers_of(target.kind, text)?;
    let mut findings = Vec::new();
    let mut already_managed = 0;
    for server in &servers {
        let argv = &server.location.argv;
        if is_connect(argv) {
            let program = Path::new(&argv[0]);
            if program.is_absolute() && !is_executable_file(program) {
                // A connect entry whose aethyme moved: point it at the
                // current one, arguments unchanged.
                let mut location = server.location.clone();
                location.new_args = argv[1..].to_vec();
                let (state, reason) = writable_state(target, options);
                findings.push(Finding {
                    agent: target.kind.agent(),
                    file: target.path.clone(),
                    scope: server.scope.clone(),
                    server: server.location.server.clone(),
                    entry: argv.get(3).cloned().unwrap_or_default(),
                    program: "aethyme".into(),
                    matched_kind: "connect",
                    matched: String::new(),
                    repair: RepairMode::Auto,
                    state,
                    reason: reason.or(Some("stale_aethyme_path")),
                    location,
                });
            } else {
                already_managed += 1;
            }
            continue;
        }
        let Some(found) = set.managed_match(argv) else {
            continue;
        };
        let mut location = server.location.clone();
        let (state, reason) = match set.route(&found) {
            Route::Decline(reason) => ("report_only", Some(reason)),
            Route::Connect { caller_args, .. } if !argv.ends_with(&caller_args) => {
                // The fallback must end with the caller's arguments; a
                // `sh -c` string does not.
                ("report_only", Some("arguments_not_suffix"))
            }
            Route::Connect {
                caller_args,
                caller_package,
            } => {
                location.new_args = super::caller::connect_args(
                    &found.name,
                    &caller_args,
                    caller_package.as_deref(),
                    argv,
                );
                finding_state(target, &found, options)
            }
        };
        findings.push(Finding {
            agent: target.kind.agent(),
            file: target.path.clone(),
            scope: server.scope.clone(),
            server: server.location.server.clone(),
            entry: found.name.clone(),
            program: program_basename(argv),
            matched_kind: found.kind,
            matched: found.matched.clone(),
            repair: found.repair,
            state,
            reason,
            location,
        });
    }
    Ok((findings, servers.len(), already_managed))
}

/// Whether this file may be written in this run at all.
fn writable_state(target: &Target, options: &Options) -> (&'static str, Option<&'static str>) {
    if !target.writable {
        return ("report_only", target.not_writable_reason);
    }
    // Automatic repair touches only the user-level TOML configuration; the
    // JSON files are rewritten often by their own applications and are
    // repaired only by an explicit `repair --apply`.
    if options.auto && target.kind != TargetKind::CodexToml {
        return ("report_only", Some("explicit_apply_only"));
    }
    ("would_repair", None)
}

fn finding_state(
    target: &Target,
    found: &ManagedMatch,
    options: &Options,
) -> (&'static str, Option<&'static str>) {
    let (state, reason) = writable_state(target, options);
    if state != "would_repair" {
        return (state, reason);
    }
    if options.auto && found.repair == RepairMode::Report {
        return ("report_only", Some("repair_report"));
    }
    ("would_repair", None)
}

fn process(
    set: &MatchSet,
    target: &Target,
    locations: &Locations,
    options: &Options,
    hook: &mut dyn FnMut(&Path, usize),
) -> FileReport {
    let mut phases = BTreeMap::new();
    let real = match real_path(&target.path, locations.home.as_deref()) {
        Ok(real) => real,
        Err(state) => return empty_report(target, state, None),
    };
    if options.apply {
        sweep_temporaries(&real);
    }
    let mut report = empty_report(target, "scanned", None);
    let mut backup_written: Option<PathBuf> = None;
    for attempt in 1..=ATTEMPTS {
        if attempt > 1 {
            backoff(attempt);
        }
        report.attempts = attempt;
        let phase = Instant::now();
        let (bytes, before, mode) = match fingerprint(&real) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                report.state = "absent";
                return report;
            }
            Err(_) => {
                report.state = "unreadable";
                return report;
            }
        };
        phases.insert("read", elapsed_ms(phase));
        let Ok(text) = String::from_utf8(bytes.clone()) else {
            report.state = "parse_error";
            report.detail = Some("not UTF-8".into());
            return report;
        };
        let phase = Instant::now();
        let (findings, servers, already_managed) = match plan(set, target, &text, options) {
            Ok(planned) => planned,
            Err(detail) => {
                report.state = "parse_error";
                report.detail = Some(detail);
                return report;
            }
        };
        phases.insert("plan", elapsed_ms(phase));
        report.servers = servers;
        report.already_managed = already_managed;
        report.findings = findings;
        report.phases_ms = phases.clone();
        let changes: Vec<Location> = report
            .findings
            .iter()
            .filter(|finding| finding.state == "would_repair")
            .map(|finding| finding.location.clone())
            .collect();
        if !options.apply || changes.is_empty() {
            return report;
        }
        let phase = Instant::now();
        let new_text = match rewrite(
            target.kind,
            &text,
            &changes,
            &options.aethyme.to_string_lossy(),
        ) {
            Ok(new_text) => new_text,
            Err(detail) => {
                report.state = "parse_error";
                report.detail = Some(detail);
                return report;
            }
        };
        // The result must parse and every changed entry must now be a connect.
        let verified = servers_of(target.kind, &new_text).is_ok_and(|after| {
            changes.iter().all(|location| {
                after.iter().any(|server| {
                    server.location.server == location.server
                        && server.location.project == location.project
                        && is_connect(&server.location.argv)
                })
            })
        });
        if !verified {
            report.state = "write_failed";
            report.detail = Some("the rewritten file did not verify; nothing written".into());
            return report;
        }
        phases.insert("rewrite", elapsed_ms(phase));
        if backup_written.is_none() {
            let phase = Instant::now();
            match write_backup(&options.backup_dir, target, &bytes) {
                Ok(path) => backup_written = Some(path),
                Err(error) => {
                    report.state = "write_failed";
                    report.detail = Some(format!("cannot write the backup: {error}"));
                    return report;
                }
            }
            phases.insert("backup", elapsed_ms(phase));
        }
        report.backup.clone_from(&backup_written);
        let phase = Instant::now();
        match swap(&real, new_text.as_bytes(), mode, &before, &mut || {
            hook(&real, attempt)
        }) {
            Ok(true) => {
                phases.insert("write", elapsed_ms(phase));
                report.phases_ms = phases;
                report.state = "repaired";
                for finding in &mut report.findings {
                    if finding.state == "would_repair" {
                        finding.state = "repaired";
                    }
                }
                return report;
            }
            Ok(false) => continue,
            Err(error) => {
                report.state = "write_failed";
                report.detail = Some(error.to_string());
                return report;
            }
        }
    }
    report.state = "contended";
    for finding in &mut report.findings {
        if finding.state == "would_repair" {
            finding.state = "contended";
        }
    }
    report
}

/// Wait 20 to 80 ms (jittered) before attempt `attempt`, so a retry does not
/// land in step with the application that rewrote the file.
fn backoff(attempt: usize) {
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::from(elapsed.subsec_nanos()) % 60)
        .unwrap_or(0);
    std::thread::sleep(Duration::from_millis(
        20 * u64::try_from(attempt - 1).unwrap_or(1) + jitter,
    ));
}

/// Write `bytes` beside `real` and rename it over `real` when the file is
/// still as `before` describes. `Ok(false)`: it changed; nothing written.
fn swap(
    real: &Path,
    bytes: &[u8],
    mode: u32,
    before: &Fingerprint,
    hook: &mut dyn FnMut(),
) -> std::io::Result<bool> {
    let directory = real
        .parent()
        .ok_or_else(|| std::io::Error::other("the configuration has no parent directory"))?;
    // Created 0600; removed when dropped on every path that does not persist.
    let mut temporary = tempfile::Builder::new()
        .prefix(TEMPORARY_PREFIX)
        .tempfile_in(directory)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(mode))?;
    hook();
    let unchanged = match fingerprint(real) {
        Ok((_, now, _)) => now == *before,
        Err(_) => false,
    };
    if !unchanged {
        // Dropping the temporary file removes it.
        return Ok(false);
    }
    temporary.persist(real).map_err(|error| error.error)?;
    Ok(true)
}

fn backup_label(target: &Target) -> String {
    let digest = Sha256::digest(target.path.to_string_lossy().as_bytes());
    let short: String = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{}-{short}", target.kind.label())
}

/// Write a 0600 backup and keep the newest [`KEEP_BACKUPS`] for this file.
fn write_backup(directory: &Path, target: &Target, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    let label = backup_label(target);
    let mut stamp = telemetry::now_ms();
    let path = loop {
        let candidate = directory.join(format!("{label}-{stamp:013}.bak"));
        if !candidate.exists() {
            break candidate;
        }
        stamp += 1;
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    prune_backups(directory, &label)?;
    Ok(path)
}

fn prune_backups(directory: &Path, label: &str) -> std::io::Result<()> {
    let prefix = format!("{label}-");
    let mut names: Vec<String> = std::fs::read_dir(directory)?
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| name.starts_with(&prefix) && name.ends_with(".bak"))
        .collect();
    names.sort();
    let excess = names.len().saturating_sub(KEEP_BACKUPS);
    for name in &names[..excess] {
        std::fs::remove_file(directory.join(name))?;
    }
    Ok(())
}

/// Record a run in `events.jsonl` (counts and timings only).
pub fn record(host: &Path, report: &Report, command_ms: u64) {
    let outcome = if report.files.iter().any(|file| file.state == "contended") {
        "contended"
    } else if report.repaired > 0 {
        "repaired"
    } else if report.matched > 0 {
        "matched"
    } else {
        "no_op"
    };
    let mut phases: BTreeMap<String, u64> = BTreeMap::new();
    phases.insert("command_total".into(), command_ms);
    phases.insert("matched".into(), report.matched as u64);
    phases.insert("repaired".into(), report.repaired as u64);
    for file in &report.files {
        for (phase, value) in &file.phases_ms {
            *phases
                .entry(format!("{}_{phase}", file.kind.label()))
                .or_default() += value;
        }
    }
    crate::warn_unrecorded(
        "record the MCP repair event",
        telemetry::append(
            host,
            &Event {
                at_ms: telemetry::now_ms(),
                command: "repair".into(),
                name: "repair".into(),
                outcome: outcome.into(),
                reason: None,
                copy: None,
                key: None,
                phases_ms: phases,
                shadowed: false,
                fallback: false,
                warnings: Vec::new(),
            },
        ),
    );
}

/// `[mcp] repair_on_session_start` in `<host>/settings.toml`; off when the
/// file is absent or does not say `true`.
pub fn repair_on_session_start(host: &Path) -> Result<bool, String> {
    let text = match std::fs::read_to_string(host.join(SETTINGS_FILE)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    let table: toml::Table = toml::from_str(&text)
        .map_err(|_| format!("{} is not valid TOML", host.join(SETTINGS_FILE).display()))?;
    match table
        .get("mcp")
        .and_then(|mcp| mcp.get("repair_on_session_start"))
    {
        None => Ok(false),
        Some(toml::Value::Boolean(on)) => Ok(*on),
        Some(_) => Err("mcp.repair_on_session_start must be true or false".into()),
    }
}

/// The host entries that are managed (host file only, no repository, no
/// Git discovery).
pub fn host_match_set(host_file: Option<PathBuf>) -> MatchSet {
    let sources = super::config::Sources {
        host_file,
        repository: None,
        trusted_digests: Vec::new(),
    };
    MatchSet::from_loaded(&super::config::load(
        &sources,
        &super::checkouts::GitCheckouts,
        None,
    ))
}

/// The session-start hook's repair: `--apply --auto` on the user-level
/// Codex TOML configuration only, within [`SESSION_START_BUDGET`] (the
/// managed list, the target and every file start inside it), and only when
/// the owner turned `mcp.repair_on_session_start` on. The JSON files are
/// left to an explicit `repair --apply`: their applications rewrite them
/// from memory. `None` on a no-op (the hook stays silent); one line when
/// something was repaired.
pub fn session_start_notice() -> Option<String> {
    let started = Instant::now();
    let deadline = started + SESSION_START_BUDGET;
    let host = super::host_directory()?;
    if !repair_on_session_start(&host).unwrap_or(false) {
        return None;
    }
    let set = host_match_set(Some(host.join("services.toml")));
    if set.is_empty() || Instant::now() + WORST_CASE_PER_FILE > deadline {
        return None;
    }
    // No repository discovery and no Git: only the TOML file is a target.
    let locations = Locations::discover(None, None, None);
    let targets = locations.targets(Some("codex"));
    let report = run(
        &set,
        &targets,
        &locations,
        &Options {
            apply: true,
            auto: true,
            deadline: Some(deadline),
            backup_dir: host.join(BACKUP_DIRECTORY),
            aethyme: super::intercept::aethyme_target().ok()?,
        },
    );
    record(&host, &report, elapsed_ms(started));
    (report.repaired > 0).then(|| {
        format!(
            "Aethyme: repaired {} managed MCP server entr{} in agent configuration to start \
             through `aethyme mcp connect`; it takes effect at the next agent start.",
            report.repaired,
            if report.repaired == 1 { "y" } else { "ies" }
        )
    })
}
