//! Launcher interception: the guarantee behind managed servers.
//!
//! `aethyme mcp intercept install` creates a shim directory (by default
//! `<host state>/mcp/shims`) holding one symlink per launcher
//! ([`super::managed::LAUNCHERS`]) and per `match_executables` name, each
//! pointing at the `aethyme` binary, and writes the precompiled match file
//! `<host state>/mcp/intercept.match`. The owner puts the directory first in
//! `PATH` for agent hosts; nothing here edits a shell profile.
//!
//! The binary is multi-call: [`shim_main`] runs first in `main` and looks at
//! `argv[0]`'s basename before anything else is initialised (no database,
//! no configuration discovery, no Git). Invoked as `aethyme` it returns at
//! once. Invoked through a shim it reads the one small match file and tests
//! the argv with [`super::managed::MatchSet::managed_match`]:
//! - no match: `exec` the real tool (the first `PATH` hit for the name that
//!   does not resolve to this binary), with argv, environment, stdio and
//!   exit status untouched and nothing printed; none found: exit 127 with
//!   `<name>: command not found`;
//! - match: `exec aethyme mcp connect <entry> --fallback -- <real tool's
//!   absolute path> <original arguments...>`, with [`GUARD_ENV`] and
//!   [`DEPTH_ENV`] set and the shim directory removed from `PATH`, so
//!   neither `connect`, its fallback nor the server can enter a shim again.
//!
//! An unreadable or corrupt match file never blocks a launcher: the call
//! passes through and the error is counted. Telemetry adds no I/O to the
//! pass-through path: only hand-offs and errors are recorded, in
//! `<host state>/mcp/events.jsonl` (`command = "intercept"`).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::Serialize;

use super::managed::{ArgsMode, LAUNCHERS, MatchEntry, MatchSet, RepairMode, Route};
use super::resolve::{is_executable_file, search_path};
use super::telemetry::{self, Event};

/// The precompiled match file, under `<host state>/mcp/`.
pub const MATCH_FILE: &str = "intercept.match";
/// The default shim directory, under `<host state>/mcp/`.
pub const DEFAULT_SHIM_DIRECTORY: &str = "shims";
/// Marks a directory as one `intercept install` created.
pub const MARKER_FILE: &str = ".aethyme-mcp-shims";
/// Set on a hand-off: a shim that sees it always passes through.
pub const GUARD_ENV: &str = "AETHYME_MCP_SHIM_GUARD";
/// Hand-off depth: the second defence should the guard be dropped.
pub const DEPTH_ENV: &str = "AETHYME_MCP_SHIM_DEPTH";
/// A shim never hands off at this depth or deeper.
pub const MAX_DEPTH: u32 = 2;

/// Pass-throughs in one chain: a shim that would be the fifth gives up.
pub const HOPS_ENV: &str = "AETHYME_MCP_SHIM_HOPS";
/// The cap on [`HOPS_ENV`].
pub const MAX_HOPS: u32 = 4;

const MATCH_HEADER: &str = "aethyme-mcp-intercept 1";

/// The parsed match file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchFile {
    pub set: MatchSet,
    pub shim_dir: Option<PathBuf>,
}

impl MatchFile {
    fn shims_executable(&self, name: &str) -> bool {
        self.set.entries.iter().any(|entry| {
            entry
                .executables
                .iter()
                .any(|executable| executable == name)
        })
    }
}

/// The match file's text. Line-based so the shim parses it without a TOML
/// parser; `end` makes a truncated file detectably corrupt.
pub fn render_match_file(set: &MatchSet, shim_dir: &Path) -> Result<String, String> {
    let dir = shim_dir.to_str().ok_or("the shim directory is not UTF-8")?;
    if dir.contains('\n') || dir.contains('\r') {
        return Err("the shim directory contains a line break".into());
    }
    let mut text = format!("{MATCH_HEADER}\nshim_dir {dir}\n");
    for entry in &set.entries {
        text.push_str(&format!(
            "entry {} {} {}\n",
            entry.name,
            entry.repair.as_str(),
            entry.args.as_str()
        ));
        if let Some(version) = &entry.version {
            text.push_str(&format!("version {version}\n"));
        }
        if !entry.base_args.is_empty() {
            let encoded = serde_json::to_string(&entry.base_args).map_err(|e| e.to_string())?;
            text.push_str(&format!("base_args {encoded}\n"));
        }
        for package in &entry.packages {
            text.push_str(&format!("package {package}\n"));
        }
        for executable in &entry.executables {
            text.push_str(&format!("executable {executable}\n"));
        }
    }
    text.push_str("end\n");
    Ok(text)
}

/// Parse a match file; any deviation is `Err` with a stable code.
pub fn parse_match_file(bytes: &[u8]) -> Result<MatchFile, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "match_file_not_utf8")?;
    let mut lines = text.lines();
    if lines.next() != Some(MATCH_HEADER) {
        return Err("match_file_header");
    }
    let mut set = MatchSet::default();
    let mut shim_dir = None;
    let mut ended = false;
    for line in lines {
        if ended {
            return Err("match_file_trailing");
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        let simple = |value: &str| !value.is_empty() && !value.contains(char::is_whitespace);
        match key {
            "shim_dir" if !value.is_empty() => shim_dir = Some(PathBuf::from(value)),
            "entry" => {
                let words: Vec<&str> = value.split(' ').collect();
                let (name, repair, args) = match words.as_slice() {
                    [name, repair] => (*name, *repair, "caller"),
                    [name, repair, args] => (*name, *repair, *args),
                    _ => return Err("match_file_entry"),
                };
                if !simple(name) {
                    return Err("match_file_entry");
                }
                set.entries.push(MatchEntry {
                    name: name.to_string(),
                    packages: Vec::new(),
                    executables: Vec::new(),
                    repair: RepairMode::parse(repair).ok_or("match_file_entry")?,
                    args: ArgsMode::parse(args).ok_or("match_file_entry")?,
                    version: None,
                    base_args: Vec::new(),
                });
            }
            "version" if simple(value) => {
                set.entries.last_mut().ok_or("match_file_order")?.version = Some(value.to_string());
            }
            "base_args" => {
                set.entries.last_mut().ok_or("match_file_order")?.base_args =
                    serde_json::from_str(value).map_err(|_| "match_file_line")?;
            }
            "package" if simple(value) => set
                .entries
                .last_mut()
                .ok_or("match_file_order")?
                .packages
                .push(value.to_string()),
            "executable" if simple(value) => set
                .entries
                .last_mut()
                .ok_or("match_file_order")?
                .executables
                .push(value.to_string()),
            "end" if value.is_empty() => ended = true,
            _ => return Err("match_file_line"),
        }
    }
    if !ended {
        return Err("match_file_truncated");
    }
    Ok(MatchFile { set, shim_dir })
}

/// `argv[0]`'s basename when it can name a shim: UTF-8 and not `aethyme`.
fn shim_name(argv0: &OsStr) -> Option<&str> {
    let name = Path::new(argv0).file_name()?.to_str()?;
    (!name.starts_with("aethyme")).then_some(name)
}

/// The multi-call entry point; call it first in `main`. `None`: this is not
/// a shim invocation and `main` continues. Otherwise the process exit code
/// (only reached when no `exec` replaced the process).
pub fn shim_main() -> Option<u8> {
    let mut args = std::env::args_os();
    let argv0 = args.next()?;
    let name = shim_name(&argv0)?.to_string();
    let launcher = LAUNCHERS.contains(&name.as_str());
    let started = Instant::now();
    let host = super::host_directory();
    let compiled = host.as_ref().map(
        |directory| match std::fs::read(directory.join(MATCH_FILE)) {
            Ok(bytes) => parse_match_file(&bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err("match_file_absent"),
            Err(_) => Err("match_file_unreadable"),
        },
    );
    let is_shim = launcher || matches!(&compiled, Some(Ok(file)) if file.shims_executable(&name));
    if !is_shim {
        return None;
    }
    let rest: Vec<OsString> = args.collect();
    // SAFETY: isatty only inspects file descriptor 0.
    let interactive = unsafe { libc::isatty(0) } == 1;
    Some(run_shim(&ShimCall {
        name,
        argv0,
        rest,
        compiled,
        host,
        path_env: std::env::var_os("PATH"),
        self_exe: std::env::current_exe()
            .ok()
            .and_then(|path| path.canonicalize().ok()),
        guard: guard_active(
            std::env::var_os(GUARD_ENV).as_deref(),
            std::env::var_os(DEPTH_ENV).as_deref(),
        ),
        depth: count(std::env::var_os(DEPTH_ENV).as_deref()),
        hops: count(std::env::var_os(HOPS_ENV).as_deref()),
        interactive,
        started,
    }))
}

fn count(value: Option<&OsStr>) -> u32 {
    value
        .and_then(OsStr::to_str)
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
}

/// Whether a shim must pass through without matching.
pub fn guard_active(guard: Option<&OsStr>, depth_value: Option<&OsStr>) -> bool {
    guard.is_some() || count(depth_value) >= MAX_DEPTH
}

struct ShimCall {
    name: String,
    argv0: OsString,
    rest: Vec<OsString>,
    compiled: Option<Result<MatchFile, &'static str>>,
    host: Option<PathBuf>,
    path_env: Option<OsString>,
    self_exe: Option<PathBuf>,
    guard: bool,
    depth: u32,
    hops: u32,
    interactive: bool,
    started: Instant,
}

/// What a shim decided, without doing it (tests drive [`decide`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Exec the real tool; with a one-line note on stderr first when a
    /// managed call was deliberately not routed.
    PassThrough { real: PathBuf, note: Option<String> },
    /// No real tool: exit 127.
    NotFound,
    /// Hand off to `connect`: the last `caller_args` arguments are the
    /// caller's, and `caller_package` replaces the entry's package spec.
    HandOff {
        entry: String,
        fallback: Option<PathBuf>,
        caller_args: usize,
        caller_package: Option<String>,
    },
}

/// What a shim sees of its call.
pub struct ShimInput<'a> {
    pub name: &'a str,
    pub args: &'a [OsString],
    pub file: Option<&'a MatchFile>,
    pub guard: bool,
    /// Standard input is a terminal: a person, not an agent host.
    pub interactive: bool,
    pub path_env: Option<&'a OsStr>,
    pub self_exe: Option<&'a Path>,
}

/// Decide for one call.
pub fn decide(input: &ShimInput<'_>) -> Decision {
    let shim_dir = input.file.and_then(|file| file.shim_dir.as_deref());
    let real = find_real_tool(input.name, input.path_env, input.self_exe, shim_dir);
    let pass = |note: Option<String>| match &real {
        Some(path) => Decision::PassThrough {
            real: path.clone(),
            note,
        },
        None => Decision::NotFound,
    };
    if input.guard || input.interactive || input.self_exe.is_none() {
        return pass(None);
    }
    let Some(file) = input.file else {
        return pass(None);
    };
    let Some(argv) = utf8_argv(input.name, input.args) else {
        return pass(None);
    };
    let Some(found) = file.set.managed_match(&argv) else {
        return pass(None);
    };
    match file.set.route(&found) {
        Route::Connect {
            caller_args,
            caller_package,
        } if caller_args.len() <= input.args.len() => Decision::HandOff {
            entry: found.name,
            fallback: real.clone(),
            caller_args: caller_args.len(),
            caller_package,
        },
        Route::Connect { .. } | Route::Decline("interactive_arguments") => pass(None),
        Route::Decline(reason) => pass(Some(format!(
            "aethyme mcp: {}: not started through connect ({reason}; see the managed entry's \
             `args` and version)",
            found.name
        ))),
    }
}

fn utf8_argv(name: &str, args: &[OsString]) -> Option<Vec<String>> {
    let mut argv = vec![name.to_string()];
    for arg in args {
        argv.push(arg.to_str()?.to_string());
    }
    Some(argv)
}

fn run_shim(call: &ShimCall) -> u8 {
    if call.hops >= MAX_HOPS {
        eprintln!(
            "{}: aethyme launcher shims passed this call on {} times without reaching the real \
             tool; check `aethyme mcp intercept status` (a stale shim directory on PATH?)",
            call.name, call.hops
        );
        record(call, "intercept", "hop_limit", None);
        return 127;
    }
    let file = match &call.compiled {
        Some(Ok(file)) => Some(file),
        Some(Err(code)) => {
            // A missing match file only means interception is off.
            if *code != "match_file_absent" {
                record(call, "intercept", "match_file_error", Some(*code));
            }
            None
        }
        None => None,
    };
    let decision = decide(&ShimInput {
        name: &call.name,
        args: &call.rest,
        file,
        guard: call.guard,
        interactive: call.interactive,
        path_env: call.path_env.as_deref(),
        self_exe: call.self_exe.as_deref(),
    });
    match decision {
        Decision::PassThrough { real, note } => {
            if let Some(note) = note {
                eprintln!("{note}");
                record(call, "intercept", "declined", None);
            }
            pass_through(call, &real)
        }
        Decision::NotFound => {
            eprintln!("{}: command not found", call.name);
            127
        }
        Decision::HandOff {
            entry,
            fallback,
            caller_args,
            caller_package,
        } => {
            let Some(self_exe) = &call.self_exe else {
                return 127;
            };
            let mut command = Command::new(self_exe);
            command
                .arg0("aethyme")
                .args(["mcp", "connect", entry.as_str()]);
            if caller_args > 0 {
                command.arg("--caller-args").arg(caller_args.to_string());
            }
            if let Some(package) = &caller_package {
                command.arg("--caller-package").arg(package);
            }
            match &fallback {
                Some(real) => {
                    command
                        .args(["--fallback", "--"])
                        .arg(real)
                        .args(&call.rest);
                }
                None if caller_args > 0 => {
                    command
                        .arg("--")
                        .args(&call.rest[call.rest.len() - caller_args..]);
                }
                None => {}
            }
            command
                .env(GUARD_ENV, "1")
                .env(DEPTH_ENV, (call.depth + 1).to_string());
            let shim_dir = file.and_then(|file| file.shim_dir.as_deref());
            if let Some(path) = strip_path(call.path_env.as_deref(), shim_dir) {
                command.env("PATH", path);
            }
            record(
                call,
                &entry,
                if fallback.is_some() {
                    "handoff"
                } else {
                    "handoff_without_fallback"
                },
                None,
            );
            let error = command.exec();
            record(call, &entry, "handoff_failed", Some("exec_failed"));
            eprintln!("aethyme mcp: {entry}: could not hand off to connect: {error}");
            match fallback {
                Some(real) => pass_through(call, &real),
                None => 127,
            }
        }
    }
}

/// Exec the real tool. Arguments, environment (plus the hop counter),
/// stdio and exit status are the real tool's. Rust starts every program
/// with SIGPIPE ignored and `Command` restores the default disposition in
/// the new image, so a parent's deliberately ignored SIGPIPE is not kept.
fn pass_through(call: &ShimCall, real: &Path) -> u8 {
    let error = Command::new(real)
        .arg0(&call.argv0)
        .args(&call.rest)
        .env(HOPS_ENV, (call.hops + 1).to_string())
        .exec();
    eprintln!("{}: {error}", call.name);
    126
}

/// Best effort; never prints on success or failure (the shim's stderr
/// belongs to the launcher).
fn record(call: &ShimCall, name: &str, outcome: &str, reason: Option<&str>) {
    let Some(directory) = &call.host else {
        return;
    };
    let mut phases = BTreeMap::new();
    phases.insert(
        "shim_us".to_string(),
        u64::try_from(call.started.elapsed().as_micros()).unwrap_or(u64::MAX),
    );
    // Best effort, and silent: the shim's stderr belongs to the launcher.
    if let Err(_unrecorded) = telemetry::append(
        directory,
        &Event {
            at_ms: telemetry::now_ms(),
            command: "intercept".into(),
            name: name.to_string(),
            outcome: outcome.into(),
            reason: reason.map(str::to_string),
            copy: None,
            key: None,
            phases_ms: phases,
            shadowed: false,
            fallback: false,
            warnings: Vec::new(),
        },
    ) {
        // Nothing more to do: a shim never prints on its own behalf.
    }
}

fn canonical(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

/// The real tool for `name`: the first absolute `PATH` entry holding an
/// executable of that name that is not the shim directory (compared
/// canonically, so a second spelling or a symlink to it is skipped too)
/// and does not resolve to this binary or to any file named `aethyme*` (a
/// stale shim directory of another build). Empty and relative `PATH`
/// entries are skipped, unlike a shell, which would search the current
/// directory. The returned path is the `PATH` spelling.
pub fn find_real_tool(
    name: &str,
    path_env: Option<&OsStr>,
    self_exe: Option<&Path>,
    shim_dir: Option<&Path>,
) -> Option<PathBuf> {
    let path_env = path_env?;
    let shim_dir = shim_dir.and_then(canonical);
    for dir in std::env::split_paths(path_env) {
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(name);
        if !is_executable_file(&candidate) {
            continue;
        }
        if shim_dir.is_some() && canonical(&dir) == shim_dir {
            continue;
        }
        let Some(resolved) = canonical(&candidate) else {
            continue;
        };
        if self_exe.is_some_and(|exe| exe == resolved)
            || resolved
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|file| file.starts_with("aethyme"))
        {
            continue;
        }
        return Some(candidate);
    }
    None
}

/// `PATH` without the shim directory (every spelling of it). `None` when
/// nothing would change.
pub fn strip_path(path_env: Option<&OsStr>, shim_dir: Option<&Path>) -> Option<OsString> {
    let path_env = path_env?;
    let shim_dir = canonical(shim_dir?)?;
    let all: Vec<PathBuf> = std::env::split_paths(path_env).collect();
    let kept: Vec<&PathBuf> = all
        .iter()
        .filter(|dir| canonical(dir).as_deref() != Some(shim_dir.as_path()))
        .collect();
    if kept.len() == all.len() {
        return None;
    }
    std::env::join_paths(kept).ok()
}

// ---------------------------------------------------------------------------
// install / uninstall / status

/// `<host>/intercept.match`.
pub fn match_file_path(host: &Path) -> PathBuf {
    host.join(MATCH_FILE)
}

/// The default shim directory for a host directory.
pub fn default_shim_dir(host: &Path) -> PathBuf {
    host.join(DEFAULT_SHIM_DIRECTORY)
}

/// The binary the shims point at: the `aethyme` on `PATH` when it is this
/// binary (a stable spelling across upgrades), else this binary's path.
pub fn aethyme_target() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| format!("cannot locate aethyme: {error}"))?;
    let canonical_exe = exe.canonicalize().unwrap_or_else(|_| exe.clone());
    if let Some(on_path) = search_path("aethyme", std::env::var_os("PATH").as_deref())
        && canonical(&on_path).as_deref() == Some(canonical_exe.as_path())
    {
        return Ok(on_path);
    }
    Ok(exe)
}

/// Every shim name for a match set: the launchers, then its executables.
pub fn shim_names(set: &MatchSet) -> Vec<String> {
    let mut names: Vec<String> = LAUNCHERS.iter().map(|name| (*name).to_string()).collect();
    names.extend(set.executables());
    names
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallReport {
    pub shim_dir: PathBuf,
    pub target: PathBuf,
    pub match_file: PathBuf,
    pub shims: Vec<String>,
    pub created: Vec<String>,
    pub removed: Vec<String>,
    pub managed_entries: usize,
    pub path_line: String,
    pub phases_ms: BTreeMap<&'static str, u64>,
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn ensure_private_directory(directory: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
}

/// The line the owner adds so agent hosts find the shims first.
pub fn path_line(shim_dir: &Path) -> String {
    let escaped: String = shim_dir
        .to_string_lossy()
        .chars()
        .flat_map(|c| match c {
            '"' | '$' | '`' | '\\' => vec!['\\', c],
            other => vec![other],
        })
        .collect();
    format!("export PATH=\"{escaped}:$PATH\"")
}

/// Create or refresh the shims and the match file.
pub fn install(
    host: &Path,
    dir: Option<&Path>,
    set: &MatchSet,
    target: &Path,
) -> Result<InstallReport, String> {
    let started = Instant::now();
    let mut phases = BTreeMap::new();
    let shim_dir = match dir {
        Some(dir) if !dir.is_absolute() => {
            return Err(format!("--dir {} must be absolute", dir.display()));
        }
        Some(dir) => dir.to_path_buf(),
        None => default_shim_dir(host),
    };
    let phase = Instant::now();
    prepare_shim_dir(&shim_dir)?;
    let names = shim_names(set);
    let (created, removed) = sync_shims(&shim_dir, &names, target)?;
    phases.insert("shims", elapsed_ms(phase));
    let phase = Instant::now();
    let text = render_match_file(set, &shim_dir)?;
    write_private(host, &match_file_path(host), text.as_bytes())
        .map_err(|error| format!("cannot write the match file: {error}"))?;
    phases.insert("match_file", elapsed_ms(phase));
    phases.insert("total", elapsed_ms(started));
    Ok(InstallReport {
        path_line: path_line(&shim_dir),
        shim_dir,
        target: target.to_path_buf(),
        match_file: match_file_path(host),
        shims: names,
        created,
        removed,
        managed_entries: set.entries.len(),
        phases_ms: phases,
    })
}

fn write_private(host: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    ensure_private_directory(host)?;
    let mut temporary = tempfile::NamedTempFile::new_in(host)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn prepare_shim_dir(shim_dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::symlink_metadata(shim_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(format!(
                "{} exists and is not a plain directory",
                shim_dir.display()
            ));
        }
        Ok(_) => {
            let ours = shim_dir.join(MARKER_FILE).is_file();
            let empty = std::fs::read_dir(shim_dir)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if !ours && !empty {
                return Err(format!(
                    "{} is not empty and was not created by `aethyme mcp intercept install`; \
                     choose an empty or new directory",
                    shim_dir.display()
                ));
            }
        }
        Err(_) => {
            if let Some(parent) = shim_dir.parent() {
                ensure_private_directory(parent)
                    .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
            }
            std::fs::create_dir(shim_dir)
                .map_err(|error| format!("cannot create {}: {error}", shim_dir.display()))?;
        }
    }
    std::fs::set_permissions(shim_dir, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("cannot set {} to 0755: {error}", shim_dir.display()))?;
    std::fs::write(
        shim_dir.join(MARKER_FILE),
        "Created by `aethyme mcp intercept install`; removed by `aethyme mcp intercept \
         uninstall`.\n",
    )
    .map_err(|error| format!("cannot mark {}: {error}", shim_dir.display()))?;
    std::fs::set_permissions(
        shim_dir.join(MARKER_FILE),
        std::fs::Permissions::from_mode(0o644),
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// Whether `path` is a symlink this feature made: it resolves to the same
/// file as `target`, or to any binary called `aethyme`.
fn is_our_shim(path: &Path, target: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.file_type().is_symlink() {
        return false;
    }
    match (canonical(path), canonical(target)) {
        (Some(resolved), Some(target)) if resolved == target => true,
        (Some(resolved), _) => resolved
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("aethyme")),
        (None, _) => std::fs::read_link(path).is_ok_and(|link| {
            link.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("aethyme"))
        }),
    }
}

fn sync_shims(
    shim_dir: &Path,
    names: &[String],
    target: &Path,
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut created = Vec::new();
    let mut removed = Vec::new();
    if let Ok(entries) = std::fs::read_dir(shim_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if file_name == MARKER_FILE || names.contains(&file_name) {
                continue;
            }
            if is_our_shim(&entry.path(), target) {
                std::fs::remove_file(entry.path())
                    .map_err(|error| format!("cannot remove {file_name}: {error}"))?;
                removed.push(file_name);
            }
        }
    }
    for name in names {
        let path = shim_dir.join(name);
        let current = std::fs::read_link(&path).ok();
        if current.as_deref() == Some(target) {
            continue;
        }
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                std::fs::remove_file(&path)
                    .map_err(|error| format!("cannot replace {name}: {error}"))?;
            }
            Ok(_) => {
                return Err(format!(
                    "{} is a file this command did not create; remove it first",
                    path.display()
                ));
            }
            Err(_) => {}
        }
        std::os::unix::fs::symlink(target, &path)
            .map_err(|error| format!("cannot create the shim {name}: {error}"))?;
        created.push(name.clone());
    }
    Ok((created, removed))
}

#[derive(Debug, Clone, Serialize)]
pub struct UninstallReport {
    pub shim_dir: PathBuf,
    pub removed: Vec<String>,
    pub directory_removed: bool,
    pub match_file_removed: bool,
}

/// Remove the shims, the marker, the directory when empty, and the match
/// file. Files the install did not create are left alone.
pub fn uninstall(host: &Path) -> Result<UninstallReport, String> {
    let match_file = match_file_path(host);
    let shim_dir = std::fs::read(&match_file)
        .ok()
        .and_then(|bytes| parse_match_file(&bytes).ok())
        .and_then(|file| file.shim_dir)
        .unwrap_or_else(|| default_shim_dir(host));
    let mut removed = Vec::new();
    let marked = shim_dir.join(MARKER_FILE).is_file();
    if marked && let Ok(entries) = std::fs::read_dir(&shim_dir) {
        let target = aethyme_target().unwrap_or_default();
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if is_our_shim(&entry.path(), &target) {
                std::fs::remove_file(entry.path())
                    .map_err(|error| format!("cannot remove {file_name}: {error}"))?;
                removed.push(file_name);
            }
        }
        std::fs::remove_file(shim_dir.join(MARKER_FILE))
            .map_err(|error| format!("cannot remove the marker: {error}"))?;
    }
    let directory_removed = marked && std::fs::remove_dir(&shim_dir).is_ok();
    let match_file_removed = match std::fs::remove_file(&match_file) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("cannot remove the match file: {error}")),
    };
    removed.sort();
    Ok(UninstallReport {
        shim_dir,
        removed,
        directory_removed,
        match_file_removed,
    })
}

/// After `manage add/remove`: refresh the match file and the shims when
/// interception is installed. `Ok(None)`: it is not.
pub fn regenerate_if_installed(
    host: &Path,
    set: &MatchSet,
) -> Result<Option<InstallReport>, String> {
    let Ok(bytes) = std::fs::read(match_file_path(host)) else {
        return Ok(None);
    };
    let shim_dir = parse_match_file(&bytes)
        .ok()
        .and_then(|file| file.shim_dir)
        .unwrap_or_else(|| default_shim_dir(host));
    let target = std::fs::read_link(shim_dir.join(LAUNCHERS[0]))
        .ok()
        .filter(|path| is_executable_file(path))
        .map_or_else(aethyme_target, Ok)?;
    install(host, Some(&shim_dir), set, &target).map(Some)
}

/// Where one launcher resolves on `PATH`.
#[derive(Debug, Clone, Serialize)]
pub struct ToolOrder {
    pub name: String,
    /// Index of the first `PATH` entry that is the shim directory.
    pub shim_index: Option<usize>,
    /// Index of the first `PATH` entry with the real tool.
    pub real_index: Option<usize>,
    pub ahead: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct InterceptStatus {
    pub installed: bool,
    pub shim_dir: PathBuf,
    pub directory_exists: bool,
    pub match_file: PathBuf,
    /// `absent`, `current`, `stale` (the managed list changed since) or a
    /// corruption code.
    pub match_file_state: String,
    /// Every shim exists and points at an executable, and the match file is
    /// current.
    pub complete: bool,
    pub missing_shims: Vec<String>,
    pub broken_shims: Vec<String>,
    pub on_path: bool,
    /// On `PATH` before every real launcher that is on it.
    pub ahead_of_real_tools: bool,
    pub tools: Vec<ToolOrder>,
    pub path_line: String,
    /// Recorded hand-offs and errors, per outcome.
    pub counters: BTreeMap<String, u64>,
}

/// Interception state against the current managed list and `PATH`.
pub fn status(host: &Path, set: &MatchSet, path_env: Option<&OsStr>) -> InterceptStatus {
    let match_file = match_file_path(host);
    let (installed, parsed) = match std::fs::read(&match_file) {
        Ok(bytes) => (true, Some(parse_match_file(&bytes))),
        Err(_) => (false, None),
    };
    let shim_dir = parsed
        .as_ref()
        .and_then(|parsed| parsed.as_ref().ok())
        .and_then(|file| file.shim_dir.clone())
        .unwrap_or_else(|| default_shim_dir(host));
    let match_file_state = match &parsed {
        None => "absent".to_string(),
        Some(Err(code)) => (*code).to_string(),
        Some(Ok(file)) if file.set == *set => "current".to_string(),
        Some(Ok(_)) => "stale".to_string(),
    };
    let directory_exists = shim_dir.is_dir();
    let names = shim_names(set);
    let mut missing_shims = Vec::new();
    let mut broken_shims = Vec::new();
    for name in &names {
        let path = shim_dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Err(_) => missing_shims.push(name.clone()),
            Ok(metadata) if !metadata.file_type().is_symlink() || !is_executable_file(&path) => {
                broken_shims.push(name.clone());
            }
            Ok(_) => {}
        }
    }
    let shim_canonical = canonical(&shim_dir);
    let self_exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok());
    let dirs: Vec<PathBuf> = path_env
        .map(|value| std::env::split_paths(value).collect())
        .unwrap_or_default();
    let shim_index = shim_canonical.as_ref().and_then(|shim| {
        dirs.iter()
            .position(|dir| canonical(dir).as_ref() == Some(shim))
    });
    let tools: Vec<ToolOrder> = LAUNCHERS
        .iter()
        .map(|name| {
            let real_index = dirs.iter().position(|dir| {
                let candidate = dir.join(name);
                dir.is_absolute()
                    && canonical(dir) != shim_canonical
                    && is_executable_file(&candidate)
                    && canonical(&candidate) != self_exe
            });
            ToolOrder {
                name: (*name).to_string(),
                shim_index,
                real_index,
                ahead: match (shim_index, real_index) {
                    (Some(shim), Some(real)) => shim < real,
                    (Some(_), None) => true,
                    _ => false,
                },
            }
        })
        .collect();
    let on_path = shim_index.is_some();
    let ahead_of_real_tools = on_path && tools.iter().all(|tool| tool.ahead);
    let counters = intercept_counters(host);
    InterceptStatus {
        installed,
        complete: installed
            && directory_exists
            && missing_shims.is_empty()
            && broken_shims.is_empty()
            && match_file_state == "current",
        path_line: path_line(&shim_dir),
        shim_dir,
        directory_exists,
        match_file,
        match_file_state,
        missing_shims,
        broken_shims,
        on_path,
        ahead_of_real_tools,
        tools,
        counters,
    }
}

fn intercept_counters(host: &Path) -> BTreeMap<String, u64> {
    let mut counters = BTreeMap::new();
    for event in telemetry::read_events(host) {
        if event.command == "intercept" {
            *counters.entry(event.outcome).or_default() += 1;
        }
    }
    counters
}
