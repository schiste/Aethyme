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
/// | `target` | a sibling `Cargo.toml`, or `CACHEDIR.TAG` inside |
/// | `.venv` | `pyvenv.cfg` inside |
/// | `build`, `dist` | a sibling `package.json`, `setup.py`, `pyproject.toml` or `CMakeLists.txt` |
/// | `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache` | always (tool caches) |
///
/// Names are compared as whole path components: `target-notes/`,
/// `my_build/` and `targets.json` are not build output.
pub const BUILT_IN_REGENERABLE: &[&str] = &[
    "node_modules",
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
        _ => None,
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
    /// Candidates whose deep proof the budget left for a later pass.
    pub deferred: usize,
}

/// A closed checkout and every session that names it.
#[derive(Debug, Clone)]
pub(crate) struct AutoCleanupCandidate {
    pub worktree: PathBuf,
    pub sessions: Vec<Session>,
}

impl AutoCleanupCandidate {
    fn session_ids(&self) -> Vec<i64> {
        self.sessions.iter().map(|session| session.id).collect()
    }

    fn branch(&self) -> Option<&str> {
        self.sessions
            .iter()
            .rev()
            .find(|session| session.origin == SessionOrigin::Spawned)
            .map(|session| session.branch.as_str())
    }
}

/// Checkouts selected for removal, not yet removed.
#[derive(Debug, Clone)]
pub struct AutoCleanupPlan {
    report: AutoCleanupReport,
    policy: AutoCleanupPolicy,
    upstream: Option<(String, String)>,
    selected: Vec<(AutoCleanupCandidate, ContainmentProof)>,
}

impl AutoCleanupPlan {
    fn nothing(report: AutoCleanupReport) -> Self {
        Self {
            report,
            policy: AutoCleanupPolicy::default(),
            upstream: None,
            selected: Vec::new(),
        }
    }

    /// The worktrees this plan would remove if nothing changes.
    pub fn selected_worktrees(&self) -> Vec<String> {
        self.selected
            .iter()
            .map(|(candidate, _)| candidate.worktree.to_string_lossy().into_owned())
            .collect()
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
    /// Closed, broker-owned checkouts that still exist, grouped by worktree.
    pub(crate) fn auto_cleanup_candidates(
        &mut self,
    ) -> Result<(Vec<AutoCleanupCandidate>, Vec<AutoKeptCheckout>), BrokerOpError> {
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let live = self.store().live_sessions()?;
        let closed = self.store().cleaned_sessions()?;
        let main_root = canonical(self.main_root());
        let mut by_path: std::collections::BTreeMap<PathBuf, Vec<Session>> =
            std::collections::BTreeMap::new();
        for session in closed {
            if session.cleanup_state == SessionCleanupState::Cleaned {
                continue;
            }
            let path = PathBuf::from(&session.worktree_path);
            if !path.is_dir() {
                continue;
            }
            by_path.entry(canonical(&path)).or_default().push(session);
        }
        let mut candidates = Vec::new();
        let mut kept = Vec::new();
        for (path, sessions) in by_path {
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
            let owned = sessions.iter().all(|session| {
                self.is_broker_owned_worktree(session, Path::new(&session.worktree_path))
            });
            if !owned {
                kept.push(keep("not a broker-owned worktree".into()));
                continue;
            }
            // Proof 1: a session still open anywhere in this tree keeps it,
            // whatever state it reports -- an exited or stale session is not
            // a closed one.
            if let Some(open) = live.iter().find(|session| {
                canonical(Path::new(&session.worktree_path)).starts_with(&path)
                    || path.starts_with(canonical(Path::new(&session.worktree_path)))
            }) {
                kept.push(keep(format!(
                    "session {} is still open ({}) in this worktree",
                    open.id,
                    open.status.as_str()
                )));
                continue;
            }
            candidates.push(AutoCleanupCandidate {
                worktree: path,
                sessions,
            });
        }
        Ok((candidates, kept))
    }

    /// Proofs 1-4 for one candidate, against the live state right now.
    ///
    /// Called once to select candidates and again, under the same GC lock,
    /// immediately before removal: a file created, a session adopted or a
    /// lease taken in between keeps the checkout.
    pub(crate) fn auto_cleanup_verdict(
        &mut self,
        candidate: &AutoCleanupCandidate,
        policy: &AutoCleanupPolicy,
        upstream: &(String, String),
        deep: bool,
        deadline: Option<Instant>,
    ) -> Result<ContainmentProof, NotRemoved> {
        let keep = NotRemoved::Keep;
        if policy.is_pinned(&candidate.worktree) {
            return Err(keep("pinned by [cleanup] keep".into()));
        }
        // Proof 1, re-read: the store may have changed since selection.
        for session in &candidate.sessions {
            let current = self
                .store()
                .session(session.id)
                .map_err(|error| keep(format!("session {} unreadable: {error}", session.id)))?;
            if !current.status.is_closed() || current.cleanup_state == SessionCleanupState::Open {
                return Err(keep(format!(
                    "session {} is not closed ({})",
                    session.id,
                    current.status.as_str()
                )));
            }
        }
        if let Some(live) = self
            .store()
            .live_sessions()
            .map_err(|error| keep(format!("live sessions unreadable: {error}")))?
            .iter()
            .find(|live| {
                let live_path = std::fs::canonicalize(&live.worktree_path)
                    .unwrap_or_else(|_| PathBuf::from(&live.worktree_path));
                live_path.starts_with(&candidate.worktree)
            })
        {
            return Err(keep(format!(
                "session {} is open in this worktree",
                live.id
            )));
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
        let branch = candidate.branch();
        match unclean_reason(&candidate.worktree, branch, self.main_root(), policy) {
            Ok(None) => {}
            Ok(Some(reason)) => return Err(keep(reason)),
            Err(error) => return Err(keep(format!("cleanliness could not be checked: {error}"))),
        }

        // Proof 4.
        let checkout = GitRepo::discover(&candidate.worktree)
            .map_err(|error| keep(format!("not a readable checkout: {error}")))?;
        let head = checkout
            .head_commit()
            .map_err(|error| keep(format!("HEAD unreadable: {error}")))?;
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
        let (candidates, mut kept) = self.auto_cleanup_candidates()?;
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

        // Cheap proofs for everyone first; the deep proof only with time left.
        let mut selected = Vec::new();
        let mut deep_needed = Vec::new();
        for candidate in candidates {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                report.deferred += 1;
                continue;
            }
            match self.auto_cleanup_verdict(&candidate, &policy, &upstream, false, deadline) {
                Ok(proof) => selected.push((candidate, proof)),
                Err(NotRemoved::Defer) => deep_needed.push(candidate),
                Err(NotRemoved::Keep(reason)) => report.kept.push(AutoKeptCheckout {
                    worktree: candidate.worktree.to_string_lossy().into_owned(),
                    sessions: candidate.session_ids(),
                    reason,
                }),
            }
        }
        for candidate in deep_needed {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                report.deferred += 1;
                continue;
            }
            match self.auto_cleanup_verdict(&candidate, &policy, &upstream, true, deadline) {
                Ok(proof) => selected.push((candidate, proof)),
                Err(NotRemoved::Defer) => report.deferred += 1,
                Err(NotRemoved::Keep(reason)) => report.kept.push(AutoKeptCheckout {
                    worktree: candidate.worktree.to_string_lossy().into_owned(),
                    sessions: candidate.session_ids(),
                    reason,
                }),
            }
        }
        Ok(AutoCleanupPlan {
            report,
            policy,
            upstream: Some(upstream),
            selected,
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
        } = plan;
        let Some(upstream) = upstream.filter(|_| !selected.is_empty()) else {
            self.record_auto_cleanup_report(&report)?;
            return Ok(report);
        };
        // One process snapshot for the whole batch, taken after selection so
        // it is as close to removal as it can be. No snapshot keeps all.
        let open_paths = crate::reclaim::open_paths_under(Path::new("/"));
        let records = crate::measurement::load_size_records(self.main_root());
        for (candidate, _) in selected {
            let worktree_text = candidate.worktree.to_string_lossy().into_owned();
            let keep_it = |report: &mut AutoCleanupReport, reason: String| {
                report.kept.push(AutoKeptCheckout {
                    worktree: worktree_text.clone(),
                    sessions: candidate.session_ids(),
                    reason,
                });
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
            let proof = match self.auto_cleanup_verdict(&candidate, &policy, &upstream, true, None)
            {
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
                        branch: candidate.branch().map(str::to_string),
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
            .worktree_remove(&candidate.worktree, true)?;
        if candidate.worktree.exists() {
            let outcome = crate::removal::remove_tree(&candidate.worktree);
            if !outcome.removed {
                return Err(BrokerOpError::Git(crate::GitError::Git {
                    args: format!("remove {}", candidate.worktree.display()),
                    stderr: outcome
                        .error
                        .unwrap_or_else(|| "the directory is still present".into()),
                }));
            }
        }
        if let Some(branch) = candidate.branch()
            && self
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
        let Ok(_lock) = crate::gc::GcLock::acquire(&self.main_root().to_path_buf()) else {
            return Ok(None);
        };
        self.auto_cleanup_plan(deadline).map(Some)
    }

    /// Revalidate and remove under the GC lock.
    pub fn auto_cleanup_apply_now(
        &mut self,
        plan: AutoCleanupPlan,
    ) -> Result<Option<AutoCleanupReport>, BrokerOpError> {
        let Ok(_lock) = crate::gc::GcLock::acquire(&self.main_root().to_path_buf()) else {
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
        let Ok(_lock) = crate::gc::GcLock::acquire(&self.main_root().to_path_buf()) else {
            return Ok(None);
        };
        self.auto_remove_disposable_checkouts(deadline).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::{AutoCleanupPolicy, classify_ignored};
    use std::path::Path;

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
