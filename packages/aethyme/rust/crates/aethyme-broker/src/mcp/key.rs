//! Which copy of a server a caller gets, and the key that names one server.
//!
//! Agents test servers on branches. An entry names a stable copy (the main
//! checkout, never a session worktree); a caller inside a broker session
//! worktree of the same repository gets that worktree's copy instead when
//! the worktree's `source` differs from the stable one, or when it asks with
//! `--from-worktree`. Otherwise it gets the stable copy.
//!
//! The key hashes what makes two servers interchangeable: the resolved argv
//! and working directory (paths inside the chosen copy are written relative
//! to it, so two worktrees holding the same tree agree), the executable's
//! identity when it lives outside the copy, the env file's content, the
//! `source` tree's content, the allowed variable names, the flavour, mode,
//! transport and, for repository entries, the repository. Sessions whose
//! source trees are identical share a key; different branches get different
//! keys, and so different servers once servers are shared.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::checkouts::{Checkout, Checkouts};
use super::config::{Entry, Mode, Scope, Transport};
use super::resolve::{Resolution, ResolveInput, resolve};

/// What `connect`, `resolve` and `status` read from the caller.
pub struct Caller {
    pub cwd: PathBuf,
    pub path_env: Option<OsString>,
    pub npm_cache: Option<PathBuf>,
    pub from_worktree: bool,
}

impl Caller {
    /// The current process's working directory and environment.
    pub fn current(from_worktree: bool) -> std::io::Result<Self> {
        let cwd = std::env::current_dir()?;
        Ok(Self {
            cwd,
            path_env: std::env::var_os("PATH"),
            npm_cache: super::resolve::npm_cache_dir(
                std::env::var_os("npm_config_cache").as_deref(),
                std::env::var_os("HOME").as_deref(),
            ),
            from_worktree,
        })
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
    pub env_file_identity: Option<String>,
    /// Variables the env file sets; values are never printed.
    #[serde(skip)]
    pub env_file_values: EnvValues,
    pub env_allow: Vec<String>,
    pub source: Option<PathBuf>,
    pub source_identity: Option<String>,
    pub resolution: Resolution,
    pub key: String,
    pub key_short: String,
    pub phases_ms: BTreeMap<&'static str, u64>,
}

/// Env-file assignments. `Debug` shows the count only: these are credentials.
#[derive(Clone, Default)]
pub struct EnvValues(pub Vec<(String, String)>);

impl std::fmt::Debug for EnvValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "EnvValues({} assignments, values redacted)",
            self.0.len()
        )
    }
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

/// Choose the copy, read the env file and source identity, resolve the argv
/// and compute the key.
pub fn plan(
    entry: &Entry,
    caller: &Caller,
    checkouts: &dyn Checkouts,
) -> Result<Plan, Box<PlanFailure>> {
    let started = Instant::now();
    let mut phases: BTreeMap<&'static str, u64> = BTreeMap::new();
    let fail = |code: &'static str, message: String, phases: &BTreeMap<&'static str, u64>| {
        Box::new(PlanFailure {
            code,
            message,
            phases_ms: phases.clone(),
        })
    };

    let phase = Instant::now();
    let copy = choose_copy(entry, caller, checkouts);
    phases.insert("copy_choice", elapsed_ms(phase));
    let copy = copy.map_err(|(code, message)| fail(code, message, &phases))?;

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
            (Some(format!("{:x}", Sha256::digest(&bytes))), values)
        }
        None => (None, Vec::new()),
    };
    let env_file_values = EnvValues(env_file_values);
    phases.insert("env_file", elapsed_ms(phase));

    let phase = Instant::now();
    let source = entry.source.as_deref().map(map);
    let source_identity = match (copy.kind, &source) {
        (CopyKind::Stable, Some(_)) if copy.stable_source_identity.is_some() => {
            copy.stable_source_identity.clone()
        }
        (CopyKind::Worktree, Some(_)) if copy.worktree_source_identity.is_some() => {
            copy.worktree_source_identity.clone()
        }
        (_, Some(path)) => Some(
            tree_identity(path)
                .map_err(|message| fail("source_identity_failed", message, &phases))?,
        ),
        (_, None) => None,
    };
    phases.insert("source_identity", elapsed_ms(phase));

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
    });
    phases.insert("resolve", elapsed_ms(phase));

    let key = compute_key(&KeyInputs {
        entry,
        copy_root: copy.copy_root(),
        resolution: &resolution,
        cwd: &cwd,
        env_file_identity: env_file_identity.as_deref(),
        source_identity: source_identity.as_deref(),
    });
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
        env_file_values,
        env_allow: entry.env_allow.clone(),
        source,
        source_identity,
        resolution,
        key_short: key[..12].to_string(),
        key,
        phases_ms: phases,
    })
}

type ChoiceResult = Result<CopyChoice, (&'static str, String)>;

/// Stable or worktree copy for this caller.
pub fn choose_copy(entry: &Entry, caller: &Caller, checkouts: &dyn Checkouts) -> ChoiceResult {
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
    let Some(source) = entry.source.as_deref() else {
        return stable("no_source_declared", Some(&session), Some(&stable_checkout));
    };
    let Some(worktree_source) = to_worktree(source).filter(|path| path.exists()) else {
        return stable(
            "worktree_copy_missing",
            Some(&session),
            Some(&stable_checkout),
        );
    };
    let stable_identity =
        tree_identity(source).map_err(|message| ("source_identity_failed", message))?;
    let worktree_identity =
        tree_identity(&worktree_source).map_err(|message| ("source_identity_failed", message))?;
    if stable_identity == worktree_identity {
        let mut choice = stable("source_identical", Some(&session), Some(&stable_checkout))?;
        choice.stable_source_identity = Some(stable_identity);
        choice.worktree_source_identity = Some(worktree_identity);
        return Ok(choice);
    }
    Ok(worktree_choice(
        "source_differs",
        Some(stable_identity),
        Some(worktree_identity),
    ))
}

struct KeyInputs<'a> {
    entry: &'a Entry,
    copy_root: Option<&'a Path>,
    resolution: &'a Resolution,
    cwd: &'a Path,
    env_file_identity: Option<&'a str>,
    source_identity: Option<&'a str>,
}

/// `<copy>/relative` for paths inside the chosen copy, else the path.
fn normalized(path: &str, copy_root: Option<&Path>) -> String {
    match copy_root.and_then(|root| Path::new(path).strip_prefix(root).ok()) {
        Some(relative) => format!("<copy>/{}", relative.to_string_lossy()),
        None => path.to_string(),
    }
}

fn compute_key(inputs: &KeyInputs<'_>) -> String {
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
    // Executables outside the copy are not covered by the source identity:
    // an upgraded interpreter is a different server.
    let outside = |path: &Path| copy_root.is_none_or(|root| !path.starts_with(root));
    let executables: Vec<serde_json::Value> = inputs
        .resolution
        .identity
        .iter()
        .flat_map(|identity| std::iter::once(&identity.file).chain(identity.interpreter.as_ref()))
        .filter(|file| outside(&file.path))
        .map(|file| {
            serde_json::json!({
                "path": file.path,
                "size": file.size,
                "mtime_s": file.mtime_s,
                "mtime_nsec": file.mtime_nsec,
            })
        })
        .collect();
    let mut env_allow = inputs.entry.env_allow.clone();
    env_allow.sort();
    env_allow.dedup();
    let material = serde_json::json!({
        "version": 1,
        "mode": inputs.entry.mode,
        "transport": inputs.entry.transport,
        "argv": argv,
        "executables": executables,
        "cwd": normalized(&inputs.cwd.to_string_lossy(), copy_root),
        "env_file": inputs.env_file_identity,
        "env_allow": env_allow,
        "source": inputs.source_identity,
        "flavour": inputs.entry.flavour,
        "repository": inputs.entry.repository.as_ref().map(|anchor| &anchor.identity),
    });
    let bytes = serde_json::to_vec(&material).unwrap_or_default();
    format!("{:x}", Sha256::digest(&bytes))
}

/// Limits for hashing a source tree outside Git.
const WALK_MAX_FILES: usize = 20_000;
const WALK_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Content identity of a file or directory tree.
///
/// Inside a Git checkout: the index entries under the path (blob ids, which
/// are content addresses) plus the content of every changed or untracked,
/// not ignored, file. Elsewhere: every file's relative path and content,
/// skipping `.git` and `node_modules`, within fixed limits.
pub fn tree_identity(path: &Path) -> Result<String, String> {
    let metadata = path
        .metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if metadata.is_file() {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut hasher = Sha256::new();
        hasher.update(b"file-v1\0");
        hasher.update(&bytes);
        return Ok(format!("{:x}", hasher.finalize()));
    }
    let in_git = path.ancestors().any(|dir| dir.join(".git").exists());
    if in_git && let Ok(repository) = crate::GitRepo::discover(path) {
        return git_tree_identity(&repository, path);
    }
    walk_tree_identity(path)
}

fn git_tree_identity(repository: &crate::GitRepo, path: &Path) -> Result<String, String> {
    let root = repository
        .root()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    let relative = canonical
        .strip_prefix(&root)
        .map_err(|_| format!("{} is outside {}", canonical.display(), root.display()))?;
    let pathspec = if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative.to_string_lossy().into_owned()
    };
    let index = repository
        .index_entries_under(&pathspec)
        .map_err(|error| error.to_string())?;
    let status = repository
        .status_entries_under(&pathspec)
        .map_err(|error| error.to_string())?;
    if index.is_empty() && status.is_empty() {
        // Nothing tracked and nothing visible: an ignored directory (build
        // output, a temporary directory inside a checkout). Git says nothing
        // about its content, so hash the files.
        return walk_tree_identity(path);
    }
    let mut hasher = Sha256::new();
    hasher.update(b"git-v1\0");
    hasher.update(&index);
    hasher.update(b"\0status\0");
    let mut records = status
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let (state, changed) = (&record[..2], &record[3..]);
        hasher.update(state);
        hasher.update(changed);
        hasher.update(b"\0");
        if matches!(state[0], b'R' | b'C')
            && let Some(origin) = records.next()
        {
            hasher.update(origin);
            hasher.update(b"\0");
        }
        let file = root.join(String::from_utf8_lossy(changed).as_ref());
        hasher.update(content_digest(&file).as_bytes());
        hasher.update(b"\0");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// A file's content digest, a symlink's target, or a marker.
fn content_digest(path: &Path) -> String {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::read_link(path)
            .map(|target| format!("link:{}", target.to_string_lossy()))
            .unwrap_or_else(|_| "link:?".into()),
        Ok(metadata) if metadata.is_file() => {
            let mut hasher = Sha256::new();
            match std::fs::File::open(path) {
                Ok(mut file) => {
                    let mut buffer = [0u8; 64 * 1024];
                    loop {
                        match file.read(&mut buffer) {
                            Ok(0) => break,
                            Ok(read) => hasher.update(&buffer[..read]),
                            Err(_) => return "unreadable".into(),
                        }
                    }
                    format!("{:x}", hasher.finalize())
                }
                Err(_) => "unreadable".into(),
            }
        }
        Ok(_) => "dir".into(),
        Err(_) => "absent".into(),
    }
}

fn walk_tree_identity(root: &Path) -> Result<String, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut total: u64 = 0;
    while let Some(dir) = pending.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == ".git" || name == "node_modules" {
                continue;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                pending.push(path);
            } else {
                total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
                files.push(path);
            }
            if files.len() > WALK_MAX_FILES || total > WALK_MAX_BYTES {
                return Err(format!(
                    "{} holds more than {WALK_MAX_FILES} files or {} MiB outside Git; declare a \
                     narrower `source`",
                    root.display(),
                    WALK_MAX_BYTES / (1024 * 1024)
                ));
            }
        }
    }
    files.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"walk-v1\0");
    for file in files {
        let relative = file.strip_prefix(root).unwrap_or(&file);
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(content_digest(&file).as_bytes());
        hasher.update(b"\0");
    }
    Ok(format!("{:x}", hasher.finalize()))
}
