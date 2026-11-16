//! Which copy of a server a caller gets, and the key that names one server.
//!
//! Agents test servers on branches. An entry names a stable copy (the main
//! checkout, never a session worktree); a caller inside a broker session
//! worktree of the same repository gets that worktree's copy instead when
//! the worktree's `source` or lockfiles differ from the stable ones, or when
//! it asks with `--from-worktree`. Otherwise it gets the stable copy.
//!
//! The key hashes what makes two servers interchangeable: the resolved argv
//! and working directory (paths inside the chosen copy are written relative
//! to it, so two worktrees holding the same tree agree); the identity of
//! executables outside the copy; the content of the entry script inside it,
//! the resolved package's name and version and the copy's lockfiles; the
//! `source` tree's content; the flavour, mode and transport; for repository
//! entries, the repository; and the values the server receives beyond the
//! default inherited set (`env_allow` values and the env file), as an HMAC
//! under the per-host secret so nothing printed or recorded reveals them.
//! Sessions whose trees are identical share a key; different branches get
//! different keys, and so different servers once servers are shared.
//!
//! In `mode = "direct"` a failed identity never blocks a start: the plan
//! records the failure, marks the key incomplete and goes on.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use super::checkouts::{Checkout, Checkouts};
use super::config::{Entry, EnvInherit, Mode, Scope, Transport, inherited_by_default};
use super::identity::{self, DEFAULT_LIMITS, Limits};
use super::resolve::{Resolution, ResolveInput, resolve};
use super::secret;

/// What `connect`, `resolve` and `status` read from the caller.
pub struct Caller {
    pub cwd: PathBuf,
    pub path_env: Option<OsString>,
    pub npm_cache: Option<PathBuf>,
    pub from_worktree: bool,
    /// The caller's whole environment; the server gets a filtered copy.
    pub env: Vec<(OsString, OsString)>,
    /// The per-host HMAC key for value-derived key parts.
    pub key_secret: Vec<u8>,
    /// Why the per-host secret could not be used, when it could not: the key
    /// then holds per-process random bytes and must never be shared.
    pub key_secret_error: Option<String>,
    /// Where source identities are cached, if anywhere.
    pub identity_cache: Option<PathBuf>,
    pub limits: Limits,
}

impl Caller {
    /// The current process's working directory and environment.
    pub fn current(from_worktree: bool) -> std::io::Result<Self> {
        let cwd = std::env::current_dir()?;
        let host = super::host_directory();
        let (key_secret, key_secret_error) = match host.as_deref() {
            Some(directory) => match secret::load_or_create(directory) {
                Ok(secret) => (secret, None),
                Err(error) => (
                    secret::random_bytes().unwrap_or_default(),
                    Some(error.to_string()),
                ),
            },
            None => (
                secret::random_bytes().unwrap_or_default(),
                Some("the host state directory cannot be resolved".to_string()),
            ),
        };
        Ok(Self {
            cwd,
            path_env: std::env::var_os("PATH"),
            npm_cache: super::resolve::npm_cache_dir(
                std::env::var_os("npm_config_cache").as_deref(),
                std::env::var_os("HOME").as_deref(),
            ),
            from_worktree,
            env: std::env::vars_os().collect(),
            key_secret,
            key_secret_error,
            identity_cache: host.map(|directory| directory.join(identity::CACHE_DIRECTORY)),
            limits: DEFAULT_LIMITS,
        })
    }

    fn variable(&self, name: &str) -> Option<&OsString> {
        self.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyKind {
    Stable,
    Worktree,
}

impl CopyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CopyKind::Stable => "stable",
            CopyKind::Worktree => "worktree",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CopyChoice {
    pub kind: CopyKind,
    /// Why this copy: a stable code.
    pub reason: &'static str,
    /// The caller's session worktree, when it is in one.
    pub session_worktree: Option<PathBuf>,
    /// The checkout holding the entry's stable copy, when it is in one.
    pub stable_root: Option<PathBuf>,
    pub stable_source_identity: Option<String>,
    pub worktree_source_identity: Option<String>,
    /// Why the choice fell back to the stable copy, when it did.
    pub note: Option<String>,
    #[serde(skip)]
    pub identity_cache_hits: u64,
}

impl CopyChoice {
    /// The root that paths in the key are written relative to.
    fn copy_root(&self) -> Option<&Path> {
        match self.kind {
            CopyKind::Stable => self.stable_root.as_deref(),
            CopyKind::Worktree => self.session_worktree.as_deref(),
        }
    }
}

/// Environment for the server. `Debug` shows the count only: values may be
/// credentials.
#[derive(Clone, Default)]
pub struct EnvValues(pub Vec<(OsString, OsString)>);

impl std::fmt::Debug for EnvValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EnvValues({} variables, values redacted)", self.0.len())
    }
}

/// Everything needed to start one server, without starting it.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub name: String,
    pub scope: Scope,
    pub mode: Mode,
    pub transport: Transport,
    pub flavour: String,
    pub copy: CopyChoice,
    /// The server's working directory (the caller's when the entry has none).
    pub cwd: PathBuf,
    pub env_file: Option<PathBuf>,
    /// HMAC of the env file under the per-host secret.
    pub env_file_identity: Option<String>,
    pub env_inherit: EnvInherit,
    pub env_allow: Vec<String>,
    /// Names the server receives; values are never printed.
    pub environment_names: Vec<String>,
    #[serde(skip)]
    pub environment: EnvValues,
    pub source: Option<PathBuf>,
    pub source_identity: Option<String>,
    pub lockfile_identity: Option<String>,
    pub resolution: Resolution,
    pub key: String,
    pub key_short: String,
    /// False when an identity failed in direct mode and was left out.
    pub key_complete: bool,
    pub notes: Vec<String>,
    pub phases_ms: BTreeMap<&'static str, u64>,
}

/// A plan that could not be made.
#[derive(Debug, Clone, Serialize)]
pub struct PlanFailure {
    pub code: &'static str,
    pub message: String,
    pub phases_ms: BTreeMap<&'static str, u64>,
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The server's environment: the default inherited set (unless
/// `env_inherit = "none"`), then `env_allow`, then the env file (which wins),
/// then a parsed shim's `NODE_PATH`, prepended as the shim would.
pub fn server_environment(
    inherit: EnvInherit,
    allow: &[String],
    caller_env: &[(OsString, OsString)],
    file: &[(String, String)],
    node_path: Option<&str>,
) -> Vec<(OsString, OsString)> {
    fn set(environment: &mut Vec<(OsString, OsString)>, name: OsString, value: OsString) {
        environment.retain(|(existing, _)| *existing != name);
        environment.push((name, value));
    }
    let mut environment: Vec<(OsString, OsString)> = Vec::new();
    if inherit == EnvInherit::Default {
        for (name, value) in caller_env {
            if name.to_str().is_some_and(inherited_by_default) {
                set(&mut environment, name.clone(), value.clone());
            }
        }
    }
    for wanted in allow {
        if let Some((name, value)) = caller_env.iter().find(|(name, _)| name == wanted.as_str()) {
            set(&mut environment, name.clone(), value.clone());
        }
    }
    for (name, value) in file {
        set(
            &mut environment,
            OsString::from(name),
            OsString::from(value),
        );
    }
    if let Some(node_path) = node_path {
        let existing = environment
            .iter()
            .find(|(name, _)| name == "NODE_PATH")
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty());
        let mut value = OsString::from(node_path);
        if let Some(existing) = existing {
            value.push(":");
            value.push(existing);
        }
        set(&mut environment, OsString::from("NODE_PATH"), value);
    }
    environment
}

/// Choose the copy, read the env file and identities, resolve the argv and
/// compute the key.
pub fn plan(
    entry: &Entry,
    caller: &Caller,
    checkouts: &dyn Checkouts,
) -> Result<Plan, Box<PlanFailure>> {
    let started = Instant::now();
    let lenient = entry.mode == Mode::Direct;
    let mut phases: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut notes: Vec<String> = Vec::new();
    let mut key_complete = true;
    let fail = |code: &'static str, message: String, phases: &BTreeMap<&'static str, u64>| {
        Box::new(PlanFailure {
            code,
            message,
            phases_ms: phases.clone(),
        })
    };

    let phase = Instant::now();
    let copy = choose_copy(entry, caller, checkouts, lenient);
    phases.insert("copy_choice", elapsed_ms(phase));
    let copy = copy.map_err(|(code, message)| fail(code, message, &phases))?;
    if let Some(note) = &copy.note {
        notes.push(note.clone());
        key_complete = false;
    }

    let map = |path: &Path| -> PathBuf {
        match (copy.kind, &copy.stable_root, &copy.session_worktree) {
            (CopyKind::Worktree, Some(stable), Some(worktree)) => path
                .strip_prefix(stable)
                .map(|relative| worktree.join(relative))
                .unwrap_or_else(|_| path.to_path_buf()),
            _ => path.to_path_buf(),
        }
    };

    let cwd = entry
        .cwd
        .as_deref()
        .map(map)
        .unwrap_or_else(|| caller.cwd.clone());
    if !cwd.is_dir() {
        return Err(fail(
            "missing_cwd",
            format!("working directory {} does not exist", cwd.display()),
            &phases,
        ));
    }

    let phase = Instant::now();
    let env_file = entry.env_file.as_deref().map(map);
    let (env_file_identity, env_file_values) = match &env_file {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|error| {
                fail(
                    "env_file_unreadable",
                    format!("{}: {error}", path.display()),
                    &phases,
                )
            })?;
            let text = String::from_utf8(bytes.clone()).map_err(|_| {
                fail(
                    "env_file_invalid",
                    format!("{} is not UTF-8", path.display()),
                    &phases,
                )
            })?;
            let values = super::envfile::parse(&text).map_err(|error| {
                fail(
                    "env_file_invalid",
                    format!("{}: {error}", path.display()),
                    &phases,
                )
            })?;
            let mut message = b"env-file-v1\0".to_vec();
            message.extend_from_slice(&bytes);
            (Some(secret::hmac_hex(&caller.key_secret, &message)), values)
        }
        None => (None, Vec::new()),
    };
    phases.insert("env_file", elapsed_ms(phase));

    let phase = Instant::now();
    let source = entry.source.as_deref().map(map);
    let mut cache_hits = copy.identity_cache_hits;
    let source_identity = match (copy.kind, &source) {
        (CopyKind::Stable, Some(_)) if copy.stable_source_identity.is_some() => {
            copy.stable_source_identity.clone()
        }
        (CopyKind::Worktree, Some(_)) if copy.worktree_source_identity.is_some() => {
            copy.worktree_source_identity.clone()
        }
        (kind, Some(path)) => {
            // Only the stable copy's identity is cached, and only in direct
            // mode: an unstaged edit changes none of the cache's markers.
            let cache = (lenient && kind == CopyKind::Stable)
                .then_some(caller.identity_cache.as_deref())
                .flatten();
            match identity::tree_identity_cached(path, &caller.limits, cache) {
                Ok((identity, hit)) => {
                    cache_hits += u64::from(hit);
                    Some(identity)
                }
                Err(message) if lenient => {
                    notes.push(format!(
                        "source identity left out of the key (direct mode continues): {message}"
                    ));
                    key_complete = false;
                    None
                }
                Err(message) => return Err(fail("source_identity_failed", message, &phases)),
            }
        }
        (_, None) => None,
    };
    phases.insert("source_identity", elapsed_ms(phase));
    phases.insert("source_identity_cache_hits", cache_hits);

    let phase = Instant::now();
    let declared: Vec<String> = entry
        .argv
        .iter()
        .map(|element| {
            if Path::new(element).is_absolute() {
                map(Path::new(element)).to_string_lossy().into_owned()
            } else {
                element.clone()
            }
        })
        .collect();
    let resolution = resolve(&ResolveInput {
        argv: &declared,
        cwd: &cwd,
        path_env: caller.path_env.as_deref(),
        npm_cache: caller.npm_cache.as_deref(),
        allow_cached: entry.allow_cached,
    });
    phases.insert("resolve", elapsed_ms(phase));

    let environment = server_environment(
        entry.env_inherit,
        &entry.env_allow,
        &caller.env,
        &env_file_values,
        resolution.node_path.as_deref(),
    );
    let environment_names = environment
        .iter()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();

    let phase = Instant::now();
    let copy_root = copy.copy_root().map(Path::to_path_buf);
    let lockfile_identity = identity::lockfile_digest(copy_root.as_deref().unwrap_or(&cwd));
    let key = compute_key(&KeyInputs {
        entry,
        caller,
        copy_root: copy_root.as_deref(),
        resolution: &resolution,
        cwd: &cwd,
        env_file_identity: env_file_identity.as_deref(),
        source_identity: source_identity.as_deref(),
        lockfile_identity: lockfile_identity.as_deref(),
        environment: &environment,
        source: source.as_deref(),
    })
    .map_err(|message| fail("entry_directory_identity_failed", message, &phases))?;
    phases.insert("key", elapsed_ms(phase));
    phases.insert("total", elapsed_ms(started));
    Ok(Plan {
        name: entry.name.clone(),
        scope: entry.scope,
        mode: entry.mode,
        transport: entry.transport,
        flavour: entry.flavour.clone(),
        copy,
        cwd,
        env_file,
        env_file_identity,
        env_inherit: entry.env_inherit,
        env_allow: entry.env_allow.clone(),
        environment_names,
        environment: EnvValues(environment),
        source,
        source_identity,
        lockfile_identity,
        resolution,
        key_short: key[..12].to_string(),
        key,
        key_complete,
        notes,
        phases_ms: phases,
    })
}

type ChoiceResult = Result<CopyChoice, (&'static str, String)>;

/// Stable or worktree copy for this caller. With `lenient` (direct mode), an
/// identity failure falls back to the stable copy with a note.
pub fn choose_copy(
    entry: &Entry,
    caller: &Caller,
    checkouts: &dyn Checkouts,
    lenient: bool,
) -> ChoiceResult {
    let stable = |reason: &'static str,
                  session: Option<&Checkout>,
                  stable_root: Option<&Checkout>|
     -> ChoiceResult {
        Ok(CopyChoice {
            kind: CopyKind::Stable,
            reason,
            session_worktree: session.map(|checkout| checkout.root.clone()),
            stable_root: stable_root.map(|checkout| checkout.root.clone()),
            stable_source_identity: None,
            worktree_source_identity: None,
            note: None,
            identity_cache_hits: 0,
        })
    };
    let refuse_worktree = |message: String| -> ChoiceResult { Err(("no_worktree_copy", message)) };
    let anchor = entry.source.as_deref().or(entry.cwd.as_deref());
    let stable_checkout = anchor.and_then(|path| checkouts.checkout(path));
    let Some(session) = checkouts.session_worktree(&caller.cwd) else {
        if caller.from_worktree {
            return refuse_worktree(format!(
                "--from-worktree: {} is not inside a broker session worktree",
                caller.cwd.display()
            ));
        }
        return stable("not_in_session_worktree", None, stable_checkout.as_ref());
    };
    let Some(stable_checkout) = stable_checkout else {
        if caller.from_worktree {
            return refuse_worktree(
                "--from-worktree: the entry's source and cwd are not in a Git checkout, so it has \
                 no worktree copy"
                    .into(),
            );
        }
        return stable("stable_copy_not_in_checkout", Some(&session), None);
    };
    if !stable_checkout.same_repository(&session) {
        if caller.from_worktree {
            return refuse_worktree(format!(
                "--from-worktree: the session worktree {} belongs to another repository than {}",
                session.root.display(),
                stable_checkout.root.display()
            ));
        }
        return stable("other_repository", Some(&session), Some(&stable_checkout));
    }
    let to_worktree = |path: &Path| {
        path.strip_prefix(&stable_checkout.root)
            .map(|relative| session.root.join(relative))
            .ok()
    };
    let worktree_choice =
        |reason: &'static str,
         stable_identity: Option<String>,
         worktree_identity: Option<String>| CopyChoice {
            kind: CopyKind::Worktree,
            reason,
            session_worktree: Some(session.root.clone()),
            stable_root: Some(stable_checkout.root.clone()),
            stable_source_identity: stable_identity,
            worktree_source_identity: worktree_identity,
            note: None,
            identity_cache_hits: 0,
        };
    let anchor_copy = anchor.and_then(to_worktree);
    if caller.from_worktree {
        return match anchor_copy {
            Some(path) if path.exists() => Ok(worktree_choice("requested", None, None)),
            _ => refuse_worktree(format!(
                "--from-worktree: the session worktree {} has no copy of {}",
                session.root.display(),
                anchor
                    .map(Path::display)
                    .map(|shown| shown.to_string())
                    .unwrap_or_default()
            )),
        };
    }
    // A dependency-only branch differs in its lockfiles, not its source.
    let lockfiles_differ = identity::lockfile_digest(&stable_checkout.root)
        != identity::lockfile_digest(&session.root);
    let Some(source) = entry.source.as_deref() else {
        if lockfiles_differ {
            return Ok(worktree_choice("lockfile_differs", None, None));
        }
        return stable("no_source_declared", Some(&session), Some(&stable_checkout));
    };
    let Some(worktree_source) = to_worktree(source).filter(|path| path.exists()) else {
        return stable(
            "worktree_copy_missing",
            Some(&session),
            Some(&stable_checkout),
        );
    };
    // The worktree side is never cached (an unstaged edit changes none of the
    // cache's markers); the stable side only in direct mode.
    let stable_cache = lenient
        .then_some(caller.identity_cache.as_deref())
        .flatten();
    let identities = identity::tree_identity_cached(source, &caller.limits, stable_cache).and_then(
        |stable_identity| {
            identity::tree_identity_cached(&worktree_source, &caller.limits, None)
                .map(|worktree_identity| (stable_identity, worktree_identity))
        },
    );
    let ((stable_identity, stable_hit), (worktree_identity, worktree_hit)) = match identities {
        Ok(identities) => identities,
        Err(message) if lenient => {
            let mut choice = stable(
                "source_identity_failed",
                Some(&session),
                Some(&stable_checkout),
            )?;
            choice.note = Some(format!(
                "could not compare source trees, so the stable copy is used (direct mode \
                 continues): {message}"
            ));
            return Ok(choice);
        }
        Err(message) => return Err(("source_identity_failed", message)),
    };
    let hits = u64::from(stable_hit) + u64::from(worktree_hit);
    let mut choice = if stable_identity != worktree_identity {
        worktree_choice(
            "source_differs",
            Some(stable_identity),
            Some(worktree_identity),
        )
    } else if lockfiles_differ {
        worktree_choice(
            "lockfile_differs",
            Some(stable_identity),
            Some(worktree_identity),
        )
    } else {
        let mut choice = stable("source_identical", Some(&session), Some(&stable_checkout))?;
        choice.stable_source_identity = Some(stable_identity);
        choice.worktree_source_identity = Some(worktree_identity);
        choice
    };
    choice.identity_cache_hits = hits;
    Ok(choice)
}

struct KeyInputs<'a> {
    entry: &'a Entry,
    caller: &'a Caller,
    copy_root: Option<&'a Path>,
    resolution: &'a Resolution,
    cwd: &'a Path,
    env_file_identity: Option<&'a str>,
    source_identity: Option<&'a str>,
    lockfile_identity: Option<&'a str>,
    /// Every variable the server receives.
    environment: &'a [(OsString, OsString)],
    source: Option<&'a Path>,
}

/// `<copy>/relative` for paths inside the chosen copy, else the path.
fn normalized(path: &str, copy_root: Option<&Path>) -> String {
    match copy_root.and_then(|root| Path::new(path).strip_prefix(root).ok()) {
        Some(relative) => format!("<copy>/{}", relative.to_string_lossy()),
        None => path.to_string(),
    }
}

/// HMAC of the `env_allow` values the caller has, under the host secret.
fn env_values_identity(entry: &Entry, caller: &Caller) -> String {
    let mut names = entry.env_allow.clone();
    names.sort();
    names.dedup();
    let mut message = b"env-values-v1\0".to_vec();
    for name in &names {
        message.extend_from_slice(name.as_bytes());
        message.push(b'=');
        match caller.variable(name) {
            Some(value) => {
                message.push(b'1');
                message.extend_from_slice(value.as_encoded_bytes());
            }
            None => message.push(b'0'),
        }
        message.push(0);
    }
    secret::hmac_hex(&caller.key_secret, &message)
}

/// HMAC of every variable the server receives, names and values, sorted.
fn environment_identity(environment: &[(OsString, OsString)], secret: &[u8]) -> String {
    let mut pairs: Vec<&(OsString, OsString)> = environment.iter().collect();
    pairs.sort();
    let mut message = b"environment-v1\0".to_vec();
    for (name, value) in pairs {
        message.extend_from_slice(name.as_encoded_bytes());
        message.push(b'=');
        message.extend_from_slice(value.as_encoded_bytes());
        message.push(0);
    }
    secret::hmac_hex(secret, &message)
}

fn compute_key(inputs: &KeyInputs<'_>) -> Result<String, String> {
    let shared = inputs.entry.mode == Mode::Shared;
    let copy_root = inputs.copy_root;
    let argv: Vec<String> = inputs
        .resolution
        .argv
        .iter()
        .map(|element| {
            if Path::new(element).is_absolute() {
                normalized(element, copy_root)
            } else {
                element.clone()
            }
        })
        .collect();
    let inside = |path: &Path| copy_root.is_some_and(|root| path.starts_with(root));
    // Executables outside the copy are not covered by any tree identity: an
    // upgraded interpreter is a different server.
    let executables: Vec<serde_json::Value> = inputs
        .resolution
        .identity
        .iter()
        .flat_map(|identity| std::iter::once(&identity.file).chain(identity.interpreter.as_ref()))
        .filter(|file| !inside(&file.path))
        .map(|file| {
            serde_json::json!({
                "path": file.path,
                "size": file.size,
                "mtime_s": file.mtime_s,
                "mtime_nsec": file.mtime_nsec,
            })
        })
        .collect();
    // The entry script inside the copy may be ignored build output that no
    // source identity covers: hash its content.
    let entry_script = inputs
        .resolution
        .identity
        .as_ref()
        .map(|identity| &identity.file.path)
        .filter(|path| inside(path))
        .map(|path| identity::content_digest(path));
    // Shared servers also hash the build output directory holding the entry
    // file when no source identity covers it (capped like the file walk).
    let entry_directory = match (
        shared,
        inputs
            .resolution
            .identity
            .as_ref()
            .map(|identity| identity.file.path.as_path())
            .filter(|path| inside(path)),
    ) {
        (true, Some(path)) if inputs.source.is_none_or(|source| !path.starts_with(source)) => {
            match path.parent() {
                Some(directory) => Some(identity::walk_tree_identity(
                    directory,
                    &inputs.caller.limits,
                )?),
                None => None,
            }
        }
        _ => None,
    };
    // Shared servers depend on everything they receive (HOME, XDG_*, PATH
    // select credentials and configuration); direct ones on env_allow.
    let env_values = if shared {
        environment_identity(inputs.environment, &inputs.caller.key_secret)
    } else {
        env_values_identity(inputs.entry, inputs.caller)
    };
    let package = inputs
        .resolution
        .package
        .as_ref()
        .map(|package| serde_json::json!({"name": package.name, "version": package.version}));
    let material = serde_json::json!({
        "version": 2,
        "mode": inputs.entry.mode,
        "transport": inputs.entry.transport,
        "argv": argv,
        "executables": executables,
        "entry_script": entry_script,
        "entry_directory": entry_directory,
        "package": package,
        "lockfiles": inputs.lockfile_identity,
        "cwd": normalized(&inputs.cwd.to_string_lossy(), copy_root),
        "env_inherit": inputs.entry.env_inherit,
        "env_file": inputs.env_file_identity,
        "env_values": env_values,
        "source": inputs.source_identity,
        "flavour": inputs.entry.flavour,
        "repository": inputs.entry.repository.as_ref().map(|anchor| &anchor.identity),
    });
    let bytes = serde_json::to_vec(&material).unwrap_or_default();
    Ok(secret::hmac_hex(&inputs.caller.key_secret, &bytes))
}
