//! Managed MCP servers: host entries agents are made to start through
//! `aethyme mcp connect`.
//!
//! An agent does not choose how a server starts: its host reads a
//! configuration and spawns the command it finds there. Enforcement is
//! therefore two mechanisms that share one matcher, [`MatchSet::managed_match`]:
//! - config repair ([`super::repair`]) rewrites agent configuration entries
//!   whose command matches a managed entry;
//! - launcher interception ([`super::intercept`]) puts shims named after the
//!   package-manager launchers (and each `match_executables` name) first in
//!   `PATH`, so a matching invocation lands in `connect` whatever started it.
//!
//! Only the host file (`<host state>/mcp/services.toml`) can mark an entry
//! managed:
//!
//! ```toml
//! [[servers]]
//! name = "example-devtools"
//! argv = ["npx", "-y", "example-devtools-mcp"]
//! managed = true
//! match_packages = ["example-devtools-mcp"]   # a version suffix is ignored
//! match_executables = ["example-devtools-mcp"] # basenames
//! repair = "auto"                              # or "report"
//! ```
//!
//! The matcher recognises the launcher shapes [`super::resolve`] parses
//! (`npx [-y] X`, `npm exec X`, `pnpm exec X`) and the ones it only reports
//! (`pnpm dlx X`, `pnpx X`, `bunx X`, `bun x X`, `yarn dlx X`), the same
//! behind `env [NAME=VALUE ...]`, behind a `node <launcher script>`
//! interpreter, and inside `sh -c "<one simple command>"` when that string
//! parses without any shell syntax. A direct program matches by basename.
//! `aethyme mcp connect ...` never matches. Nothing is executed or read to
//! decide a match.

use serde::{Deserialize, Serialize};

use super::config::{Loaded, Scope};
use super::resolve::parse_spec;

/// Launchers the interception shims are named after.
pub const LAUNCHERS: &[&str] = &["npx", "npm", "pnpm", "pnpx", "bunx", "yarn"];

/// How far nested wrappers (`env`, `sh -c`, `node <script>`) are followed.
const MAX_WRAPPER_DEPTH: u8 = 4;

/// Whether `aethyme mcp repair --apply --auto` rewrites the entry's matches
/// (`auto`), or only reports them (`report`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairMode {
    #[default]
    Auto,
    Report,
}

impl RepairMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RepairMode::Auto => "auto",
            RepairMode::Report => "report",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(RepairMode::Auto),
            "report" => Some(RepairMode::Report),
            _ => None,
        }
    }
}

/// Which arguments a managed server gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgsMode {
    /// The entry's argv (program resolution and base arguments) followed by
    /// the caller's arguments; they are part of the sharing key.
    #[default]
    Caller,
    /// The entry's argv only. A caller whose arguments differ is not routed
    /// through `connect`.
    Pinned,
}

impl ArgsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ArgsMode::Caller => "caller",
            ArgsMode::Pinned => "pinned",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "caller" => Some(ArgsMode::Caller),
            "pinned" => Some(ArgsMode::Pinned),
            _ => None,
        }
    }
}

/// A validated managed specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedSpec {
    /// Package names without a version.
    pub match_packages: Vec<String>,
    /// Program basenames.
    pub match_executables: Vec<String>,
    pub repair: RepairMode,
    pub args: ArgsMode,
}

/// The managed fields of one raw entry, before validation.
pub struct RawSpec<'a> {
    pub managed: bool,
    pub match_packages: &'a [String],
    pub match_executables: &'a [String],
    pub repair: Option<RepairMode>,
    pub args: Option<ArgsMode>,
}

/// `(field, code, message)` of a refused managed specification.
pub type SpecError = (&'static str, &'static str, String);

/// The package name of a launcher argument or a `match_packages` element,
/// without its version; `None` when it is not a registry package name.
pub fn package_name(word: &str) -> Option<String> {
    parse_spec(word).map(|spec| spec.name)
}

/// The version a package spec asks for (`latest` included).
pub fn package_version(word: &str) -> Option<String> {
    parse_spec(word).and_then(|spec| spec.version)
}

fn valid_executable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

/// Validate the managed fields. `Ok(None)`: the entry is not managed.
pub fn validate_spec(raw: &RawSpec<'_>, scope: Scope) -> Result<Option<ManagedSpec>, SpecError> {
    let any_field = !raw.match_packages.is_empty()
        || !raw.match_executables.is_empty()
        || raw.repair.is_some()
        || raw.args.is_some();
    if scope == Scope::Repository && (raw.managed || any_field) {
        return Err((
            "managed",
            "managed_in_repository",
            "only the host file (<host state>/mcp/services.toml) can mark a server managed; a \
             repository file never can"
                .into(),
        ));
    }
    if !raw.managed {
        if any_field {
            let field = if !raw.match_packages.is_empty() {
                "match_packages"
            } else if !raw.match_executables.is_empty() {
                "match_executables"
            } else if raw.repair.is_some() {
                "repair"
            } else {
                "args"
            };
            return Err((
                field,
                "managed_fields_without_managed",
                format!("{field} has no effect without managed = true"),
            ));
        }
        return Ok(None);
    }
    if raw.match_packages.is_empty() && raw.match_executables.is_empty() {
        return Err((
            "match_packages",
            "managed_without_matchers",
            "a managed entry needs match_packages or match_executables".into(),
        ));
    }
    let mut match_packages = Vec::new();
    for word in raw.match_packages {
        let Some(name) = package_name(word) else {
            return Err((
                "match_packages",
                "invalid_package",
                format!("{word:?} is not a registry package name"),
            ));
        };
        if !match_packages.contains(&name) {
            match_packages.push(name);
        }
    }
    let mut match_executables = Vec::new();
    for name in raw.match_executables {
        if !valid_executable(name) {
            return Err((
                "match_executables",
                "invalid_executable",
                format!(
                    "{name:?} is not a program basename (A-Z, a-z, 0-9, '-', '_', '.', '+'; no \
                     '/')"
                ),
            ));
        }
        if name.starts_with("aethyme") || LAUNCHERS.contains(&name.as_str()) {
            return Err((
                "match_executables",
                "reserved_executable",
                format!(
                    "{name:?} is a launcher or Aethyme itself; launchers are always intercepted \
                     and matched by the package they run"
                ),
            ));
        }
        if !match_executables.contains(name) {
            match_executables.push(name.clone());
        }
    }
    Ok(Some(ManagedSpec {
        match_packages,
        match_executables,
        repair: raw.repair.unwrap_or_default(),
        args: raw.args.unwrap_or_default(),
    }))
}

/// One managed entry as the matcher sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatchEntry {
    pub name: String,
    pub packages: Vec<String>,
    pub executables: Vec<String>,
    pub repair: RepairMode,
    pub args: ArgsMode,
    /// The package version the entry's argv pins (`latest` is no pin).
    pub version: Option<String>,
    /// The entry's own arguments after its package or program.
    pub base_args: Vec<String>,
}

impl MatchEntry {
    /// The pinned version and base arguments an entry argv declares.
    pub fn argv_facts(argv: &[String]) -> (Option<String>, Vec<String>) {
        match classify(argv, 0) {
            Invocation::Launch {
                packages,
                bin: None,
                rest,
                ..
            } => (
                packages
                    .first()
                    .and_then(|spec| package_version(spec))
                    .filter(|version| version != "latest"),
                rest,
            ),
            Invocation::Launch { rest, .. } | Invocation::Direct { rest, .. } => (None, rest),
            Invocation::Connect | Invocation::Unknown => (None, Vec::new()),
        }
    }
}

/// Every managed entry: what the matcher, the shims and repair read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MatchSet {
    pub entries: Vec<MatchEntry>,
}

/// What matched, and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedMatch {
    /// The managed entry's name.
    pub name: String,
    /// `package` or `executable`.
    pub kind: &'static str,
    /// The package or executable name that matched (never an argument value).
    pub matched: String,
    /// The launcher form, when one was recognised.
    pub launcher: Option<&'static str>,
    pub repair: RepairMode,
    /// The caller's arguments after the package or program (never printed).
    #[serde(skip)]
    pub caller_args: Vec<String>,
    /// The caller's package spec as written, when a package matched.
    #[serde(skip)]
    pub caller_spec: Option<String>,
}

/// Arguments that mean a person is asking the program something: such a
/// call is never routed to a server.
pub const INTERACTIVE_ARGUMENTS: &[&str] = &["--help", "-h", "--version"];

/// What to do with a match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Start through `connect` with these caller arguments and, when the
    /// caller pins a version the entry does not, that package spec.
    Connect {
        caller_args: Vec<String>,
        caller_package: Option<String>,
    },
    /// Leave the call alone, for this reason (`interactive_arguments`,
    /// `arguments_differ`, `version_differs`).
    Decline(&'static str),
}

impl MatchSet {
    /// Host entries marked managed. Repository entries never are.
    pub fn from_loaded(loaded: &Loaded) -> Self {
        let entries = loaded
            .entries
            .iter()
            .filter(|entry| entry.scope == Scope::Host)
            .filter_map(|entry| {
                entry.managed.as_ref().map(|spec| {
                    let (version, base_args) = MatchEntry::argv_facts(&entry.argv);
                    MatchEntry {
                        name: entry.name.clone(),
                        packages: spec.match_packages.clone(),
                        executables: spec.match_executables.clone(),
                        repair: spec.repair,
                        args: spec.args,
                        version,
                        base_args,
                    }
                })
            })
            .collect();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every executable name some entry matches, sorted and unique.
    pub fn executables(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .entries
            .iter()
            .flat_map(|entry| entry.executables.iter().cloned())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn find(&self, name: &str) -> Option<&MatchEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// The managed entry `argv` starts, if any. The first entry in file
    /// order wins when several match.
    pub fn managed_match(&self, argv: &[String]) -> Option<ManagedMatch> {
        if self.entries.is_empty() {
            return None;
        }
        let invocation = classify(argv, 0);
        self.match_invocation(&invocation)
    }

    /// Whether and how a match is started through `connect`.
    pub fn route(&self, found: &ManagedMatch) -> Route {
        if found
            .caller_args
            .iter()
            .any(|arg| INTERACTIVE_ARGUMENTS.contains(&arg.as_str()))
        {
            return Route::Decline("interactive_arguments");
        }
        let Some(entry) = self.find(&found.name) else {
            return Route::Decline("unknown_entry");
        };
        let caller_version = found.caller_spec.as_deref().and_then(package_version);
        if let (Some(pinned), Some(asked)) = (&entry.version, &caller_version)
            && pinned != asked
        {
            return Route::Decline("version_differs");
        }
        match entry.args {
            ArgsMode::Pinned if found.caller_args == entry.base_args => Route::Connect {
                caller_args: Vec::new(),
                caller_package: None,
            },
            ArgsMode::Pinned => Route::Decline("arguments_differ"),
            ArgsMode::Caller => Route::Connect {
                caller_args: found.caller_args.clone(),
                caller_package: if entry.version.is_none()
                    && caller_version
                        .as_deref()
                        .is_some_and(|version| version != "latest")
                {
                    found.caller_spec.clone()
                } else {
                    None
                },
            },
        }
    }

    fn match_invocation(&self, invocation: &Invocation) -> Option<ManagedMatch> {
        let found = |entry: &MatchEntry,
                     kind: &'static str,
                     matched: &str,
                     launcher: Option<&'static str>,
                     rest: &[String],
                     spec: Option<&String>| ManagedMatch {
            name: entry.name.clone(),
            kind,
            matched: matched.to_string(),
            launcher,
            repair: entry.repair,
            caller_args: rest.to_vec(),
            caller_spec: spec.cloned(),
        };
        match invocation {
            Invocation::Connect | Invocation::Unknown => None,
            Invocation::Direct { program, rest } => self
                .entries
                .iter()
                .find(|entry| entry.executables.iter().any(|name| name == program))
                .map(|entry| {
                    found(
                        entry,
                        "executable",
                        program.as_str(),
                        None,
                        rest.as_slice(),
                        None,
                    )
                }),
            Invocation::Launch {
                launcher,
                packages,
                bin: None,
                rest,
            } => {
                let spec = packages.first()?;
                let name = package_name(spec)?;
                self.entries
                    .iter()
                    .find(|entry| entry.packages.contains(&name))
                    .map(|entry| {
                        found(
                            entry,
                            "package",
                            &name,
                            Some(*launcher),
                            rest.as_slice(),
                            Some(spec),
                        )
                    })
            }
            Invocation::Launch {
                launcher,
                packages,
                bin: Some(bin),
                rest,
            } => {
                // A named binary matches only by name: one listed in
                // match_executables, or one equal to the unscoped package
                // name when that package is the only one the call installs
                // (`npx -p P P`, `pnpm exec P`). A scoped package's last
                // segment is never taken for its binary.
                if let Some(entry) = self
                    .entries
                    .iter()
                    .find(|entry| entry.executables.iter().any(|name| name == bin))
                {
                    return Some(found(
                        entry,
                        "executable",
                        bin.as_str(),
                        Some(*launcher),
                        rest.as_slice(),
                        None,
                    ));
                }
                if packages.len() > 1 || bin.starts_with('@') {
                    return None;
                }
                if let Some(spec) = packages.first() {
                    let name = package_name(spec)?;
                    if &name != bin {
                        return None;
                    }
                }
                self.entries
                    .iter()
                    .find(|entry| entry.packages.contains(bin))
                    .map(|entry| {
                        found(
                            entry,
                            "package",
                            bin.as_str(),
                            Some(*launcher),
                            rest.as_slice(),
                            packages.first(),
                        )
                    })
            }
        }
    }
}

/// What an argv starts, as far as matching needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Invocation {
    /// `aethyme mcp connect ...`: already managed.
    Connect,
    /// A program, by basename, and its arguments.
    Direct { program: String, rest: Vec<String> },
    /// A launcher: the package specs it installs or runs, the binary it
    /// runs when that is named separately (`pnpm exec X`, `npx -p P X`),
    /// and the arguments after the package or binary.
    Launch {
        launcher: &'static str,
        packages: Vec<String>,
        bin: Option<String>,
        rest: Vec<String>,
    },
    /// A form that cannot be read unambiguously.
    Unknown,
}

fn basename(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// The launcher a `node <script>` interpreter form runs, by the script's
/// basename (what `ps` shows for a launcher started through its shebang).
fn launcher_script(script: &str) -> Option<&'static str> {
    match basename(script) {
        "npx" | "npx-cli.js" => Some("npx"),
        "npm" | "npm-cli.js" => Some("npm"),
        "pnpm" | "pnpm.cjs" | "pnpm.js" => Some("pnpm"),
        "pnpx" | "pnpx.cjs" | "pnpx.js" => Some("pnpx"),
        "yarn" | "yarn.js" | "yarn.cjs" => Some("yarn"),
        "bunx" => Some("bunx"),
        _ => None,
    }
}

fn direct(program: &str, argv: &[String]) -> Invocation {
    Invocation::Direct {
        program: program.to_string(),
        rest: argv[1..].to_vec(),
    }
}

/// Where the launcher form of a top-level argv starts reading: the
/// launcher's name and the index of its first option.
fn launcher_start(argv: &[String]) -> Option<(&'static str, usize)> {
    let word = |index: usize| argv.get(index).map(String::as_str);
    match basename(argv.first()?) {
        "npx" => Some(("npx", 1)),
        "npm" if matches!(word(1), Some("exec" | "x")) => Some(("npm exec", 2)),
        "pnpm" if word(1) == Some("dlx") => Some(("pnpm dlx", 2)),
        "pnpx" => Some(("pnpx", 1)),
        "bunx" => Some(("bunx", 1)),
        "bun" if word(1) == Some("x") => Some(("bun x", 2)),
        "yarn" if word(1) == Some("dlx") => Some(("yarn dlx", 2)),
        _ => None,
    }
}

fn classify(argv: &[String], depth: u8) -> Invocation {
    let Some(first) = argv.first() else {
        return Invocation::Unknown;
    };
    if depth > MAX_WRAPPER_DEPTH {
        return Invocation::Unknown;
    }
    let program = basename(first);
    let word = |index: usize| argv.get(index).map(String::as_str);
    match program {
        "aethyme" if word(1) == Some("mcp") && word(2) == Some("connect") => Invocation::Connect,
        "env" => match strip_env(argv) {
            Some(rest) if !rest.is_empty() => classify(rest, depth + 1),
            _ => Invocation::Unknown,
        },
        "sh" | "bash" | "dash" | "zsh" if word(1) == Some("-c") => {
            if argv.len() != 3 {
                return Invocation::Unknown;
            }
            match simple_command(&argv[2]) {
                Some(words) => classify(&words, depth + 1),
                None => Invocation::Unknown,
            }
        }
        "node" | "nodejs" | "bun" if word(1).and_then(launcher_script).is_some() => {
            let launcher = word(1).and_then(launcher_script).unwrap_or_default();
            let mut inner = vec![launcher.to_string()];
            inner.extend(argv[2..].iter().cloned());
            classify(&inner, depth + 1)
        }
        "pnpm" if word(1) == Some("exec") => pnpm_exec(argv),
        _ => match launcher_start(argv) {
            Some((launcher, start)) => npx_like(argv, start, launcher),
            None => direct(program, argv),
        },
    }
}

/// The command `env [options] [NAME=VALUE ...] command ...` runs; `None`
/// for options it does not read unambiguously.
fn strip_env(argv: &[String]) -> Option<&[String]> {
    let mut index = 1;
    while let Some(word) = argv.get(index) {
        match word.as_str() {
            "-i" | "--ignore-environment" | "-" => index += 1,
            "-u" | "--unset" => index += 2,
            "--" => return argv.get(index + 1..),
            option if option.starts_with("--unset=") => index += 1,
            option if option.starts_with("-u") && option.len() > 2 => index += 1,
            option if option.starts_with('-') => return None,
            assignment if assignment.contains('=') => index += 1,
            _ => return argv.get(index..),
        }
    }
    None
}

/// Launcher options that take no value and change nothing about what runs.
const FLAG_OPTIONS: &[&str] = &[
    "-y",
    "--yes",
    "--no",
    "--no-install",
    "--prefer-offline",
    "--prefer-online",
    "--ignore-existing",
    "-q",
    "--quiet",
    "-s",
    "--silent",
    "--bun",
];

/// A launcher's options read: `--package` values, the index of the first
/// positional word, and whether a `--` came before it.
struct Scanned {
    packages: Vec<String>,
    positional: usize,
    separated: bool,
}

fn scan_launcher(argv: &[String], start: usize) -> Option<Scanned> {
    let mut packages = Vec::new();
    let mut index = start;
    let mut separated = false;
    while let Some(word) = argv.get(index) {
        if let Some(value) = word.strip_prefix("--package=") {
            packages.push(value.to_string());
            index += 1;
            continue;
        }
        match word.as_str() {
            flag if FLAG_OPTIONS.contains(&flag) => index += 1,
            "-p" | "--package" => {
                packages.push(argv.get(index + 1)?.clone());
                index += 2;
            }
            "--" => {
                index += 1;
                separated = true;
                break;
            }
            option if option.starts_with('-') => return None,
            _ => break,
        }
    }
    Some(Scanned {
        packages,
        positional: index,
        separated,
    })
}

/// `npx`, `npm exec`, `pnpm dlx`, `pnpx`, `bunx`, `bun x`, `yarn dlx`:
/// options, then a package spec (or, after `--package`, the binary).
fn npx_like(argv: &[String], start: usize, launcher: &'static str) -> Invocation {
    let Some(scanned) = scan_launcher(argv, start) else {
        return Invocation::Unknown;
    };
    let Some(positional) = argv.get(scanned.positional).cloned() else {
        return Invocation::Unknown;
    };
    let mut rest = argv[scanned.positional + 1..].to_vec();
    // `npm exec` reads options after the package as its own until a `--`,
    // which it consumes; the caller's arguments are what follows.
    if launcher == "npm exec"
        && !scanned.separated
        && let Some(end) = rest.iter().position(|word| word == "--")
    {
        rest.remove(end);
    }
    if scanned.packages.is_empty() {
        Invocation::Launch {
            launcher,
            packages: vec![positional],
            bin: None,
            rest,
        }
    } else {
        Invocation::Launch {
            launcher,
            packages: scanned.packages,
            bin: Some(positional),
            rest,
        }
    }
}

/// `pnpm exec [--] X ...`: X is an installed binary.
fn pnpm_exec(argv: &[String]) -> Invocation {
    let mut index = 2;
    if argv.get(index).map(String::as_str) == Some("--") {
        index += 1;
    }
    match argv.get(index) {
        Some(bin) if !bin.starts_with('-') => Invocation::Launch {
            launcher: "pnpm exec",
            packages: Vec::new(),
            bin: Some(bin.clone()),
            rest: argv[index + 1..].to_vec(),
        },
        _ => Invocation::Unknown,
    }
}

/// An entry argv with the caller's arguments (and package spec) applied:
/// `caller_package` replaces the launcher's package spec (refused when the
/// argv has none at its top level), and `caller_args` are appended, behind
/// a `--` for `npm exec` when the argv has none.
pub fn apply_caller(
    argv: &[String],
    caller_args: &[String],
    caller_package: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut out = argv.to_vec();
    let layout = launcher_start(argv)
        .and_then(|(launcher, start)| scan_launcher(argv, start).map(|scan| (launcher, scan)));
    if let Some(package) = caller_package {
        match &layout {
            Some((_, scanned)) if scanned.packages.is_empty() && scanned.positional < out.len() => {
                out[scanned.positional] = package.to_string();
            }
            _ => {
                return Err(
                    "the entry's argv names no package spec to replace with the caller's version"
                        .into(),
                );
            }
        }
    }
    if let Some(("npm exec", scanned)) = &layout
        && !caller_args.is_empty()
        && !scanned.separated
        && !out[scanned.positional.min(out.len())..]
            .iter()
            .any(|word| word == "--")
    {
        out.push("--".into());
    }
    out.extend(caller_args.iter().cloned());
    Ok(out)
}

/// Split a `sh -c` string that is one simple command: words, single quotes
/// and double quotes without expansions, optionally behind `exec`. Any
/// other shell syntax (operators, redirections, expansions, globs,
/// assignments, comments, escapes) gives `None`: such a string is not read.
pub fn simple_command(text: &str) -> Option<Vec<String>> {
    const SPECIAL: &[char] = &[
        ';', '&', '|', '<', '>', '(', ')', '$', '`', '\\', '*', '?', '[', ']', '{', '}', '~', '#',
        '!', '\n', '\r',
    ];
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => current.push(inner),
                        None => return None,
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('$' | '`' | '\\' | '!') | None => return None,
                        Some(inner) => current.push(inner),
                    }
                }
            }
            other if SPECIAL.contains(&other) => return None,
            other => {
                in_word = true;
                current.push(other);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    if words.first().map(String::as_str) == Some("exec") {
        words.remove(0);
    }
    // `NAME=value cmd` is an assignment prefix: not read.
    if words.first().is_none_or(|word| word.contains('=')) {
        return None;
    }
    Some(words)
}
