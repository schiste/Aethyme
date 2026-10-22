//! Deterministic removal of disposable session checkouts (#588).
//!
//! The unattended sweep already reclaims build caches. This removes the
//! checkout itself, without an operator confirming a plan, but only when four
//! proofs hold, each re-checked under the GC lock immediately before the
//! directory goes:
//!
//! 1. **Sessions.** Every broker session that names the worktree is closed.
//!    A session still open in any state -- active, idle, stale, exited --
//!    keeps it.
//! 2. **No use.** No process has a file or working directory open in it, no
//!    lease that still holds covers it (a lease whose holder is gone past its
//!    grace does not hold, one of unknown liveness does), and no gate runs.
//! 3. **Clean.** No tracked, staged or untracked change, no stash made on the
//!    branch, no rebase, merge, cherry-pick, revert or bisect in progress, and
//!    no ignored file outside the regenerable set. A `.env` or a kept data
//!    file is ignored by Git and still somebody's work.
//! 4. **Contained.** Every commit is in the *fetched remote* default branch,
//!    by ancestry or by the verbatim content/patch proof. A commit that only
//!    reached local integration or a local branch is not contained.
//!
//! Anything that cannot be decided -- a listing that failed, a proof the
//! budget cut, a missing remote -- keeps the checkout and says why. Removal
//! never rests on the absence of evidence. Removal deletes the worktree and
//! its session branch and records `broker.cleanup.auto_removed` with the
//! proof; there is nothing to restore, because by proof the commits are on the
//! remote.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::types::{Session, SessionCleanupState, SessionOrigin};
use crate::{Broker, BrokerOpError, GitRepo};

/// Build-output directories, each recognised by its exact name **and** the
/// evidence that proves what it is (#588). A name alone proves nothing:
/// `build/` and `dist/` routinely hold source, assets or data that a
/// repository happens to ignore, so they count only beside the manifest that
/// produces them.
///
/// | Directory | Counts only when |
/// | --- | --- |
/// | `node_modules` | always (package manager output) |
/// | `.pnpm-store` | nonempty package-manager store (shared artifact witness) |
/// | `target` | a sibling `Cargo.toml`, or `CACHEDIR.TAG` inside |
/// | `.venv` | `pyvenv.cfg` inside |
/// | `build`, `dist` | a sibling `package.json`, `setup.py`, `pyproject.toml` or `CMakeLists.txt` |
/// | `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache` | always (tool caches) |
///
/// Names are compared as whole path components: `target-notes/`,
/// `my_build/` and `targets.json` are not build output.
pub const BUILT_IN_REGENERABLE: &[&str] = &[
    "node_modules",
    ".pnpm-store",
    "target",
    ".venv",
    "build",
    "dist",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
];

/// Manifests whose presence beside `build/` or `dist/` proves it is output.
const BUILD_MANIFESTS: &[&str] = &[
    "package.json",
    "setup.py",
    "pyproject.toml",
    "CMakeLists.txt",
];

/// Most entries a single regenerable directory may hold before it counts as
/// unclassifiable. Classification walks it looking for sensitive files and
/// escaping symlinks; a tree too large to walk is kept, not assumed safe.
const CLASSIFY_ENTRY_CAP: usize = 300_000;

/// Paths a configured `regenerable` glob must never match: secrets, data and
/// ordinary root files. A glob that matches any of them is rejected at load.
const GLOB_PROBES: &[&str] = &[
    ".env",
    ".env.local",
    "secrets.key",
    "cert.pem",
    "data.sqlite",
    "app.db",
    "notes.txt",
    "README.md",
    ".aethyme",
];

/// Meta key holding the last auto-cleanup report, which `status` and
/// `gc plan` show.
pub(crate) const AUTO_CLEANUP_LAST_REPORT_KEY: &str = "auto_cleanup.last_report";
const AUTO_CLEANUP_CURSOR_KEY: &str = "auto_cleanup.cursor";
const AUTO_CLEANUP_CURSOR_VERSION: u32 = 1;

/// Durable scheduling state for closed-checkout cleanup. It grants no
/// authority to remove a checkout; every candidate still passes the regular
/// ownership, lifecycle, cleanliness and process-use proofs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct AutoCleanupCursor {
    version: u32,
    after_session_id: i64,
    high_water_session_id: i64,
}

impl Default for AutoCleanupCursor {
    fn default() -> Self {
        Self {
            version: AUTO_CLEANUP_CURSOR_VERSION,
            after_session_id: 0,
            high_water_session_id: 0,
        }
    }
}

fn decode_auto_cleanup_cursor(value: Option<&str>) -> AutoCleanupCursor {
    value
        .and_then(|value| serde_json::from_str::<AutoCleanupCursor>(value).ok())
        .filter(|cursor| {
            cursor.version == AUTO_CLEANUP_CURSOR_VERSION
                && cursor.after_session_id >= 0
                && cursor.high_water_session_id >= cursor.after_session_id
        })
        .unwrap_or_default()
}

fn candidate_cursor_session_id(candidate: &AutoCleanupCandidate) -> i64 {
    candidate
        .sessions
        .iter()
        .map(|session| session.id)
        .min()
        .unwrap_or(0)
}

/// Sort candidates by their minimum session ID and return the unvisited
/// suffix of the current lap. The fixed high-water mark keeps new sessions
/// from extending a lap forever; a completed lap starts again from the
/// beginning and includes arrivals since the prior snapshot.
fn auto_cleanup_lap_range<T>(
    candidates: &mut [T],
    cursor: &mut AutoCleanupCursor,
    session_id: impl Fn(&T) -> i64,
) -> std::ops::Range<usize> {
    candidates.sort_by_key(|candidate| session_id(candidate));
    let current_high_water = candidates.last().map(&session_id).unwrap_or(0);
    let current_lap_has_work = cursor.high_water_session_id > cursor.after_session_id
        && candidates.iter().any(|candidate| {
            let id = session_id(candidate);
            id > cursor.after_session_id && id <= cursor.high_water_session_id
        });
    if !current_lap_has_work {
        cursor.after_session_id = 0;
        cursor.high_water_session_id = current_high_water;
    }

    let start = candidates
        .iter()
        .position(|candidate| {
            let id = session_id(candidate);
            id > cursor.after_session_id && id <= cursor.high_water_session_id
        })
        .unwrap_or(candidates.len());
    let end = candidates
        .partition_point(|candidate| session_id(candidate) <= cursor.high_water_session_id);
    start..end
}

/// `[cleanup]` in `.aethyme/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoCleanupPolicy {
    /// On by default. `false` turns automatic checkout removal off.
    pub auto_remove: bool,
    /// Extra globs, relative to the worktree root, that count as regenerable
    /// in addition to [`BUILT_IN_REGENERABLE`].
    pub regenerable: Vec<String>,
    /// Globs matched against the worktree path, or its final component, that
    /// are never removed automatically.
    pub keep: Vec<String>,
}

impl Default for AutoCleanupPolicy {
    fn default() -> Self {
        Self {
            auto_remove: true,
            regenerable: Vec::new(),
            keep: Vec::new(),
        }
    }
}

impl AutoCleanupPolicy {
    /// Read `[cleanup]`. A missing file or table is the default policy; an
    /// unreadable one is an error, which the sweep turns into "removal off"
    /// rather than guessing what the operator meant.
    pub fn load(main_root: &Path) -> Result<Self, String> {
        let path = main_root.join(".aethyme/config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        let root: toml::Value = toml::from_str(&text)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        let Some(table) = root.get("cleanup") else {
            return Ok(Self::default());
        };
        let table = table
            .as_table()
            .ok_or_else(|| "[cleanup] must be a table".to_string())?;
        let mut policy = Self::default();
        for (key, value) in table {
            match key.as_str() {
                "auto_remove" => {
                    policy.auto_remove = value
                        .as_bool()
                        .ok_or_else(|| "[cleanup] auto_remove must be true or false".to_string())?;
                }
                "regenerable" | "keep" => {
                    let list = value
                        .as_array()
                        .and_then(|items| {
                            items
                                .iter()
                                .map(|item| item.as_str().map(str::to_string))
                                .collect::<Option<Vec<_>>>()
                        })
                        .ok_or_else(|| format!("[cleanup] {key} must be a list of strings"))?;
                    for glob in &list {
                        let matcher = globset::Glob::new(glob)
                            .map_err(|error| format!("[cleanup] {key} glob {glob:?}: {error}"))?
                            .compile_matcher();
                        if key == "regenerable"
                            && let Some(probe) =
                                GLOB_PROBES.iter().find(|probe| matcher.is_match(probe))
                        {
                            return Err(format!(
                                "[cleanup] regenerable glob {glob:?} matches {probe:?}; it may only name build-output directories"
                            ));
                        }
                    }
                    if key == "regenerable" {
                        policy.regenerable = list;
                    } else {
                        policy.keep = list;
                    }
                }
                other => return Err(format!("[cleanup] has no key {other:?}")),
            }
        }
        Ok(policy)
    }

    fn glob_set(globs: &[String]) -> globset::GlobSet {
        let mut builder = globset::GlobSetBuilder::new();
        for glob in globs {
            if let Ok(glob) = globset::Glob::new(glob) {
                builder.add(glob);
            }
        }
        builder
            .build()
            .unwrap_or_else(|_| globset::GlobSet::empty())
    }

    /// Whether `path` (or its final component) matches a `keep` pin.
    pub fn is_pinned(&self, worktree: &Path) -> bool {
        if self.keep.is_empty() {
            return false;
        }
        let set = Self::glob_set(&self.keep);
        set.is_match(worktree)
            || worktree
                .file_name()
                .is_some_and(|name| set.is_match(Path::new(name)))
    }

    /// Whether a configured `regenerable` glob names this directory,
    /// relative to the worktree root. Configured globs only ever apply to
    /// directories, never to files.
    fn configured_regenerable_dir(&self, relative_dir: &str) -> bool {
        !self.regenerable.is_empty() && Self::glob_set(&self.regenerable).is_match(relative_dir)
    }
}

/// A file name that is somebody's secret or data, wherever it sits.
fn is_sensitive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(".env")
        || [".key", ".pem", ".sqlite", ".sqlite3", ".db"]
            .iter()
            .any(|suffix| lower.ends_with(suffix))
}

/// Whether `relative_dir` (no trailing slash) is a recognised build-output
/// directory of `worktree`, by [`BUILT_IN_REGENERABLE`]'s evidence rules or a
/// configured glob. `Some(true)` marks a Cargo target directory, where the
/// compiler's own `.aethyme`-named fingerprints are not Aethyme state.
fn regenerable_dir(
    worktree: &Path,
    relative_dir: &str,
    policy: &AutoCleanupPolicy,
) -> Option<bool> {
    let path = worktree.join(relative_dir);
    let name = Path::new(relative_dir).file_name()?.to_str()?;
    let parent = path.parent()?;
    let recognised = match name {
        "node_modules" | "__pycache__" | ".pytest_cache" | ".mypy_cache" | ".ruff_cache" => {
            Some(false)
        }
        "target" if parent.join("Cargo.toml").is_file() || path.join("CACHEDIR.TAG").is_file() => {
            Some(true)
        }
        ".venv" if path.join("pyvenv.cfg").is_file() => Some(false),
        "build" | "dist"
            if BUILD_MANIFESTS
                .iter()
                .any(|manifest| parent.join(manifest).is_file()) =>
        {
            Some(false)
        }
        // Share the artifact-sweep catalog rather than silently leaving a
        // recognized package-manager store behind in whole-checkout cleanup.
        // The recursive safety scan still runs after this classification.
        _ => crate::artifact_catalog::unattended_witness(name, &[])
            .filter(|witness| witness.confirms(&path))
            .map(|_| false),
    };
    recognised.or_else(|| {
        policy
            .configured_regenerable_dir(relative_dir)
            .then_some(false)
    })
}

/// The nearest enclosing build-output directory of an ignored path, as
/// `(relative_dir, is_cargo_target)`.
fn enclosing_regenerable(
    worktree: &Path,
    relative: &str,
    policy: &AutoCleanupPolicy,
) -> Option<(String, bool)> {
    let components = relative
        .trim_end_matches('/')
        .split('/')
        .collect::<Vec<_>>();
    (1..=components.len()).find_map(|depth| {
        let prefix = components[..depth].join("/");
        regenerable_dir(worktree, &prefix, policy).map(|cargo| (prefix, cargo))
    })
}

/// Walk a build-output directory without following symlinks. Anything that
/// is somebody's -- a sensitive file, Aethyme state, a symlink that leaves the
/// worktree -- or anything that cannot be read, keeps the checkout.
fn scan_regenerable_dir(
    worktree: &Path,
    relative_dir: &str,
    cargo_target: bool,
    budget: &mut usize,
) -> Result<(), String> {
    let root = std::fs::canonicalize(worktree)
        .map_err(|error| format!("cannot resolve {}: {error}", worktree.display()))?;
    let mut stack = vec![worktree.join(relative_dir)];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
            if *budget == 0 {
                return Err(format!(
                    "{relative_dir} holds more than {CLASSIFY_ENTRY_CAP} entries; too large to classify"
                ));
            }
            *budget -= 1;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let display = path
                .strip_prefix(worktree)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let kind = entry
                .file_type()
                .map_err(|error| format!("cannot stat {display}: {error}"))?;
            if kind.is_symlink() {
                let target = std::fs::canonicalize(&path)
                    .map_err(|_| format!("symlink {display} cannot be resolved"))?;
                if !target.starts_with(&root) {
                    return Err(format!(
                        "symlink {display} points outside the worktree, to {}",
                        target.display()
                    ));
                }
                continue;
            }
            if name == ".aethyme" && !cargo_target {
                return Err(format!("{display} holds Aethyme state"));
            }
            if is_sensitive_name(&name) {
                return Err(format!("{display} looks like a secret or data file"));
            }
            if kind.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(())
}

/// Whether one ignored entry (as `git ls-files --directory` prints it, with a
/// trailing `/` for a directory) is disposable: `Ok(())` when it is Finder
/// metadata or lies inside a recognised build-output directory and holds
/// nothing of anyone's; otherwise the reason it keeps the checkout.
pub(crate) fn classify_ignored(
    worktree: &Path,
    relative: &str,
    policy: &AutoCleanupPolicy,
    budget: &mut usize,
) -> Result<(), String> {
    let is_dir = relative.ends_with('/');
    let trimmed = relative.trim_end_matches('/');
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if !is_dir && name == ".DS_Store" {
        return Ok(());
    }
    let path = worktree.join(trimmed);
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("cannot stat ignored path {relative}: {error}"))?;
    let Some((_enclosing, cargo_target)) = enclosing_regenerable(worktree, trimmed, policy) else {
        return Err(format!(
            "ignored path {relative} is not inside a recognised build-output directory"
        ));
    };
    if trimmed.split('/').any(|component| component == ".aethyme") && !cargo_target {
        return Err(format!("{relative} holds Aethyme state"));
    }
    if metadata.file_type().is_symlink() {
        let root = std::fs::canonicalize(worktree)
            .map_err(|error| format!("cannot resolve worktree: {error}"))?;
        let target = std::fs::canonicalize(&path)
            .map_err(|_| format!("symlink {relative} cannot be resolved"))?;
        if !target.starts_with(&root) {
            return Err(format!("symlink {relative} points outside the worktree"));
        }
        return Ok(());
    }
    if metadata.is_dir() {
        return scan_regenerable_dir(worktree, trimmed, cargo_target, budget);
    }
    if is_sensitive_name(name) {
        return Err(format!("{relative} looks like a secret or data file"));
    }
    // A plain file inside a recognised build-output directory.
    Ok(())
}

/// One checkout auto-cleanup removed, with its proof.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoRemovedCheckout {
    pub worktree: String,
    pub sessions: Vec<i64>,
    pub branch: Option<String>,
    pub head: String,
    /// The remote ref the commits were proved against.
    pub contained_in: String,
    /// The upstream commit that contains the head: the ref tip for an
    /// ancestry proof, the landing commit for a content or patch proof.
    pub containing_commit: String,
    /// `ancestry`, `content`, `patch equivalence` or `no net change`.
    pub proof: String,
    /// Bytes the last recorded measurement held for the worktree, when one
    /// exists. Removal does not walk the tree to count them.
    pub bytes: Option<u64>,
}

/// One candidate auto-cleanup kept, and why.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoKeptCheckout {
    pub worktree: String,
    pub sessions: Vec<i64>,
    pub reason: String,
}

/// What one auto-cleanup pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoCleanupReport {
    /// `false` when `[cleanup] auto_remove = false` or the section is invalid.
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
    pub evaluated_at_ms: i64,
    pub removed: Vec<AutoRemovedCheckout>,
    pub kept: Vec<AutoKeptCheckout>,
    /// Candidates, or one incomplete discovery pass, left for later because
    /// the cursor or time budget did not allow a complete proof (#460).
    pub deferred: usize,
}

/// A closed checkout and every session that names it.
#[derive(Debug, Clone)]
pub(crate) struct AutoCleanupCandidate {
    pub worktree: PathBuf,
    /// The retained spawned session whose creation record proves this path
    /// belongs to the broker. Adopted sessions can refer to the same checkout
    /// without being ownership evidence.
    pub spawned_owner: Session,
    /// The instance identity captured from the GitRepo returned by spawn and
    /// committed with `spawned_owner`. `None` is diagnostic-only and is
    /// refused before containment or removal.
    pub spawned_instance_witness: Option<crate::events::SpawnedWorktreeInstanceWitness>,
    pub sessions: Vec<Session>,
}

impl AutoCleanupCandidate {
    fn session_ids(&self) -> Vec<i64> {
        self.sessions.iter().map(|session| session.id).collect()
    }

    fn branch(&self) -> &str {
        self.spawned_owner.branch.as_str()
    }
}

/// Checkouts selected for removal, not yet removed.
#[derive(Debug, Clone)]
pub struct AutoCleanupPlan {
    report: AutoCleanupReport,
    policy: AutoCleanupPolicy,
    upstream: Option<(String, String)>,
    selected: Vec<(AutoCleanupCandidate, ContainmentProof)>,
    /// The selection's deadline, which removal honors too: a pass inside a
    /// broker open must not pay unbounded removals after a bounded selection.
    deadline: Option<Instant>,
}

impl AutoCleanupPlan {
    fn nothing(report: AutoCleanupReport) -> Self {
        Self {
            report,
            policy: AutoCleanupPolicy::default(),
            upstream: None,
            selected: Vec::new(),
            deadline: None,
        }
    }

    /// The worktrees this plan would remove if nothing changes.
    pub fn selected_worktrees(&self) -> Vec<String> {
        self.selected
            .iter()
            .map(|(candidate, _)| candidate.worktree.to_string_lossy().into_owned())
            .collect()
    }

    /// Replace the deadline removal honors.
    ///
    /// Production plans carry the selection's deadline. This hook lets tests
    /// put removal past its budget without racing the wall clock.
    #[doc(hidden)]
    pub fn with_deadline(mut self, deadline: Option<Instant>) -> Self {
        self.deadline = deadline;
        self
    }
}

/// The proof a candidate passed, as of one evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContainmentProof {
    pub head: String,
    pub containing_commit: String,
    pub proof: String,
}

/// Why a candidate is not removed in this pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotRemoved {
    Keep(String),
    /// The deep proof needs time this pass does not have.
    Defer,
}

/// The canonical git directory a checkout's `.git` resolves to. A missing
/// `.git` target is known absence; permission, malformed metadata and other
/// I/O failures remain unknown so they cannot authorize cleanup.
fn checkout_git_dir_checked(worktree: &Path) -> Result<Option<PathBuf>, std::io::Error> {
    let dot_git = worktree.join(".git");
    let metadata = match std::fs::metadata(&dot_git) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let target = if metadata.is_dir() {
        dot_git
    } else if metadata.is_file() {
        let text = match std::fs::read_to_string(&dot_git) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let target = text
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "malformed .git file without a gitdir target",
                )
            })?;
        let target = Path::new(target);
        if target.is_absolute() {
            target.to_path_buf()
        } else {
            worktree.join(target)
        }
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            ".git is neither a directory nor a gitdir file",
        ));
    };

    match std::fs::canonicalize(target) {
        Ok(target) => Ok(Some(target)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Debug, Clone)]
struct ResolvedWorktreePath {
    raw: PathBuf,
    canonical_or_raw: PathBuf,
    git_dir: Option<PathBuf>,
    resolution_errors: Vec<String>,
}

impl ResolvedWorktreePath {
    fn resolve(path: &Path) -> Self {
        let (canonical_or_raw, path_error) = match std::fs::canonicalize(path) {
            Ok(path) => (path, None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Preserve the prior raw-path fallback for a genuinely missing
                // live checkout while keeping non-missing errors distinguishable.
                (path.to_path_buf(), None)
            }
            Err(error) => (
                path.to_path_buf(),
                Some(format!("worktree path could not be resolved: {error}")),
            ),
        };
        let (git_dir, git_error) = match checkout_git_dir_checked(path) {
            Ok(git_dir) => (git_dir, None),
            Err(error) => (
                None,
                Some(format!("Git directory could not be resolved: {error}")),
            ),
        };
        let resolution_errors = path_error.into_iter().chain(git_error).collect();
        Self {
            raw: path.to_path_buf(),
            canonical_or_raw,
            git_dir,
            resolution_errors,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveOverlapEvidence {
    Disjoint,
    Overlap,
    Unknown,
}

fn deadline_expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

/// Resolve every live path once, retaining the store's order so a candidate
/// reports the same first matching session as the former per-pair check. An
/// expired or partial index is never used to authorize cleanup.
fn resolve_live_worktree_paths(
    live: &[Session],
    deadline: Option<Instant>,
) -> Option<Vec<ResolvedWorktreePath>> {
    resolve_worktree_paths_until(
        live.iter().map(|session| Path::new(&session.worktree_path)),
        || deadline_expired(deadline),
        ResolvedWorktreePath::resolve,
    )
}

fn resolve_worktree_paths_until<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
    mut expired: impl FnMut() -> bool,
    mut resolve: impl FnMut(&Path) -> ResolvedWorktreePath,
) -> Option<Vec<ResolvedWorktreePath>> {
    let mut resolved = Vec::new();
    for path in paths {
        if expired() {
            return None;
        }
        resolved.push(resolve(path));
        if expired() {
            return None;
        }
    }
    Some(resolved)
}

fn live_worktree_overlap(
    candidate: &ResolvedWorktreePath,
    live: &ResolvedWorktreePath,
) -> LiveOverlapEvidence {
    let candidate_paths = [&candidate.raw, &candidate.canonical_or_raw];
    let live_paths = [&live.raw, &live.canonical_or_raw];
    let paths_overlap = candidate_paths.iter().any(|candidate_path| {
        live_paths.iter().any(|live_path| {
            live_path.starts_with(candidate_path) || candidate_path.starts_with(live_path)
        })
    });
    let git_identity_matches = candidate
        .git_dir
        .as_ref()
        .zip(live.git_dir.as_ref())
        .is_some_and(|(candidate_git_dir, live_git_dir)| candidate_git_dir == live_git_dir);
    if paths_overlap || git_identity_matches {
        LiveOverlapEvidence::Overlap
    } else if !candidate.resolution_errors.is_empty() || !live.resolution_errors.is_empty() {
        LiveOverlapEvidence::Unknown
    } else {
        LiveOverlapEvidence::Disjoint
    }
}

fn first_live_worktree_overlap(
    candidate: &ResolvedWorktreePath,
    live: &[ResolvedWorktreePath],
) -> Option<(usize, LiveOverlapEvidence)> {
    live.iter().enumerate().find_map(
        |(index, live)| match live_worktree_overlap(candidate, live) {
            LiveOverlapEvidence::Disjoint => None,
            evidence => Some((index, evidence)),
        },
    )
}

fn overlap_resolution_error(
    candidate: &ResolvedWorktreePath,
    live: &ResolvedWorktreePath,
) -> String {
    candidate
        .resolution_errors
        .iter()
        .chain(&live.resolution_errors)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Proof 3: nothing in the checkout is somebody's unrecorded work.
///
/// `Ok(None)` is clean; `Ok(Some(reason))` names what keeps it; `Err` is a
/// listing that failed, which the caller also treats as keep.
pub(crate) fn unclean_reason(
    worktree: &Path,
    branch: Option<&str>,
    main_root: &Path,
    policy: &AutoCleanupPolicy,
) -> Result<Option<String>, String> {
    let status = git(
        worktree,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let changed = status
        .split('\0')
        .filter(|entry| entry.len() > 3)
        .filter(|entry| {
            // Finder metadata is not somebody's change.
            !(entry.starts_with("?? ") && entry[3..].rsplit('/').next() == Some(".DS_Store"))
        })
        .map(|entry| entry[3..].to_string())
        .collect::<Vec<_>>();
    if let Some(first) = changed.first() {
        return Ok(Some(format!(
            "{} uncommitted or untracked path(s), first: {first}",
            changed.len()
        )));
    }

    let git_dir = git(worktree, &["rev-parse", "--absolute-git-dir"])?;
    let git_dir = PathBuf::from(git_dir.trim());
    for (marker, what) in [
        ("MERGE_HEAD", "a merge"),
        ("CHERRY_PICK_HEAD", "a cherry-pick"),
        ("REVERT_HEAD", "a revert"),
        ("BISECT_LOG", "a bisect"),
        ("rebase-merge", "a rebase"),
        ("rebase-apply", "a rebase or am"),
    ] {
        if git_dir.join(marker).exists() {
            return Ok(Some(format!("{what} is in progress")));
        }
    }

    if let Some(branch) = branch {
        let stashes = git(main_root, &["stash", "list", "--format=%gs"])?;
        let made_here = [format!("WIP on {branch}:"), format!("On {branch}:")];
        if let Some(entry) = stashes
            .lines()
            .find(|line| made_here.iter().any(|prefix| line.starts_with(prefix)))
        {
            return Ok(Some(format!(
                "a stash was made on {branch}: {}",
                entry.trim()
            )));
        }
    }

    let ignored = git(
        worktree,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
    )?;
    // Default-deny: every ignored entry must be shown disposable.
    let mut budget = CLASSIFY_ENTRY_CAP;
    for relative in ignored.split('\0').filter(|path| !path.is_empty()) {
        if let Err(reason) = classify_ignored(worktree, relative, policy, &mut budget) {
            return Ok(Some(format!(
                "ignored content outside the regenerable set: {reason}"
            )));
        }
    }
    Ok(None)
}

/// Proof 4: every commit at `head` is in `upstream_tip`.
///
/// `deep` allows the content and patch search; without it only ancestry is
/// tried and anything else is deferred.
pub(crate) fn containment(
    repo: &GitRepo,
    head: &str,
    upstream_tip: &str,
    deep: bool,
    deadline: Option<Instant>,
) -> Result<ContainmentProof, NotRemoved> {
    if repo.is_ancestor(head, upstream_tip) {
        return Ok(ContainmentProof {
            head: head.to_string(),
            containing_commit: upstream_tip.to_string(),
            proof: crate::LandingEvidence::Ancestry.as_str().to_string(),
        });
    }
    if !deep {
        return Err(NotRemoved::Defer);
    }
    match crate::representation::work_landed_within(repo, head, upstream_tip, deadline) {
        Ok(crate::LandingVerdict::Landed {
            evidence,
            landed_by,
        }) => Ok(ContainmentProof {
            head: head.to_string(),
            containing_commit: landed_by.unwrap_or_else(|| upstream_tip.to_string()),
            proof: evidence.as_str().to_string(),
        }),
        Ok(crate::LandingVerdict::NotLanded { truncated, .. }) => {
            Err(NotRemoved::Keep(if truncated {
                "the landing search hit its candidate cap without proving the commits are on the remote default branch".into()
            } else {
                "commits are not contained in the remote default branch".into()
            }))
        }
        Err(BrokerOpError::RepresentationUnavailable { reason })
            if reason.contains("time budget") =>
        {
            Err(NotRemoved::Defer)
        }
        Err(error) => Err(NotRemoved::Keep(format!(
            "containment could not be proved: {error}"
        ))),
    }
}

impl Broker {
    fn auto_cleanup_cursor(
        &mut self,
    ) -> Result<(AutoCleanupCursor, Option<String>), BrokerOpError> {
        let raw = self.store().meta_get(AUTO_CLEANUP_CURSOR_KEY)?;
        Ok((decode_auto_cleanup_cursor(raw.as_deref()), raw))
    }

    fn persist_auto_cleanup_cursor(
        &mut self,
        cursor: &AutoCleanupCursor,
        expected_raw: &mut Option<String>,
    ) -> Result<(), BrokerOpError> {
        // This structure contains only integer fields, so JSON serialization
        // cannot fail. A storage write failure or stale checkpoint must stop
        // planning before any selected checkout can be applied.
        let value =
            serde_json::to_string(cursor).expect("serializing the cleanup cursor is infallible");
        if !self.store().meta_compare_and_set(
            AUTO_CLEANUP_CURSOR_KEY,
            expected_raw.as_deref(),
            &value,
        )? {
            return Err(BrokerOpError::AutoCleanupCursorChanged);
        }
        *expected_raw = Some(value);
        Ok(())
    }

    /// Closed, broker-owned checkouts that still exist, grouped by worktree.
    pub(crate) fn auto_cleanup_candidates(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<(Vec<AutoCleanupCandidate>, Vec<AutoKeptCheckout>)>, BrokerOpError> {
        if deadline_expired(deadline) {
            return Ok(None);
        }
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let live = self.store().live_sessions()?;
        if deadline_expired(deadline) {
            return Ok(None);
        }
        let Some(live_worktrees) = resolve_live_worktree_paths(&live, deadline) else {
            return Ok(None);
        };
        let closed = self.store().cleaned_sessions()?;
        if deadline_expired(deadline) {
            return Ok(None);
        }
        let main_root = canonical(self.main_root());
        if deadline_expired(deadline) {
            return Ok(None);
        }
        let mut by_path: std::collections::BTreeMap<PathBuf, Vec<Session>> =
            std::collections::BTreeMap::new();
        for session in closed {
            if deadline_expired(deadline) {
                return Ok(None);
            }
            if session.cleanup_state == SessionCleanupState::Cleaned {
                continue;
            }
            let path = PathBuf::from(&session.worktree_path);
            if !path.is_dir() {
                if deadline_expired(deadline) {
                    return Ok(None);
                }
                continue;
            }
            let canonical_path = canonical(&path);
            if deadline_expired(deadline) {
                return Ok(None);
            }
            by_path.entry(canonical_path).or_default().push(session);
        }
        let mut candidates = Vec::new();
        let mut kept = Vec::new();
        for (path, sessions) in by_path {
            if deadline_expired(deadline) {
                return Ok(None);
            }
            let ids = sessions
                .iter()
                .map(|session| session.id)
                .collect::<Vec<_>>();
            let keep = |reason: String| AutoKeptCheckout {
                worktree: path.to_string_lossy().into_owned(),
                sessions: ids.clone(),
                reason,
            };
            if path == main_root {
                kept.push(keep("the primary checkout is never removed".into()));
                continue;
            }
            let spawned_sessions = sessions
                .iter()
                .rev()
                .filter(|session| {
                    session.origin == SessionOrigin::Spawned
                        && session.cleanup_state == SessionCleanupState::Closed
                })
                .collect::<Vec<_>>();
            if spawned_sessions.is_empty() {
                kept.push(keep(
                    "no matching spawn-time worktree instance witness".into(),
                ));
                continue;
            }
            // Only a still-retained spawned row with a persisted witness for
            // this exact Git worktree instance proves broker creation.
            // Adopted rows and legacy spawned rows without a witness cannot
            // authorize cleanup, even when the path and repository match.
            let checkout_result = {
                let _git_deadline = deadline.map(crate::git::limit_git_until);
                GitRepo::discover(&path)
            };
            if deadline_expired(deadline) {
                return Ok(None);
            }
            let checkout = match checkout_result {
                Ok(checkout) => checkout,
                Err(_) => {
                    kept.push(keep("not a readable broker-owned worktree".into()));
                    continue;
                }
            };
            let current_witness = {
                let _git_deadline = deadline.map(crate::git::limit_git_until);
                checkout.spawned_worktree_instance_witness()
            };
            if deadline_expired(deadline) {
                return Ok(None);
            }
            let mut matched_owner = None;
            let mut diagnostic_owner = None;
            let mut unreadable_witness = None;
            for session in spawned_sessions {
                if deadline_expired(deadline) {
                    return Ok(None);
                }
                let witness = match self.store().spawned_worktree_instance_witness(session.id) {
                    Ok(Some(witness)) => Some(witness),
                    Ok(None) => None,
                    Err(error) => {
                        unreadable_witness = Some(error.to_string());
                        continue;
                    }
                };
                if deadline_expired(deadline) {
                    return Ok(None);
                }
                if witness
                    .as_ref()
                    .is_some_and(|witness| witness.worktree_path != path.to_string_lossy().as_ref())
                {
                    continue;
                }
                if current_witness.as_ref().is_some_and(|current| {
                    witness.as_ref().is_some_and(|witness| witness == current)
                }) {
                    matched_owner = Some((session.clone(), witness));
                    break;
                }
                // Keep an unproven spawn row only as a diagnostic candidate.
                // The plan can still report live-session, lease, dirty-tree,
                // or gate protections on platforms without a stable identity
                // primitive; auto_cleanup_verdict refuses it before running
                // containment or removing anything.
                if diagnostic_owner.is_none() {
                    diagnostic_owner = Some((session.clone(), witness));
                }
            }
            let owner = matched_owner.or(diagnostic_owner);
            let Some((spawned_owner, spawned_instance_witness)) = owner else {
                let reason = unreadable_witness
                    .map(|error| format!("spawn-time worktree identity is unreadable: {error}"))
                    .unwrap_or_else(|| "no matching spawn-time worktree instance witness".into());
                kept.push(keep(reason));
                continue;
            };
            let is_owned = {
                let _git_deadline = deadline.map(crate::git::limit_git_until);
                self.is_broker_owned_worktree(
                    &spawned_owner,
                    Path::new(&spawned_owner.worktree_path),
                )
            };
            if deadline_expired(deadline) {
                return Ok(None);
            }
            if !is_owned {
                kept.push(keep("not a broker-owned worktree".into()));
                continue;
            }
            // Proof 1: a session still open anywhere in this tree keeps it,
            // whatever state it reports -- an exited or stale session is not
            // a closed one.
            let candidate_worktree = ResolvedWorktreePath::resolve(&path);
            if deadline_expired(deadline) {
                return Ok(None);
            }
            if let Some((index, evidence)) =
                first_live_worktree_overlap(&candidate_worktree, &live_worktrees)
            {
                let open = &live[index];
                let reason = match evidence {
                    LiveOverlapEvidence::Overlap => format!(
                        "session {} is still open ({}) in this worktree",
                        open.id,
                        open.status.as_str()
                    ),
                    LiveOverlapEvidence::Unknown => format!(
                        "session {} worktree overlap could not be resolved: {}",
                        open.id,
                        overlap_resolution_error(&candidate_worktree, &live_worktrees[index])
                    ),
                    LiveOverlapEvidence::Disjoint => {
                        unreachable!("only possible overlaps are returned")
                    }
                };
                kept.push(keep(reason));
                continue;
            }
            candidates.push(AutoCleanupCandidate {
                worktree: path,
                spawned_owner,
                spawned_instance_witness,
                sessions,
            });
            if deadline_expired(deadline) {
                return Ok(None);
            }
        }
        if deadline_expired(deadline) {
            return Ok(None);
        }
        Ok(Some((candidates, kept)))
    }

    /// Proofs 1-4 for one candidate, against the live state right now.
    ///
    /// Called once to select candidates and again, under the same GC lock,
    /// immediately before removal: a file created, a session adopted or a
    /// lease taken in between keeps the checkout.
    ///
    /// `selected` is the proof selection found. Containment depends only on
    /// HEAD and the upstream tip, and the plan pins the tip, so an unchanged
    /// HEAD reuses it instead of repeating the deep landing search with no
    /// deadline; every other proof is re-read (#460).
    pub(crate) fn auto_cleanup_verdict(
        &mut self,
        candidate: &AutoCleanupCandidate,
        policy: &AutoCleanupPolicy,
        upstream: &(String, String),
        deep: bool,
        deadline: Option<Instant>,
        selected: Option<&ContainmentProof>,
    ) -> Result<ContainmentProof, NotRemoved> {
        let keep = NotRemoved::Keep;
        // Only a checkout a broker session record proves it created may go.
        // Candidates come from session records, never from scanning the
        // worktree root, so this is the guard behind that invariant.
        if candidate.sessions.is_empty() {
            return Err(keep("no recorded owner".into()));
        }
        if !candidate
            .sessions
            .iter()
            .any(|session| session.id == candidate.spawned_owner.id)
        {
            return Err(keep("no recorded owner".into()));
        }
        if candidate
            .spawned_instance_witness
            .as_ref()
            .is_some_and(|witness| {
                witness.worktree_path != candidate.worktree.to_string_lossy().as_ref()
            })
        {
            return Err(keep("spawn-time worktree identity path changed".into()));
        }
        if policy.is_pinned(&candidate.worktree) {
            return Err(keep("pinned by [cleanup] keep".into()));
        }
        // Proof 1, re-read: the store may have changed since selection.
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let mut current_spawned_owner = None;
        for session in &candidate.sessions {
            let current = self
                .store()
                .session(session.id)
                .map_err(|error| keep(format!("session {} unreadable: {error}", session.id)))?;
            if !current.status.is_closed() || current.cleanup_state != SessionCleanupState::Closed {
                return Err(keep(format!(
                    "session {} is not closed ({})",
                    session.id,
                    current.status.as_str()
                )));
            }
            if canonical(Path::new(&current.worktree_path)) != candidate.worktree {
                return Err(keep(format!("session {} changed worktree", session.id)));
            }
            if current.id == candidate.spawned_owner.id {
                current_spawned_owner = Some(current);
            }
        }
        let mut expected_session_ids = candidate.session_ids();
        expected_session_ids.sort_unstable();
        let mut current_session_ids = self
            .store()
            .cleaned_sessions()
            .map_err(|error| keep(format!("closed sessions unreadable: {error}")))?
            .into_iter()
            .filter(|session| {
                session.cleanup_state == SessionCleanupState::Closed
                    && canonical(Path::new(&session.worktree_path)) == candidate.worktree
            })
            .map(|session| session.id)
            .collect::<Vec<_>>();
        current_session_ids.sort_unstable();
        if current_session_ids != expected_session_ids {
            return Err(keep("closed session set changed".into()));
        }
        let owner = current_spawned_owner.ok_or_else(|| keep("no recorded owner".into()))?;
        if owner.origin != SessionOrigin::Spawned || owner.branch != candidate.spawned_owner.branch
        {
            return Err(keep("spawned ownership record changed".into()));
        }
        let persisted_witness = self
            .store()
            .spawned_worktree_instance_witness(owner.id)
            .map_err(|error| keep(format!("spawn-time worktree identity unreadable: {error}")))?;
        if persisted_witness.as_ref() != candidate.spawned_instance_witness.as_ref() {
            return Err(keep(
                "spawn-time worktree identity changed or is missing".into(),
            ));
        }
        let checkout = GitRepo::discover(&candidate.worktree)
            .map_err(|error| keep(format!("worktree could not be re-opened: {error}")))?;
        let current_witness = checkout.spawned_worktree_instance_witness();
        match candidate.spawned_instance_witness.as_ref() {
            Some(expected) if current_witness.as_ref() != Some(expected) => {
                return Err(keep(
                    "worktree instance no longer matches its spawn record".into(),
                ));
            }
            None => {}
            Some(_) => {}
        }
        if !self
            .is_broker_owned_worktree_checked(&owner, &candidate.worktree)
            .map_err(|error| keep(format!("worktree ownership could not be checked: {error}")))?
        {
            return Err(keep("not a broker-owned worktree".into()));
        }
        let live = self
            .store()
            .live_sessions()
            .map_err(|error| keep(format!("live sessions unreadable: {error}")))?;
        let Some(live_worktrees) = resolve_live_worktree_paths(&live, deadline) else {
            return Err(NotRemoved::Defer);
        };
        if deadline_expired(deadline) {
            return Err(NotRemoved::Defer);
        }
        let candidate_worktree = ResolvedWorktreePath::resolve(&candidate.worktree);
        if deadline_expired(deadline) {
            return Err(NotRemoved::Defer);
        }
        if let Some((index, evidence)) =
            first_live_worktree_overlap(&candidate_worktree, &live_worktrees)
        {
            let reason = match evidence {
                LiveOverlapEvidence::Overlap => {
                    format!("session {} is open in this worktree", live[index].id)
                }
                LiveOverlapEvidence::Unknown => format!(
                    "session {} worktree overlap could not be resolved: {}",
                    live[index].id,
                    overlap_resolution_error(&candidate_worktree, &live_worktrees[index])
                ),
                LiveOverlapEvidence::Disjoint => {
                    unreachable!("only possible overlaps are returned")
                }
            };
            return Err(keep(reason));
        }

        // Proof 2: leases and gates. Processes are checked by the caller
        // with one system-wide snapshot.
        let now = now_ms();
        let ids = candidate.session_ids();
        let liveness = self
            .lease_liveness(now)
            .map_err(|error| keep(format!("lease liveness unreadable: {error}")))?;
        if let Some(lease) = liveness
            .iter()
            .find(|lease| ids.contains(&lease.session_id) && lease.liveness_evidence.holds)
        {
            return Err(keep(format!(
                "lease on {} is {} and still holds",
                lease.path,
                lease.liveness.as_str()
            )));
        }
        let gates = crate::gates::running_gate_evidence(self.main_root());
        if let Some(gate) = gates.first() {
            return Err(keep(format!("a gate may be running: {gate}")));
        }

        // Proof 3.
        match unclean_reason(
            &candidate.worktree,
            Some(candidate.branch()),
            self.main_root(),
            policy,
        ) {
            Ok(None) => {}
            Ok(Some(reason)) => return Err(keep(reason)),
            Err(error) => return Err(keep(format!("cleanliness could not be checked: {error}"))),
        }

        // Without a spawn-time identity, the prior proofs are useful only for
        // accurate diagnostics. Never allow a legacy or unsupported-platform
        // row to proceed to containment or removal based on current-path
        // observations alone.
        if candidate.spawned_instance_witness.is_none() || current_witness.is_none() {
            return Err(keep(
                "spawn-time worktree instance identity is unavailable".into(),
            ));
        }

        // Proof 4.
        let checkout = GitRepo::discover(&candidate.worktree)
            .map_err(|error| keep(format!("not a readable checkout: {error}")))?;
        let head = checkout
            .head_commit()
            .map_err(|error| keep(format!("HEAD unreadable: {error}")))?;
        if let Some(proof) = selected.filter(|proof| proof.head == head) {
            return Ok(proof.clone());
        }
        containment(self.repo_handle(), &head, &upstream.1, deep, deadline)
    }

    /// Run one auto-cleanup pass within `deadline`: select, then revalidate
    /// and remove. The caller holds the GC lock for the whole pass.
    pub(crate) fn auto_remove_disposable_checkouts(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<AutoCleanupReport, BrokerOpError> {
        let plan = self.auto_cleanup_plan(deadline)?;
        self.auto_cleanup_apply(plan)
    }

    /// Select the checkouts every proof currently allows removing. Removes
    /// nothing; [`Self::auto_cleanup_apply`] re-checks each before acting.
    pub(crate) fn auto_cleanup_plan(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<AutoCleanupPlan, BrokerOpError> {
        let mut report = AutoCleanupReport {
            evaluated_at_ms: now_ms(),
            ..AutoCleanupReport::default()
        };
        let policy = match AutoCleanupPolicy::load(self.main_root()) {
            Ok(policy) => policy,
            Err(error) => {
                report.config_error = Some(error);
                return Ok(AutoCleanupPlan::nothing(report));
            }
        };
        report.enabled = policy.auto_remove;
        if !policy.auto_remove {
            return Ok(AutoCleanupPlan::nothing(report));
        }
        let Some((mut candidates, mut kept)) = self.auto_cleanup_candidates(deadline)? else {
            report.deferred += 1;
            return Ok(AutoCleanupPlan::nothing(report));
        };
        report.kept.append(&mut kept);
        let Some(upstream) = self.remote_default_ref_and_tip() else {
            for candidate in &candidates {
                report.kept.push(AutoKeptCheckout {
                    worktree: candidate.worktree.to_string_lossy().into_owned(),
                    sessions: candidate.session_ids(),
                    reason: "no fetched remote default branch (origin/HEAD) to prove containment against".into(),
                });
            }
            return Ok(AutoCleanupPlan::nothing(report));
        };

        // A persisted high-water mark bounds each lap. The cursor advances
        // only after one candidate's cheap/deep proof has been attempted, so
        // a spent deadline never skips an unvisited candidate. This deadline
        // may already be exhausted by the caller's artifact sweep; the cursor
        // guarantees fairness only among candidates this pass can inspect.
        let (mut cursor, mut expected_cursor) = self.auto_cleanup_cursor()?;
        let lap = auto_cleanup_lap_range(&mut candidates, &mut cursor, candidate_cursor_session_id);
        let deferred_after_lap = candidates.len().saturating_sub(lap.end);
        let candidates = candidates.drain(lap).collect::<Vec<_>>();
        report.deferred += deferred_after_lap;

        let candidate_count = candidates.len();
        let mut selected = Vec::new();
        for (index, candidate) in candidates.into_iter().enumerate() {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                report.deferred += candidate_count - index;
                break;
            }
            let session_id = candidate_cursor_session_id(&candidate);
            let verdict =
                self.auto_cleanup_verdict(&candidate, &policy, &upstream, false, deadline, None);
            let verdict = match verdict {
                Err(NotRemoved::Defer)
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) =>
                {
                    Err(NotRemoved::Defer)
                }
                Err(NotRemoved::Defer) => {
                    self.auto_cleanup_verdict(&candidate, &policy, &upstream, true, deadline, None)
                }
                verdict => verdict,
            };
            match verdict {
                Ok(proof) => selected.push((candidate, proof)),
                Err(NotRemoved::Defer) => report.deferred += 1,
                Err(NotRemoved::Keep(reason)) => report.kept.push(AutoKeptCheckout {
                    worktree: candidate.worktree.to_string_lossy().into_owned(),
                    sessions: candidate.session_ids(),
                    reason,
                }),
            }
            cursor.after_session_id = session_id;
            self.persist_auto_cleanup_cursor(&cursor, &mut expected_cursor)?;
        }
        Ok(AutoCleanupPlan {
            report,
            policy,
            upstream: Some(upstream),
            selected,
            deadline,
        })
    }

    /// Revalidate every proof for each selected checkout, under the lock and
    /// immediately before removal, then remove the ones that still pass.
    pub(crate) fn auto_cleanup_apply(
        &mut self,
        plan: AutoCleanupPlan,
    ) -> Result<AutoCleanupReport, BrokerOpError> {
        let AutoCleanupPlan {
            mut report,
            policy,
            upstream,
            selected,
            deadline,
        } = plan;
        let Some(upstream) = upstream.filter(|_| !selected.is_empty()) else {
            self.record_auto_cleanup_report(&report)?;
            return Ok(report);
        };
        // One process snapshot for the whole batch, taken after selection so
        // it is as close to removal as it can be. No snapshot keeps all.
        let open_paths = crate::reclaim::open_paths_under(Path::new("/"));
        let records = crate::measurement::load_size_records(self.main_root());
        let mut attempted_any = false;
        for (candidate, selected_proof) in selected {
            // The first selected checkout always goes, so a pass whose
            // selection spent the budget still converges; the rest wait for
            // a later pass rather than stretching a broker open (#460).
            if attempted_any && deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                report.deferred += 1;
                continue;
            }
            attempted_any = true;
            let worktree_text = candidate.worktree.to_string_lossy().into_owned();
            let keep_it = |report: &mut AutoCleanupReport, reason: String| {
                report.kept.push(AutoKeptCheckout {
                    worktree: worktree_text.clone(),
                    sessions: candidate.session_ids(),
                    reason,
                });
            };
            let _lifecycle_claim = match self.claim_worktree_lifecycle() {
                Ok(claim) => claim,
                Err(error) => {
                    keep_it(
                        &mut report,
                        format!("could not claim the worktree lifecycle: {error}"),
                    );
                    continue;
                }
            };
            let Some(open_paths) = open_paths.as_ref() else {
                keep_it(
                    &mut report,
                    "could not list open files (lsof unavailable), so process use is unknown"
                        .into(),
                );
                continue;
            };
            if let Some(open) = open_paths
                .iter()
                .find(|open| open.starts_with(&candidate.worktree))
            {
                keep_it(
                    &mut report,
                    format!("a process has {} open", open.display()),
                );
                continue;
            }
            // Revalidate every proof now, under the lock, right before removal.
            let proof = match self.auto_cleanup_verdict(
                &candidate,
                &policy,
                &upstream,
                true,
                None,
                Some(&selected_proof),
            ) {
                Ok(proof) => proof,
                Err(NotRemoved::Keep(reason)) => {
                    keep_it(&mut report, format!("changed before removal: {reason}"));
                    continue;
                }
                Err(NotRemoved::Defer) => {
                    report.deferred += 1;
                    continue;
                }
            };
            match self.remove_auto_cleanup_candidate(&candidate, &proof) {
                Ok(()) => {
                    let removed = AutoRemovedCheckout {
                        worktree: worktree_text.clone(),
                        sessions: candidate.session_ids(),
                        branch: Some(candidate.branch().to_string()),
                        head: proof.head.clone(),
                        contained_in: upstream.0.clone(),
                        containing_commit: proof.containing_commit.clone(),
                        proof: proof.proof.clone(),
                        bytes: records
                            .get(&worktree_text)
                            .map(|record| record.bytes)
                            .or_else(|| {
                                candidate.sessions.iter().find_map(|session| {
                                    records.get(&session.worktree_path).map(|r| r.bytes)
                                })
                            }),
                    };
                    let payload = serde_json::to_string(&removed).unwrap_or_default();
                    self.store().append_event(
                        crate::events::BROKER_CLEANUP_AUTO_REMOVED,
                        candidate.sessions.last().map(|session| session.id),
                        Some(&payload),
                    )?;
                    report.removed.push(removed);
                }
                Err(error) => keep_it(&mut report, format!("removal failed: {error}")),
            }
        }
        self.record_auto_cleanup_report(&report)?;
        Ok(report)
    }

    fn remove_auto_cleanup_candidate(
        &mut self,
        candidate: &AutoCleanupCandidate,
        proof: &ContainmentProof,
    ) -> Result<(), BrokerOpError> {
        let first = candidate
            .sessions
            .first()
            .map(|session| session.id)
            .unwrap_or_default();
        self.refuse_live_checkout(first, &candidate.worktree)?;
        // Forced: the proofs above are stricter than Git's own check, and the
        // only untracked file they let through is Finder's `.DS_Store`, which
        // a plain `worktree remove` refuses.
        self.repo_handle()
            .worktree_remove_registered(&candidate.worktree, true)?;
        ensure_auto_cleanup_path_absent(&candidate.worktree)?;
        let branch = candidate.branch();
        if self
            .repo_handle()
            .resolve_ref(&format!("refs/heads/{branch}"))
            .as_deref()
            == Some(proof.head.as_str())
        {
            // Only the exact proved head is deleted; a branch that moved is
            // somebody's and stays. The checked delete refuses a race.
            self.repo_handle()
                .delete_branch_ref_checked(branch, &proof.head)?;
        }
        for session in &candidate.sessions {
            self.store()
                .set_session_status(session.id, crate::SessionStatus::Cleaned, None)?;
        }
        Ok(())
    }

    fn record_auto_cleanup_report(
        &mut self,
        report: &AutoCleanupReport,
    ) -> Result<(), BrokerOpError> {
        let text = serde_json::to_string(report).unwrap_or_default();
        self.store().meta_set(AUTO_CLEANUP_LAST_REPORT_KEY, &text)?;
        Ok(())
    }

    /// The last auto-cleanup report, for `status` and `gc plan`.
    pub fn last_auto_cleanup_report(&mut self) -> Option<AutoCleanupReport> {
        self.store()
            .meta_get(AUTO_CLEANUP_LAST_REPORT_KEY)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(&text).ok())
    }

    /// Select under the GC lock, without removing anything.
    pub fn auto_cleanup_plan_now(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<AutoCleanupPlan>, BrokerOpError> {
        let Ok(_lock) = crate::gc::GcLock::acquire(self.main_root()) else {
            return Ok(None);
        };
        self.auto_cleanup_plan(deadline).map(Some)
    }

    /// Revalidate and remove under the GC lock.
    pub fn auto_cleanup_apply_now(
        &mut self,
        plan: AutoCleanupPlan,
    ) -> Result<Option<AutoCleanupReport>, BrokerOpError> {
        let Ok(_lock) = crate::gc::GcLock::acquire(self.main_root()) else {
            return Ok(None);
        };
        self.auto_cleanup_apply(plan).map(Some)
    }

    /// Run an auto-cleanup pass now, outside the sweep's cadence. Takes the
    /// GC lock; returns `None` when another GC holds it.
    pub fn auto_cleanup_now(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<AutoCleanupReport>, BrokerOpError> {
        let Ok(_lock) = crate::gc::GcLock::acquire(self.main_root()) else {
            return Ok(None);
        };
        self.auto_remove_disposable_checkouts(deadline).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AUTO_CLEANUP_CURSOR_KEY, AutoCleanupCursor, AutoCleanupPolicy, LiveOverlapEvidence,
        ResolvedWorktreePath, auto_cleanup_lap_range, classify_ignored, decode_auto_cleanup_cursor,
        first_live_worktree_overlap, resolve_worktree_paths_until,
    };
    use std::path::Path;
    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::process::Command;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    fn classify(root: &Path, relative: &str, policy: &AutoCleanupPolicy) -> Result<(), String> {
        let mut budget = super::CLASSIFY_ENTRY_CAP;
        classify_ignored(root, relative, policy, &mut budget)
    }

    fn touch(root: &Path, relative: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
    }

    #[test]
    fn partial_live_path_index_is_discarded_when_deadline_expires_mid_scan() {
        let temporary = tempfile::tempdir().unwrap();
        let first_path = temporary.path().join("first");
        let second_path = temporary.path().join("second");
        std::fs::create_dir_all(&first_path).unwrap();
        std::fs::create_dir_all(&second_path).unwrap();
        let paths = [first_path.as_path(), second_path.as_path()];
        let resolved_count = std::cell::Cell::new(0);

        let resolved = resolve_worktree_paths_until(
            paths,
            || resolved_count.get() == 1,
            |path| {
                resolved_count.set(resolved_count.get() + 1);
                ResolvedWorktreePath::resolve(path)
            },
        );

        assert!(resolved.is_none(), "a partial index cannot be used");
        assert_eq!(resolved_count.get(), 1, "expiry stops before the next path");
    }

    #[test]
    fn live_overlap_index_preserves_component_and_missing_path_fallback() {
        let temporary = tempfile::tempdir().unwrap();
        let candidate_path = temporary.path().join("checkout");
        std::fs::create_dir_all(&candidate_path).unwrap();
        let candidate = ResolvedWorktreePath::resolve(&candidate_path);
        let missing_child = ResolvedWorktreePath::resolve(&candidate_path.join("not-created"));
        let sibling = ResolvedWorktreePath::resolve(&temporary.path().join("checkout-old"));

        assert_eq!(
            first_live_worktree_overlap(&candidate, &[missing_child]),
            Some((0, LiveOverlapEvidence::Overlap)),
            "a genuinely missing live path keeps the existing raw-path fallback"
        );
        assert_eq!(
            first_live_worktree_overlap(&candidate, &[sibling]),
            None,
            "path-prefix checks compare components rather than string prefixes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn live_overlap_index_preserves_canonical_alias_and_git_directory_identity() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let real = temporary.path().join("real");
        std::fs::create_dir_all(real.join("child")).unwrap();
        let alias = temporary.path().join("alias");
        symlink(&real, &alias).unwrap();
        let candidate = ResolvedWorktreePath::resolve(&real);
        let alias_child = ResolvedWorktreePath::resolve(&alias.join("child"));
        assert_eq!(
            first_live_worktree_overlap(&candidate, &[alias_child]),
            Some((0, LiveOverlapEvidence::Overlap)),
            "canonical aliases of nested live paths remain protected"
        );

        let git_dir = temporary.path().join("shared-git-dir");
        std::fs::create_dir_all(&git_dir).unwrap();
        let candidate_path = temporary.path().join("checkout-one");
        let live_path = temporary.path().join("checkout-two");
        for worktree in [&candidate_path, &live_path] {
            std::fs::create_dir_all(worktree).unwrap();
            std::fs::write(
                worktree.join(".git"),
                format!("gitdir: {}\n", git_dir.display()),
            )
            .unwrap();
        }
        let candidate = ResolvedWorktreePath::resolve(&candidate_path);
        let live = ResolvedWorktreePath::resolve(&live_path);
        assert_eq!(
            first_live_worktree_overlap(&candidate, &[live]),
            Some((0, LiveOverlapEvidence::Overlap)),
            "different recorded paths sharing a Git directory remain protected"
        );
    }

    #[test]
    fn unknown_live_path_identity_blocks_instead_of_authorizing_cleanup() {
        let temporary = tempfile::tempdir().unwrap();
        let candidate_path = temporary.path().join("candidate");
        let live_path = temporary.path().join("live");
        std::fs::create_dir_all(candidate_path.join(".git")).unwrap();
        std::fs::create_dir_all(&live_path).unwrap();
        std::fs::write(live_path.join(".git"), "not a gitdir pointer\n").unwrap();

        let candidate = ResolvedWorktreePath::resolve(&candidate_path);
        let unknown_live = ResolvedWorktreePath::resolve(&live_path);
        assert_eq!(
            first_live_worktree_overlap(&candidate, &[unknown_live]),
            Some((0, LiveOverlapEvidence::Unknown)),
            "unreadable or malformed identity is not treated as a complete disjoint index"
        );
    }

    #[test]
    fn live_overlap_index_keeps_the_first_matching_session_order() {
        let temporary = tempfile::tempdir().unwrap();
        let candidate_path = temporary.path().join("checkout");
        std::fs::create_dir_all(candidate_path.join("one")).unwrap();
        std::fs::create_dir_all(candidate_path.join("two")).unwrap();
        let candidate = ResolvedWorktreePath::resolve(&candidate_path);
        let first = ResolvedWorktreePath::resolve(&candidate_path.join("one"));
        let second = ResolvedWorktreePath::resolve(&candidate_path.join("two"));

        assert_eq!(
            first_live_worktree_overlap(&candidate, &[first, second]),
            Some((0, LiveOverlapEvidence::Overlap)),
            "the index returns the first overlapping live row in store order"
        );
    }

    #[cfg(unix)]
    fn git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    fn closed_dirty_checkout(broker: &mut crate::Broker, label: &str) -> (i64, PathBuf) {
        let session = broker.start_worktree(label, None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        broker
            .close_abandoning_unpushed(session.id, "cursor test backlog owner")
            .unwrap();
        std::fs::write(worktree.join("cursor-dirty.txt"), "retain this checkout\n").unwrap();
        (session.id, worktree)
    }

    #[cfg(unix)]
    #[test]
    fn planner_resumes_wraps_and_rechecks_changed_checkout_eligibility() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Auto cleanup test"]);
        git(
            &root,
            &["config", "user.email", "auto-cleanup@example.invalid"],
        );
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", ".gitignore", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        git(&root, &["remote", "add", "origin", "."]);
        git(
            &root,
            &["fetch", "-q", "origin", "main:refs/remotes/origin/main"],
        );
        git(&root, &["remote", "set-head", "origin", "main"]);

        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nauto_remove = true\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 1\nclosed_worktree_grace_hours = 0\nauto_cleanup_worktrees_on_finish = false\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let (first_id, first_path) = closed_dirty_checkout(&mut broker, "cursor first candidate");
        let (second_id, second_path) =
            closed_dirty_checkout(&mut broker, "cursor second candidate");
        assert!(first_id < second_id);
        assert!(first_path.is_dir());
        assert!(second_path.is_dir());
        for session_id in [first_id, second_id] {
            assert!(
                broker
                    .store()
                    .spawned_worktree_instance_witness(session_id)
                    .unwrap()
                    .is_some(),
                "Unix fixture sessions must carry valid spawn-time worktree witnesses"
            );
        }

        let seed = AutoCleanupCursor {
            version: super::AUTO_CLEANUP_CURSOR_VERSION,
            after_session_id: first_id,
            high_water_session_id: second_id,
        };
        let seed_json = serde_json::to_string(&seed).unwrap();
        broker
            .store()
            .meta_set(AUTO_CLEANUP_CURSOR_KEY, &seed_json)
            .unwrap();

        let expired_deadline = Instant::now() - Duration::from_millis(1);
        let expired_pass = broker.auto_cleanup_plan(Some(expired_deadline)).unwrap();
        assert!(expired_pass.selected_worktrees().is_empty());
        assert!(expired_pass.report.kept.is_empty());
        assert_eq!(expired_pass.report.deferred, 1);
        assert_eq!(
            broker
                .store()
                .meta_get(AUTO_CLEANUP_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some(seed_json.as_str()),
            "an incomplete discovery pass does not advance the cursor"
        );

        let first_pass = broker.auto_cleanup_plan(None).unwrap();
        assert_eq!(
            first_pass
                .report
                .kept
                .iter()
                .map(|kept| kept.sessions.clone())
                .collect::<Vec<_>>(),
            vec![vec![second_id]],
            "the persisted cursor resumes after the first candidate"
        );

        let wrapped_pass = broker.auto_cleanup_plan(None).unwrap();
        let mut wrapped_ids = wrapped_pass
            .report
            .kept
            .iter()
            .flat_map(|kept| kept.sessions.iter().copied())
            .collect::<Vec<_>>();
        wrapped_ids.sort_unstable();
        assert_eq!(
            wrapped_ids,
            vec![first_id, second_id],
            "finishing a lap wraps around and revisits earlier candidates"
        );

        std::fs::remove_file(first_path.join("cursor-dirty.txt")).unwrap();
        let changed_eligibility_pass = broker.auto_cleanup_plan(None).unwrap();
        let selected = changed_eligibility_pass.selected_worktrees();
        assert!(
            selected.contains(&first_path.to_string_lossy().into_owned()),
            "a candidate that becomes clean is checked again on a later lap"
        );
        assert!(
            !selected.contains(&second_path.to_string_lossy().into_owned()),
            "the still-dirty candidate remains protected"
        );
    }

    #[test]
    fn cleanup_cursor_bounds_laps_and_revisits_new_arrivals() {
        let mut cursor = AutoCleanupCursor::default();
        let mut first_lap = vec![1_i64, 2, 3];
        let range = auto_cleanup_lap_range(&mut first_lap, &mut cursor, |id| *id);
        assert_eq!(&first_lap[range], &[1, 2, 3]);
        assert_eq!(cursor.high_water_session_id, 3);

        cursor.after_session_id = 2;
        let mut with_arrivals = vec![1_i64, 2, 3, 4, 5];
        let range = auto_cleanup_lap_range(&mut with_arrivals, &mut cursor, |id| *id);
        assert_eq!(&with_arrivals[range], &[3]);
        assert_eq!(cursor.high_water_session_id, 3);

        // Once the lap reaches its captured high-water mark, later IDs join
        // the next lap instead of extending this one.
        cursor.after_session_id = 3;
        let mut next_lap = vec![1_i64, 2, 3, 4, 5];
        let range = auto_cleanup_lap_range(&mut next_lap, &mut cursor, |id| *id);
        assert_eq!(&next_lap[range], &[1, 2, 3, 4, 5]);
        assert_eq!(cursor.after_session_id, 0);
        assert_eq!(cursor.high_water_session_id, 5);
    }

    #[test]
    fn unknown_cleanup_cursor_version_resets_scheduling_state() {
        let cursor = decode_auto_cleanup_cursor(Some(
            r#"{"version":9,"after_session_id":7,"high_water_session_id":12}"#,
        ));
        assert_eq!(cursor, AutoCleanupCursor::default());
    }

    #[test]
    fn whole_checkout_names_cover_the_unattended_artifact_catalog() {
        for kind in crate::artifact_catalog::CATALOG {
            if kind.unattended {
                assert!(
                    super::BUILT_IN_REGENERABLE.contains(&kind.name),
                    "{}",
                    kind.name
                );
            }
        }
    }

    #[test]
    fn pnpm_store_requires_a_witness_and_keeps_sensitive_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        std::fs::create_dir(root.join(".pnpm-store")).unwrap();
        assert!(classify(root, ".pnpm-store/", &policy).is_err());
        touch(root, ".pnpm-store/v10/files/00/content");
        assert_eq!(classify(root, ".pnpm-store/", &policy), Ok(()));
        touch(root, ".pnpm-store/.env");
        assert!(classify(root, ".pnpm-store/", &policy).is_err());
        std::fs::remove_file(root.join(".pnpm-store/.env")).unwrap();
        touch(root, ".pnpm-store/.aethyme/state.json");
        assert!(classify(root, ".pnpm-store/", &policy).is_err());
    }

    #[test]
    fn build_output_counts_only_with_the_evidence_that_proves_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        touch(root, "node_modules/pkg/index.js");
        touch(root, "Cargo.toml");
        touch(root, "target/debug/app");
        touch(root, "py/__pycache__/m.pyc");
        touch(root, ".DS_Store");
        for ok in ["node_modules/", "target/", "py/__pycache__/", ".DS_Store"] {
            assert_eq!(classify(root, ok, &policy), Ok(()), "{ok}");
        }
        // `build/` with no manifest beside it is somebody's directory.
        touch(root, "assets/build/logo.svg");
        assert!(classify(root, "assets/build/", &policy).is_err());
        touch(root, "web/package.json");
        touch(root, "web/build/app.js");
        assert_eq!(classify(root, "web/build/", &policy), Ok(()));
        // A Cargo-less `target/` is not a Cargo target directory.
        touch(root, "data/target/run.csv");
        assert!(classify(root, "data/target/", &policy).is_err());
        // Whole components only: no prefix or substring matches.
        touch(root, "target-notes/a.md");
        touch(root, "my_build/x");
        touch(root, "targets.json");
        for blocked in ["target-notes/", "my_build/", "targets.json"] {
            assert!(classify(root, blocked, &policy).is_err(), "{blocked}");
        }
    }

    #[test]
    fn secrets_state_and_escaping_symlinks_block_even_inside_build_output() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        touch(root, ".env");
        assert!(classify(root, ".env", &policy).is_err());
        touch(root, "node_modules/pkg/server.pem");
        assert!(classify(root, "node_modules/", &policy).is_err());
        std::fs::remove_file(root.join("node_modules/pkg/server.pem")).unwrap();
        touch(root, "node_modules/.aethyme/state.json");
        assert!(classify(root, "node_modules/", &policy).is_err());
        std::fs::remove_dir_all(root.join("node_modules/.aethyme")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.join("node_modules/escape")).unwrap();
            let reason = classify(root, "node_modules/", &policy).unwrap_err();
            assert!(reason.contains("outside the worktree"), "{reason}");
            std::fs::remove_file(root.join("node_modules/escape")).unwrap();
            std::os::unix::fs::symlink(
                root.join("node_modules/pkg"),
                root.join("node_modules/inside"),
            )
            .unwrap();
        }
        assert_eq!(classify(root, "node_modules/", &policy), Ok(()));
    }

    #[test]
    fn configured_globs_add_directories_and_never_secrets_or_root_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".aethyme")).unwrap();
        let write =
            |body: &str| std::fs::write(dir.path().join(".aethyme/config.toml"), body).unwrap();
        assert_eq!(
            AutoCleanupPolicy::load(dir.path()).unwrap(),
            AutoCleanupPolicy::default()
        );
        write(
            "[cleanup]\nauto_remove = false\nkeep = [\"*-pinned\"]\nregenerable = [\".gradle\", \"**/.gradle\"]\n",
        );
        let policy = AutoCleanupPolicy::load(dir.path()).unwrap();
        assert!(!policy.auto_remove);
        assert!(policy.is_pinned(Path::new("/w/repo/fix-pinned")));
        touch(dir.path(), ".gradle/caches/x.bin");
        assert_eq!(classify(dir.path(), ".gradle/", &policy), Ok(()));
        for rejected in [
            "\".env*\"",
            "\"*\"",
            "\"**\"",
            "\"*.db\"",
            "\"*.txt\"",
            "\".aethyme\"",
        ] {
            write(&format!("[cleanup]\nregenerable = [{rejected}]\n"));
            assert!(AutoCleanupPolicy::load(dir.path()).is_err(), "{rejected}");
        }
        write("[cleanup]\nauto_remove = \"yes\"\n");
        assert!(AutoCleanupPolicy::load(dir.path()).is_err());
        write("[cleanup]\nautoremove = true\n");
        assert!(AutoCleanupPolicy::load(dir.path()).is_err());
    }
}

fn ensure_auto_cleanup_path_absent(path: &Path) -> Result<(), BrokerOpError> {
    let args = format!("remove {}", path.display());
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(BrokerOpError::Git(crate::GitError::Git {
            args,
            stderr: format!(
                "Git reported worktree removal but {} still exists; retained it because its identity is unproven",
                path.display()
            ),
        })),
        Err(error) => Err(BrokerOpError::Git(crate::GitError::Git {
            args,
            stderr: format!(
                "could not verify that {} is absent after Git worktree removal; retained it: {error}",
                path.display()
            ),
        })),
    }
}

#[cfg(test)]
mod fail_closed_removal_tests {
    use super::ensure_auto_cleanup_path_absent;
    #[cfg(unix)]
    use super::{AutoCleanupCandidate, ContainmentProof};
    #[cfg(unix)]
    use crate::{SessionCleanupState, git::with_git_deadline};
    #[cfg(unix)]
    use std::path::Path;
    #[cfg(unix)]
    use std::process::Command;
    #[cfg(unix)]
    use std::time::Duration;

    #[cfg(unix)]
    fn git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn auto_cleanup_preserves_candidate_after_git_removal_timeout() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        git(&repository, &["config", "user.name", "Auto cleanup test"]);
        git(
            &repository,
            &["config", "user.email", "auto-cleanup@example.invalid"],
        );
        git(&repository, &["commit", "--allow-empty", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("auto cleanup candidate", None)
            .unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        broker.close(session.id).unwrap();
        let evidence = worktree.join("keep-for-retry.txt");
        std::fs::write(&evidence, b"preserve this candidate").unwrap();
        let spawned_owner = broker.store().session(session.id).unwrap();
        let spawned_instance_witness = broker
            .store()
            .spawned_worktree_instance_witness(session.id)
            .unwrap()
            .expect("the production spawn path records its instance witness");
        let candidate = AutoCleanupCandidate {
            worktree: worktree.clone(),
            spawned_owner: spawned_owner.clone(),
            spawned_instance_witness: Some(spawned_instance_witness),
            sessions: vec![spawned_owner],
        };
        let proof = ContainmentProof {
            head: "fixture-head".into(),
            containing_commit: "fixture-containing-commit".into(),
            proof: "ancestry".into(),
        };

        let result = with_git_deadline(Duration::ZERO, || {
            broker.remove_auto_cleanup_candidate(&candidate, &proof)
        });
        assert!(result.is_err(), "expired Git budget must refuse removal");
        assert_eq!(
            std::fs::read(&evidence).unwrap(),
            b"preserve this candidate"
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed,
            "failed removal must leave the candidate available for retry"
        );
    }

    #[test]
    fn auto_cleanup_preserves_a_path_git_left_after_removal() {
        let owner = tempfile::tempdir().unwrap();
        let replacement = owner.path().join("replacement-worktree");
        std::fs::create_dir(&replacement).unwrap();
        let evidence = replacement.join("keep.txt");
        std::fs::write(&evidence, b"preserve replacement").unwrap();

        let result = ensure_auto_cleanup_path_absent(&replacement);

        assert!(result.is_err(), "a remaining path is not proof of removal");
        assert_eq!(std::fs::read(evidence).unwrap(), b"preserve replacement");
    }
}
