//! Launcher chain -> direct argv, offline.
//!
//! `pnpm exec X`, `npm exec X` and `npx [-y] X[@version]` each keep a Node
//! process alive in front of the server. When the package is already
//! installed the launcher adds nothing: this module finds the installed
//! executable and returns an argv that starts it directly.
//!
//! The guarantees:
//! - Nothing is run. The package manager is never executed; files are only
//!   stat'ed and read (`package.json`, a script's first line, a shim).
//! - Nothing is downloaded. Only `node_modules` between the working directory
//!   and its project root (the nearest repository root or directory holding
//!   a lockfile or workspace file), and, for a pinned version or an entry
//!   with `allow_cached`, npm's own `_npx` cache are searched. When nothing
//!   matches, the result says so and keeps the declared argv.
//! - Launchers it does not resolve (`yarn`, `bunx`, `bun x`, `pnpx`,
//!   `pnpm dlx`, `env NAME=VALUE ...`) are reported unresolved, never
//!   executed. Anything else is treated as the program it names.
//! - A script's `#!/usr/bin/env <program>` line is resolved against the
//!   caller's `PATH` now, and a pnpm-style `#!/bin/sh` shim is read to its
//!   target and started as `<node> <target>`, so the server does not need
//!   `PATH` to start.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A file's identity as far as change detection needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    /// Canonical path (symlinks resolved).
    pub path: PathBuf,
    pub size: u64,
    pub mtime_s: i64,
    pub mtime_nsec: i64,
}

impl FileIdentity {
    pub fn of(path: &Path) -> std::io::Result<Self> {
        let canonical = path.canonicalize()?;
        let metadata = canonical.metadata()?;
        Ok(Self {
            path: canonical,
            size: metadata.len(),
            mtime_s: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
        })
    }
}

/// The executable that will run, and for scripts the interpreter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutableIdentity {
    pub file: FileIdentity,
    /// The `#!` line without `#!`, for scripts.
    pub shebang: Option<String>,
    pub interpreter: Option<FileIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// The declared argv starts with the program itself.
    Direct,
    /// `node_modules/.bin/<bin>` in the working directory or an ancestor.
    NodeModulesBin,
    /// `node_modules/<package>` in the working directory or an ancestor.
    InstalledPackage,
    /// npm's `_npx` cache.
    NpxCache,
    Unresolved,
}

/// The installed package a launcher form resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageInfo {
    pub name: String,
    pub requested: Option<String>,
    pub version: Option<String>,
    pub directory: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct Resolution {
    pub declared: Vec<String>,
    /// The direct argv, or the declared argv when unresolved.
    pub argv: Vec<String>,
    pub launcher: Option<&'static str>,
    pub method: Method,
    pub resolved: bool,
    /// Why resolution failed (a stable code), when it did.
    pub reason: Option<&'static str>,
    pub message: Option<String>,
    pub package: Option<PackageInfo>,
    pub notes: Vec<String>,
    pub identity: Option<ExecutableIdentity>,
    /// `NODE_PATH` a parsed pnpm shim would have exported; applied the way
    /// the shim does (prepended to the caller's value).
    pub node_path: Option<String>,
}

/// What resolution reads from the caller.
pub struct ResolveInput<'a> {
    pub argv: &'a [String],
    /// The server's working directory: where `node_modules` is looked for.
    pub cwd: &'a Path,
    /// The caller's `PATH`, for bare program names and `#!/usr/bin/env`.
    pub path_env: Option<&'a OsStr>,
    /// npm's cache directory (`$npm_config_cache`, else `~/.npm`).
    pub npm_cache: Option<&'a Path>,
    /// Whether an unpinned request may use a copy from npm's cache.
    pub allow_cached: bool,
}

struct Failure {
    reason: &'static str,
    message: String,
}

fn failure(reason: &'static str, message: impl Into<String>) -> Failure {
    Failure {
        reason,
        message: message.into(),
    }
}

/// npm's cache directory for a caller environment.
pub fn npm_cache_dir(npm_config_cache: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    npm_config_cache
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".npm"))
        })
}

/// Files that mark a project root for the `node_modules` walk.
pub const PROJECT_MARKERS: &[&str] = &[
    "pnpm-workspace.yaml",
    "pnpm-lock.yaml",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
];

/// The directories from `cwd` up to its project root, nearest first. Without
/// a repository root or project marker above it, `cwd` alone.
pub fn project_directories(cwd: &Path) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    for dir in cwd.ancestors() {
        directories.push(dir.to_path_buf());
        let root = dir.join(".git").exists()
            || PROJECT_MARKERS
                .iter()
                .any(|marker| dir.join(marker).is_file());
        if root {
            return directories;
        }
    }
    vec![cwd.to_path_buf()]
}

enum Launch {
    Direct,
    Bin {
        launcher: &'static str,
        bin: String,
        rest: Vec<String>,
    },
    Package {
        launcher: &'static str,
        spec: PackageSpec,
        rest: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackageSpec {
    pub(crate) name: String,
    pub(crate) version: Option<String>,
}

impl PackageSpec {
    pub(crate) fn bin_name(&self) -> &str {
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }
}

fn program_name(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
}

/// Resolve `input.argv`. Never fails: an unresolvable chain comes back with
/// `resolved: false`, a reason, and the declared argv.
pub fn resolve(input: &ResolveInput<'_>) -> Resolution {
    let declared = input.argv.to_vec();
    // `env cmd ...` runs `cmd`: resolve that. Assignments and options are
    // not interpreted.
    if program_name(&input.argv[0]) == "env" && input.argv.len() > 1 {
        let next = &input.argv[1];
        if next.starts_with('-') || next.contains('=') {
            return unresolved(
                declared,
                None,
                failure(
                    "unsupported_launcher_form",
                    "`env` with options or NAME=VALUE assignments is not resolved; move the \
                     variables to env_file",
                ),
            );
        }
        let mut inner = resolve(&ResolveInput {
            argv: &input.argv[1..],
            cwd: input.cwd,
            path_env: input.path_env,
            npm_cache: input.npm_cache,
            allow_cached: input.allow_cached,
        });
        if !inner.resolved {
            inner.argv = declared.clone();
        }
        inner.declared = declared;
        return inner;
    }
    let launch = match parse_launcher(input.argv) {
        Ok(launch) => launch,
        Err(failure) => return unresolved(declared, None, failure),
    };
    let (launcher, outcome) = match launch {
        Launch::Direct => (None, resolve_direct(input)),
        Launch::Bin {
            launcher,
            bin,
            rest,
        } => (Some(launcher), resolve_bin(input, &bin, &rest)),
        Launch::Package {
            launcher,
            spec,
            rest,
        } => (Some(launcher), resolve_package(input, &spec, &rest)),
    };
    match outcome {
        Ok(found) => Resolution {
            declared,
            argv: found.finalized.argv,
            launcher,
            method: found.method,
            resolved: true,
            reason: None,
            message: None,
            package: found.package,
            notes: found.finalized.notes,
            identity: Some(found.finalized.identity),
            node_path: found.finalized.node_path,
        },
        Err(failure) => unresolved(declared, launcher, failure),
    }
}

fn unresolved(
    declared: Vec<String>,
    launcher: Option<&'static str>,
    failure: Failure,
) -> Resolution {
    Resolution {
        argv: declared.clone(),
        declared,
        launcher,
        method: Method::Unresolved,
        resolved: false,
        reason: Some(failure.reason),
        message: Some(failure.message),
        package: None,
        notes: Vec::new(),
        identity: None,
        node_path: None,
    }
}

fn parse_launcher(argv: &[String]) -> Result<Launch, Failure> {
    let unsupported = |what: &str| {
        Err(failure(
            "unsupported_launcher_form",
            format!(
                "{what} is a package-manager launcher this build does not resolve; it is never \
                 run by resolution"
            ),
        ))
    };
    match program_name(&argv[0]) {
        "pnpm" => {
            if argv.get(1).map(String::as_str) != Some("exec") {
                return unsupported("`pnpm` other than `pnpm exec`");
            }
            let mut index = 2;
            if argv.get(index).map(String::as_str) == Some("--") {
                index += 1;
            }
            match argv.get(index) {
                Some(bin) if !bin.starts_with('-') => Ok(Launch::Bin {
                    launcher: "pnpm exec",
                    bin: bin.clone(),
                    rest: argv[index + 1..].to_vec(),
                }),
                Some(option) => Err(failure(
                    "unsupported_launcher_form",
                    format!("`pnpm exec` option {option} is not resolved"),
                )),
                None => Err(failure(
                    "unsupported_launcher_form",
                    "`pnpm exec` names no binary",
                )),
            }
        }
        "npm" => {
            if argv.get(1).map(String::as_str) != Some("exec") {
                return unsupported("`npm` other than `npm exec`");
            }
            parse_npx_like(argv, 2, "npm exec")
        }
        "npx" => parse_npx_like(argv, 1, "npx"),
        "yarn" => unsupported("`yarn`"),
        "bunx" => unsupported("`bunx`"),
        "pnpx" => unsupported("`pnpx`"),
        "bun" if matches!(argv.get(1).map(String::as_str), Some("x" | "run")) => {
            unsupported("`bun x` / `bun run`")
        }
        _ => Ok(Launch::Direct),
    }
}

fn parse_npx_like(
    argv: &[String],
    start: usize,
    launcher: &'static str,
) -> Result<Launch, Failure> {
    let mut index = start;
    let mut separated = false;
    while let Some(word) = argv.get(index) {
        match word.as_str() {
            "-y" | "--yes" | "--no-install" | "--prefer-offline" | "-q" | "--quiet" => index += 1,
            "--" => {
                index += 1;
                separated = true;
                break;
            }
            option if option.starts_with('-') => {
                return Err(failure(
                    "unsupported_launcher_form",
                    format!("`{launcher}` option {option} is not resolved"),
                ));
            }
            _ => break,
        }
    }
    let Some(word) = argv.get(index) else {
        return Err(failure(
            "unsupported_launcher_form",
            format!("`{launcher}` names no package"),
        ));
    };
    let Some(spec) = parse_spec(word) else {
        return Err(failure(
            "unsupported_launcher_form",
            format!(
                "{word:?} is not a registry package name (paths, URLs and git specs are not resolved)"
            ),
        ));
    };
    let mut rest = argv[index + 1..].to_vec();
    // `npm exec` reads options after the package as its own unless a `--`
    // ends them (and consumes that `--`); `npx` passes everything after the
    // package to the command.
    if launcher == "npm exec" && !separated {
        let end = rest.iter().position(|word| word == "--");
        let before = &rest[..end.unwrap_or(rest.len())];
        if let Some(option) = before.iter().find(|word| word.starts_with('-')) {
            return Err(failure(
                "unsupported_launcher_form",
                format!(
                    "`npm exec` would read {option} as its own option; put the server's \
                     arguments after `--`"
                ),
            ));
        }
        if let Some(end) = end {
            rest.remove(end);
        }
    }
    Ok(Launch::Package {
        launcher,
        spec,
        rest,
    })
}

pub(crate) fn parse_spec(word: &str) -> Option<PackageSpec> {
    let (name, version) = match word.strip_prefix('@') {
        Some(rest) => match rest.split_once('@') {
            Some((name, version)) => (format!("@{name}"), Some(version.to_string())),
            None => (word.to_string(), None),
        },
        None => match word.split_once('@') {
            Some((name, version)) => (name.to_string(), Some(version.to_string())),
            None => (word.to_string(), None),
        },
    };
    let body = name.strip_prefix('@').unwrap_or(&name);
    let scoped_ok = !name.starts_with('@') || body.matches('/').count() == 1;
    let unscoped_ok = name.starts_with('@') || !body.contains('/');
    let chars_ok = !body.is_empty()
        && !body.starts_with('.')
        && body.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.' | '~' | '/')
        });
    (scoped_ok && unscoped_ok && chars_ok).then_some(PackageSpec {
        name,
        version: version.filter(|version| !version.is_empty()),
    })
}

struct Found {
    finalized: Finalized,
    method: Method,
    package: Option<PackageInfo>,
}

fn resolve_direct(input: &ResolveInput<'_>) -> Result<Found, Failure> {
    let program = &input.argv[0];
    let path = if program.contains('/') {
        let path = PathBuf::from(program);
        if path.is_absolute() {
            path
        } else {
            input.cwd.join(path)
        }
    } else {
        search_path(program, input.path_env).ok_or_else(|| {
            failure(
                "program_not_found",
                format!("{program} is not on the caller's PATH"),
            )
        })?
    };
    Ok(Found {
        finalized: finalize(&path, &input.argv[1..], input.path_env)?,
        method: Method::Direct,
        package: None,
    })
}

fn resolve_bin(input: &ResolveInput<'_>, bin: &str, rest: &[String]) -> Result<Found, Failure> {
    let script = find_bin(input.cwd, bin).ok_or_else(|| {
        failure(
            "not_installed",
            format!(
                "no node_modules/.bin/{bin} between {} and its project root; the declared \
                 command is kept (resolution never installs or downloads)",
                input.cwd.display()
            ),
        )
    })?;
    Ok(Found {
        finalized: finalize(&script, rest, input.path_env)?,
        method: Method::NodeModulesBin,
        package: None,
    })
}

fn find_bin(cwd: &Path, bin: &str) -> Option<PathBuf> {
    if bin.contains('/') {
        return None;
    }
    project_directories(cwd)
        .into_iter()
        .map(|dir| dir.join("node_modules").join(".bin").join(bin))
        .find(|candidate| candidate.is_file())
}

enum VersionWant {
    Any,
    Latest,
    Req(semver::VersionReq),
}

fn version_want(requested: Option<&str>) -> Result<VersionWant, Failure> {
    match requested {
        None => Ok(VersionWant::Any),
        Some("latest") => Ok(VersionWant::Latest),
        Some(text) => {
            if semver::Version::parse(text).is_ok() {
                return semver::VersionReq::parse(&format!("={text}"))
                    .map(VersionWant::Req)
                    .map_err(|error| failure("unsupported_version", error.to_string()));
            }
            semver::VersionReq::parse(text)
                .map(VersionWant::Req)
                .map_err(|_| {
                    failure(
                        "unsupported_version",
                        format!(
                            "version {text:?} is a dist-tag or spec that cannot be checked offline"
                        ),
                    )
                })
        }
    }
}

impl VersionWant {
    fn accepts(&self, installed: Option<&str>) -> bool {
        match self {
            VersionWant::Any | VersionWant::Latest => true,
            VersionWant::Req(requirement) => installed
                .and_then(|version| semver::Version::parse(version).ok())
                .is_some_and(|version| requirement.matches(&version)),
        }
    }

    fn pinned(&self) -> bool {
        matches!(self, VersionWant::Req(_))
    }
}

struct Manifest {
    version: Option<String>,
    bin: Option<serde_json::Value>,
}

fn read_manifest(package_dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(package_dir.join("package.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(Manifest {
        version: value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        bin: value.get("bin").cloned(),
    })
}

fn resolve_package(
    input: &ResolveInput<'_>,
    spec: &PackageSpec,
    rest: &[String],
) -> Result<Found, Failure> {
    let want = version_want(spec.version.as_deref())?;
    let mut seen_versions: Vec<String> = Vec::new();

    // node_modules up to the project root, nearest first.
    for dir in project_directories(input.cwd) {
        let package_dir = dir.join("node_modules").join(&spec.name);
        let Some(manifest) = read_manifest(&package_dir) else {
            continue;
        };
        if want.accepts(manifest.version.as_deref()) {
            return found_package(
                input,
                spec,
                &want,
                package_dir,
                manifest,
                Method::InstalledPackage,
                rest,
            );
        }
        seen_versions.extend(manifest.version);
    }

    // A plain `.bin` link still serves an unversioned request.
    if matches!(want, VersionWant::Any)
        && let Some(script) = find_bin(input.cwd, spec.bin_name())
    {
        return Ok(Found {
            finalized: finalize(&script, rest, input.path_env)?,
            method: Method::NodeModulesBin,
            package: None,
        });
    }

    let requested = spec
        .version
        .as_deref()
        .map(|version| format!("@{version}"))
        .unwrap_or_default();

    // npm's npx cache: only for a pinned version, or when the entry allows an
    // arbitrary cached copy for an unpinned one.
    if !want.pinned() && !input.allow_cached {
        return Err(failure(
            "unpinned",
            format!(
                "not resolved: unpinned. {}{requested} has no install under node_modules between \
                 {} and its project root, and the entry neither pins a version nor sets \
                 allow_cached = true, so no cached copy is used; the declared command is kept",
                spec.name,
                input.cwd.display()
            ),
        ));
    }
    if let Some(cache) = input.npm_cache {
        let mut best: Option<(Option<semver::Version>, PathBuf, Manifest)> = None;
        if let Ok(entries) = std::fs::read_dir(cache.join("_npx")) {
            for entry in entries.flatten() {
                let package_dir = entry.path().join("node_modules").join(&spec.name);
                let Some(manifest) = read_manifest(&package_dir) else {
                    continue;
                };
                if !want.accepts(manifest.version.as_deref()) {
                    seen_versions.extend(manifest.version);
                    continue;
                }
                let version = manifest
                    .version
                    .as_deref()
                    .and_then(|version| semver::Version::parse(version).ok());
                let better = best
                    .as_ref()
                    .is_none_or(|(best_version, _, _)| version > *best_version);
                if better {
                    best = Some((version, package_dir, manifest));
                }
            }
        }
        if let Some((_, package_dir, manifest)) = best {
            return found_package(
                input,
                spec,
                &want,
                package_dir,
                manifest,
                Method::NpxCache,
                rest,
            );
        }
    }

    let cache_text = input
        .npm_cache
        .map(|cache| format!(" or {}", cache.join("_npx").display()))
        .unwrap_or_default();
    let versions_text = if seen_versions.is_empty() {
        String::new()
    } else {
        seen_versions.sort();
        seen_versions.dedup();
        format!(
            " (installed versions that do not match: {})",
            seen_versions.join(", ")
        )
    };
    Err(failure(
        "not_installed",
        format!(
            "no installed {}{requested} under node_modules between {} and its project \
             root{cache_text}{versions_text}; the declared command is kept (resolution never \
             installs or downloads)",
            spec.name,
            input.cwd.display(),
        ),
    ))
}

fn found_package(
    input: &ResolveInput<'_>,
    spec: &PackageSpec,
    want: &VersionWant,
    package_dir: PathBuf,
    manifest: Manifest,
    method: Method,
    rest: &[String],
) -> Result<Found, Failure> {
    let script = package_bin(&package_dir, manifest.bin.as_ref(), spec.bin_name())?;
    let mut finalized = finalize(&script, rest, input.path_env)?;
    if matches!(want, VersionWant::Latest) {
        finalized.notes.push(format!(
            "@latest resolved to the installed {}; it is not checked against the registry \
             (resolution never uses the network)",
            manifest.version.as_deref().unwrap_or("copy")
        ));
    }
    Ok(Found {
        finalized,
        method,
        package: Some(PackageInfo {
            name: spec.name.clone(),
            requested: spec.version.clone(),
            version: manifest.version,
            directory: package_dir,
        }),
    })
}

fn package_bin(
    package_dir: &Path,
    bin: Option<&serde_json::Value>,
    preferred: &str,
) -> Result<PathBuf, Failure> {
    let relative = match bin {
        Some(serde_json::Value::String(path)) => path.clone(),
        Some(serde_json::Value::Object(map)) if map.len() == 1 => map
            .values()
            .next()
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| failure("invalid_package_json", "the bin entry is not a path"))?,
        Some(serde_json::Value::Object(map)) => map
            .get(preferred)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                let names: Vec<&str> = map.keys().map(String::as_str).collect();
                failure(
                    "ambiguous_bin",
                    format!(
                        "{} declares several binaries ({}) and none is named {preferred}; declare the \
                         binary path in argv instead",
                        package_dir.display(),
                        names.join(", ")
                    ),
                )
            })?,
        _ => {
            return Err(failure(
                "invalid_package_json",
                format!("{} declares no bin", package_dir.join("package.json").display()),
            ));
        }
    };
    let script = package_dir.join(&relative);
    // The bin must stay inside its package, symlinks resolved.
    let inside = match (package_dir.canonicalize(), script.canonicalize()) {
        (Ok(package), Ok(target)) => target.starts_with(&package),
        _ => false,
    };
    if !inside {
        return Err(failure(
            "bin_outside_package",
            format!(
                "{}: bin {relative:?} is missing or resolves outside the package",
                package_dir.display()
            ),
        ));
    }
    Ok(script)
}

/// The first `PATH` entry holding an executable file called `name`.
pub fn search_path(name: &str, path_env: Option<&OsStr>) -> Option<PathBuf> {
    let path_env = path_env?;
    std::env::split_paths(path_env)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

pub fn is_executable_file(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// The `#!` line of `path` without `#!`, when it has one.
pub fn read_shebang(path: &Path) -> Option<String> {
    let mut buffer = [0u8; 256];
    let mut file = std::fs::File::open(path).ok()?;
    let read = file.read(&mut buffer).ok()?;
    let head = buffer.get(..read)?;
    let line = head.strip_prefix(b"#!")?;
    let end = line
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(line.len());
    let text = String::from_utf8_lossy(&line[..end]);
    let text = text.trim().trim_end_matches('\r').trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Whether a `#!` interpreter is a POSIX shell.
pub fn is_shell(interpreter: &str) -> bool {
    matches!(
        Path::new(interpreter).file_name().and_then(OsStr::to_str),
        Some("sh" | "bash" | "dash")
    )
}

/// What a pnpm-style shim runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimTarget {
    pub target: PathBuf,
    pub node_path: Option<String>,
}

/// Read a package-manager shell shim (the shape pnpm writes into
/// `node_modules/.bin`) to its target. `None` for anything that is not
/// exactly that shape: such a shim is kept and run as it is.
pub fn parse_node_shim(text: &str, shim_dir: &Path) -> Option<ShimTarget> {
    if text.len() > 16 * 1024 {
        return None;
    }
    let mut target = None;
    let mut node_path = None;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if index == 0 && line.starts_with("#!") {
            continue;
        }
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("basedir=")
            || line.starts_with("case ")
            || line == "esac"
            || (line.starts_with('*') && line.ends_with(";;"))
            || line.starts_with("if [ ")
            || line == "else"
            || line == "fi"
            || line.starts_with("exec \"$basedir/node\"")
        {
            continue;
        }
        if let Some(value) = line.strip_prefix("export NODE_PATH=") {
            let value = value.trim_matches('"');
            if node_path.is_none() {
                // The first form is the one used when the caller has none.
                if value.contains('$') || value.contains('`') {
                    return None;
                }
                node_path = Some(value.to_string());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("exec node ") {
            let words: Vec<&str> = rest.split_whitespace().collect();
            let [script, "\"$@\""] = words.as_slice() else {
                return None;
            };
            let relative = script
                .strip_prefix("\"$basedir/")
                .and_then(|value| value.strip_suffix('"'))?;
            if relative.contains('$') || relative.contains('`') || target.is_some() {
                return None;
            }
            target = Some(shim_dir.join(relative));
            continue;
        }
        return None;
    }
    let target = target?;
    target.is_file().then_some(ShimTarget { target, node_path })
}

/// The argv that runs a resolved file, its identity and notes.
#[derive(Debug, Clone)]
pub struct Finalized {
    pub argv: Vec<String>,
    pub identity: ExecutableIdentity,
    pub notes: Vec<String>,
    pub node_path: Option<String>,
}

/// The argv that runs `path` with `rest`, and its identity.
fn finalize(path: &Path, rest: &[String], path_env: Option<&OsStr>) -> Result<Finalized, Failure> {
    let file = FileIdentity::of(path)
        .map_err(|error| failure("program_not_found", format!("{}: {error}", path.display())))?;
    let metadata = file
        .path
        .metadata()
        .map_err(|error| failure("program_not_found", error.to_string()))?;
    if !metadata.is_file() {
        return Err(failure(
            "program_not_found",
            format!("{} is not a file", path.display()),
        ));
    }
    let executable = metadata.permissions().mode() & 0o111 != 0;
    let shown = path.to_string_lossy().into_owned();
    let mut notes = Vec::new();
    let shebang = read_shebang(&file.path);
    let Some(line) = &shebang else {
        if !executable {
            return Err(failure(
                "not_executable",
                format!("{} is neither executable nor a script", path.display()),
            ));
        }
        return Ok(Finalized {
            argv: std::iter::once(shown).chain(rest.iter().cloned()).collect(),
            identity: ExecutableIdentity {
                file,
                shebang: None,
                interpreter: None,
            },
            notes,
            node_path: None,
        });
    };
    let mut words = line.split_whitespace();
    let interpreter = words.next().unwrap_or_default().to_string();
    let mut extra: Vec<String> = words.map(str::to_string).collect();

    if program_name(&interpreter) == "env" {
        if extra.first().map(String::as_str) == Some("-S") {
            extra.remove(0);
        }
        if extra.is_empty() {
            return Err(failure(
                "interpreter_not_found",
                format!("{}: `#!{line}` names no program", path.display()),
            ));
        }
        let program = extra.remove(0);
        let program_path = search_path(&program, path_env).ok_or_else(|| {
            failure(
                "interpreter_not_found",
                format!(
                    "{} starts with `#!{line}` and {program} is not on the caller's PATH",
                    path.display()
                ),
            )
        })?;
        let interpreter = FileIdentity::of(&program_path).ok();
        let mut argv = vec![program_path.to_string_lossy().into_owned()];
        argv.extend(extra);
        argv.push(shown);
        argv.extend(rest.iter().cloned());
        return Ok(Finalized {
            argv,
            identity: ExecutableIdentity {
                file,
                shebang,
                interpreter,
            },
            notes,
            node_path: None,
        });
    }

    let Ok(interpreter_identity) = FileIdentity::of(Path::new(&interpreter)) else {
        return Err(failure(
            "interpreter_not_found",
            format!(
                "{} starts with `#!{line}`, which does not exist",
                path.display()
            ),
        ));
    };

    if is_shell(&interpreter)
        && let Ok(text) = std::fs::read_to_string(&file.path)
        && let Some(shim_dir) = path.parent()
        && let Some(shim) = parse_node_shim(&text, shim_dir)
    {
        // The shim prefers a `node` beside it, as the shim itself does.
        let local = shim_dir.join("node");
        let node = if is_executable_file(&local) {
            Some(local)
        } else {
            search_path("node", path_env)
        };
        if let Some(node) = node
            && let (Ok(target), Ok(node_identity)) =
                (FileIdentity::of(&shim.target), FileIdentity::of(&node))
        {
            notes.push(format!(
                "package-manager shim {} read to its target; node starts it directly",
                path.display()
            ));
            let mut argv = vec![
                node.to_string_lossy().into_owned(),
                shim.target.to_string_lossy().into_owned(),
            ];
            argv.extend(rest.iter().cloned());
            return Ok(Finalized {
                argv,
                identity: ExecutableIdentity {
                    file: target,
                    shebang: Some("node".into()),
                    interpreter: Some(node_identity),
                },
                notes,
                node_path: shim.node_path,
            });
        }
        notes.push("a shell shim whose node is not on the caller's PATH; it is kept".into());
    } else if is_shell(&interpreter) {
        notes.push("a shell shim kept as it is: it execs its target".into());
    }

    let argv = if executable {
        std::iter::once(shown).chain(rest.iter().cloned()).collect()
    } else {
        let mut argv = vec![interpreter];
        argv.extend(extra);
        argv.push(shown);
        argv.extend(rest.iter().cloned());
        argv
    };
    Ok(Finalized {
        argv,
        identity: ExecutableIdentity {
            file,
            shebang,
            interpreter: Some(interpreter_identity),
        },
        notes,
        node_path: None,
    })
}

/// What differs between a remembered identity and the current one.
pub fn identity_changes(
    previous: &ExecutableIdentity,
    current: &ExecutableIdentity,
) -> Vec<&'static str> {
    let mut changes = Vec::new();
    if previous.file.path != current.file.path {
        changes.push("path");
    }
    if previous.file.size != current.file.size
        || previous.file.mtime_s != current.file.mtime_s
        || previous.file.mtime_nsec != current.file.mtime_nsec
    {
        changes.push("content");
    }
    if previous.shebang != current.shebang {
        changes.push("shebang");
    }
    if previous.interpreter != current.interpreter {
        changes.push("interpreter");
    }
    changes
}
