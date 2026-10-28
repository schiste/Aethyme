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
//!    no protected standalone ignored file. All Git-ignored directories are
//!    disposable by default; strict mode instead requires no ignored residue.
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
use std::time::{Duration, Instant};

use crate::types::{Session, SessionCleanupState, SessionOrigin};
use crate::{Broker, BrokerOpError, GitRepo};

/// Fallback classification for ignored files and artifact-root symlinks.
/// Ignored directories are disposable independently of this catalog unless
/// strict mode is enabled. Each fallback is recognised by its exact name and the
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
const SESSION_RETIREMENT_CURSOR_KEY: &str = "auto_cleanup.session_retirement_cursor";
const SESSION_RETIREMENT_CURSOR_VERSION: u32 = 1;
const SESSION_RETIREMENT_PAGE_SIZE: usize = 128;
/// Bound discovery of ignored roots before whole-checkout cleanup considers
/// their existing policy classifications.
const IGNORED_PATH_LISTING_BUDGET: Duration = Duration::from_secs(15);

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

/// Durable progress for the bounded open-session retirement lane. It never
/// authorizes closure: each row is reloaded and judged against the current
/// activity and holder evidence before the state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SessionRetirementCursor {
    version: u32,
    after_session_id: i64,
    high_water_session_id: i64,
}

impl Default for SessionRetirementCursor {
    fn default() -> Self {
        Self {
            version: SESSION_RETIREMENT_CURSOR_VERSION,
            after_session_id: 0,
            high_water_session_id: 0,
        }
    }
}

fn decode_session_retirement_cursor(value: Option<&str>) -> SessionRetirementCursor {
    value
        .and_then(|value| serde_json::from_str::<SessionRetirementCursor>(value).ok())
        .filter(|cursor| {
            cursor.version == SESSION_RETIREMENT_CURSOR_VERSION
                && cursor.after_session_id >= 0
                && cursor.high_water_session_id >= cursor.after_session_id
        })
        .unwrap_or_default()
}

/// Reuse one process-table observation during planning, but take it lazily:
/// most closed checkouts have no retained lease rows to classify. Apply gives
/// each candidate its own fresh instance under the lifecycle claim.
#[derive(Default)]
struct LeaseProcessTableSnapshot {
    snapshot: Option<Option<crate::session_holder::ProcessTable>>,
}

impl LeaseProcessTableSnapshot {
    fn get_or_snapshot(&mut self) -> Option<&crate::session_holder::ProcessTable> {
        self.snapshot
            .get_or_insert_with(crate::session_holder::ProcessTable::snapshot)
            .as_ref()
    }
}

struct AutoCleanupVerdictOptions<'a> {
    deep: bool,
    deadline: Option<Instant>,
    selected: Option<&'a ContainmentProof>,
    lease_process_table: &'a mut LeaseProcessTableSnapshot,
}

fn checkout_group_cursor_session_id(group: &AutoCleanupCheckoutGroup) -> i64 {
    group
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
    /// Require a checkout with no ignored leftovers. Otherwise all Git-ignored
    /// directories are disposable without inspecting their contents.
    pub strict: bool,
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
            strict: false,
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
                "strict" => {
                    policy.strict = value
                        .as_bool()
                        .ok_or_else(|| "[cleanup] strict must be true or false".to_string())?;
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

    /// Strict mode (or an unreadable policy) leaves ignored output to an
    /// explicit reviewed cleanup instead of a background artifact sweep.
    pub(crate) fn artifact_sweep_allowed(main_root: &Path) -> bool {
        Self::load(main_root).is_ok_and(|policy| !policy.strict)
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
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_dir() && !metadata.file_type().is_symlink() {
        return None;
    }
    // Never probe an artifact-root symlink's target for a witness. Known
    // dependency/cache links themselves are disposable; their targets are not.
    if metadata.file_type().is_symlink() {
        return match name {
            "node_modules" | ".pnpm-store" | ".venv" | "__pycache__" | ".pytest_cache"
            | ".mypy_cache" | ".ruff_cache" => Some(false),
            _ if policy.configured_regenerable_dir(relative_dir) => Some(false),
            _ => None,
        };
    }
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

/// Classification used by recovery archives, which preserve protected
/// standalone ignored files while still discarding policy-approved output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryIgnoredClassification {
    /// An ignored directory root. Its contents stay opaque and are disposable
    /// under the existing cleanup policy.
    DisposableDirectory,
    /// A standalone path the current cleanup policy permits removing.
    DisposableOutput,
    /// A standalone path the policy protects and recovery must archive.
    Preserve(&'static str),
    /// Strict mode requires an explicit operator decision for every leftover.
    StrictRefusal,
}

/// Classify an entry from Git's ignored listing. Directories are disposable
/// opaque output by default, including local data and session records inside
/// them. Standalone files need the existing metadata/artifact classification.
/// Strict mode keeps every ignored leftover for explicit operator cleanup.
pub(crate) fn classify_ignored_for_recovery(
    worktree: &Path,
    relative: &str,
    policy: &AutoCleanupPolicy,
) -> Result<RecoveryIgnoredClassification, String> {
    if policy.strict {
        return Ok(RecoveryIgnoredClassification::StrictRefusal);
    }
    let trimmed = relative.trim_end_matches('/');
    if trimmed.is_empty()
        || Path::new(trimmed)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(format!("invalid ignored path {relative:?}"));
    }
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if (!relative.ends_with('/') && name == ".DS_Store")
        || crate::runtime_paths::is_broker_runtime_path(relative)
    {
        return Ok(RecoveryIgnoredClassification::DisposableOutput);
    }
    let metadata = std::fs::symlink_metadata(worktree.join(trimmed))
        .map_err(|error| format!("cannot stat ignored path {relative}: {error}"))?;
    // Git produced this ignored path. An ignored directory is disposable by
    // default regardless of its name, size or nested contents. Do not follow
    // symlinks to discover whether they happen to target a directory.
    if metadata.is_dir() {
        return Ok(RecoveryIgnoredClassification::DisposableDirectory);
    }
    let Some((_enclosing, cargo_target)) = enclosing_regenerable(worktree, trimmed, policy) else {
        return Ok(RecoveryIgnoredClassification::Preserve(
            "is not inside a recognised build-output directory",
        ));
    };
    if trimmed.split('/').any(|component| component == ".aethyme") && !cargo_target {
        return Ok(RecoveryIgnoredClassification::Preserve(
            "holds Aethyme state",
        ));
    }
    // The policy declares this output disposable. Package fixtures routinely
    // contain keys, databases, dangling links and hundreds of thousands of files;
    // inspecting those contents both invents blockers and scales with the output.
    // Worktree removal unlinks symlinks; it never follows their targets.
    Ok(RecoveryIgnoredClassification::DisposableOutput)
}

/// Cleanup's removal verdict remains conservative: only paths with an
/// explicit disposable classification pass.
pub(crate) fn classify_ignored(
    worktree: &Path,
    relative: &str,
    policy: &AutoCleanupPolicy,
) -> Result<(), String> {
    match classify_ignored_for_recovery(worktree, relative, policy)? {
        RecoveryIgnoredClassification::DisposableDirectory
        | RecoveryIgnoredClassification::DisposableOutput => Ok(()),
        RecoveryIgnoredClassification::Preserve(reason) => {
            Err(format!("ignored path {relative} {reason}"))
        }
        RecoveryIgnoredClassification::StrictRefusal => Err(format!(
            "strict cleanup requires no ignored leftovers; remove {relative} manually or review an explicit discard"
        )),
    }
}

/// Snapshot the ignored directory roots that passed the same cleanup
/// classifier before finish starts removal. The resulting prefixes are part
/// of its durable recovery proof; an orphan retry may discard contents only
/// beneath these roots after Git metadata has gone.
pub(crate) fn disposable_ignored_directory_prefixes(
    worktree: &Path,
    policy: &AutoCleanupPolicy,
) -> Result<Vec<String>, String> {
    let deadline = Instant::now() + IGNORED_PATH_LISTING_BUDGET;
    let expected_root = worktree
        .canonicalize()
        .map_err(|error| format!("cannot resolve cleanup checkout root: {error}"))?;
    let checkout = GitRepo::discover(worktree).map_err(|error| error.to_string())?;
    let discovered_root = checkout
        .root()
        .canonicalize()
        .map_err(|error| format!("cannot resolve discovered Git root: {error}"))?;
    if discovered_root != expected_root {
        return Err("Git resolved a different root than the cleanup checkout".into());
    }

    let ignored = checkout
        .ignored_paths_readonly_until(deadline)
        .map_err(|error| error.to_string())?;
    let mut prefixes = Vec::new();
    for entry in ignored {
        let relative = if entry.directory_marker {
            format!("{}/", entry.relative)
        } else {
            entry.relative.clone()
        };
        match classify_ignored_for_recovery(worktree, &relative, policy)? {
            RecoveryIgnoredClassification::DisposableDirectory if entry.directory_marker => {
                prefixes.push(format!("{}/", entry.relative));
            }
            RecoveryIgnoredClassification::DisposableOutput if !entry.directory_marker => {}
            RecoveryIgnoredClassification::DisposableDirectory => {
                return Err(format!(
                    "ignored path {relative} changed shape while finish cleanup was being proved"
                ));
            }
            // These entries receive no discard prefix. Keeping the identity
            // proof lets registered recovery re-run the active policy after
            // an operator resolves them; orphan recovery will reject any
            // surviving path that the proof did not authorize.
            RecoveryIgnoredClassification::Preserve(_)
            | RecoveryIgnoredClassification::StrictRefusal
            | RecoveryIgnoredClassification::DisposableOutput => {}
        }
    }
    prefixes.sort();
    prefixes.dedup();
    Ok(prefixes)
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
    /// Whether automatic checkout removal is enabled. Session retirement is
    /// governed separately by the retention policy.
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
    pub evaluated_at_ms: i64,
    pub removed: Vec<AutoRemovedCheckout>,
    pub kept: Vec<AutoKeptCheckout>,
    /// Candidates, or one incomplete cleanup or retirement pass, left for
    /// later because its cursor or time budget did not allow completion (#460).
    pub deferred: usize,
    /// Open sessions this pass retired after the configured abandonment
    /// window and current holder-liveness checks allowed closure.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub sessions_retired: usize,
    /// Why an otherwise due session-retirement pass did not finish.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retirement_deferred_reasons: Vec<SessionRetirementDeferredReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionRetirementDeferredReason {
    /// The abandonment window could not be established from repository policy.
    RetentionPolicyUnavailable { message: String },
    /// The bounded maintenance deadline elapsed before the next session was checked.
    DeadlineExceeded { after_session_id: i64 },
    /// The recorded task holder could not be observed completely.
    HolderLivenessUnavailable { session_id: i64 },
    /// The existing worktree's Git activity metadata was not readable.
    WorktreeActivityUnavailable { session_id: i64 },
    /// Another lifecycle operation held the nonblocking close claim.
    LifecycleBusy { session_id: i64 },
    /// A concurrent SQLite writer prevented the atomic abandonment transition.
    DatabaseBusy { session_id: i64 },
    /// Lease liveness evidence was unavailable or an active lease still held.
    LeaseLivenessUnavailable { session_id: i64 },
    /// More open sessions remain in the current fixed-high-water lap.
    MoreSessionsRemain {
        after_session_id: i64,
        high_water_session_id: i64,
    },
}

#[derive(Debug, Default)]
struct SessionRetirementSummary {
    retired: usize,
    deferred: usize,
    reasons: Vec<SessionRetirementDeferredReason>,
}

fn is_zero_count(count: &usize) -> bool {
    *count == 0
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

/// A complete canonical checkout cohort, before any deletion proof is made.
/// Grouping all retained rows is required so aliases and colocated sessions
/// cannot be split into independent cleanup candidates.
#[derive(Debug, Clone)]
struct AutoCleanupCheckoutGroup {
    worktree: PathBuf,
    sessions: Vec<Session>,
}

struct AutoCleanupGroupScan {
    groups: Vec<AutoCleanupCheckoutGroup>,
    live: Vec<Session>,
    live_worktrees: Vec<ResolvedWorktreePath>,
}

enum AutoCleanupGroupInspection {
    Candidate(Box<AutoCleanupCandidate>),
    Kept(AutoKeptCheckout),
    Deferred,
}

impl AutoCleanupCandidate {
    fn session_ids(&self) -> Vec<i64> {
        self.sessions.iter().map(|session| session.id).collect()
    }

    fn branch(&self) -> &str {
        self.spawned_owner.branch.as_str()
    }
}

/// Group all extant retained rows by canonical checkout path. This pass is
/// intentionally complete: a partial cohort is never deletion authority.
fn group_closed_checkout_sessions(
    closed: Vec<Session>,
    deadline: Option<Instant>,
) -> Option<Vec<AutoCleanupCheckoutGroup>> {
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut by_path: std::collections::BTreeMap<PathBuf, Vec<Session>> =
        std::collections::BTreeMap::new();
    for session in closed {
        if deadline_expired(deadline) {
            return None;
        }
        if session.cleanup_state == SessionCleanupState::Cleaned {
            continue;
        }
        let path = PathBuf::from(&session.worktree_path);
        if !path.is_dir() {
            if deadline_expired(deadline) {
                return None;
            }
            continue;
        }
        let canonical_path = canonical(&path);
        if deadline_expired(deadline) {
            return None;
        }
        by_path.entry(canonical_path).or_default().push(session);
    }
    if deadline_expired(deadline) {
        return None;
    }
    Some(
        by_path
            .into_iter()
            .map(|(worktree, sessions)| AutoCleanupCheckoutGroup { worktree, sessions })
            .collect(),
    )
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
    crate::git::checked_worktree_git_dir(worktree)
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedWorktreePath {
    raw: PathBuf,
    canonical_or_raw: PathBuf,
    git_dir: Option<PathBuf>,
    resolution_errors: Vec<String>,
}

impl ResolvedWorktreePath {
    pub(crate) fn resolve(path: &Path) -> Self {
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
pub(crate) enum LiveOverlapEvidence {
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
pub(crate) fn resolve_live_worktree_paths(
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

pub(crate) fn live_worktree_overlap(
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

pub(crate) fn overlap_resolution_error(
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
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    let changed = status
        .split('\0')
        .filter(|entry| entry.len() > 3)
        .filter(|entry| {
            // Finder metadata is not somebody's change.
            policy.strict
                || !(entry.starts_with("?? ") && entry[3..].rsplit('/').next() == Some(".DS_Store"))
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

    ignored_content_reason(worktree, policy)
}

/// Shared ignored-content verdict for automatic and finish-time cleanup.
/// Callers still prove tracked cleanliness, ownership, liveness and delivery.
pub(crate) fn ignored_content_reason(
    worktree: &Path,
    policy: &AutoCleanupPolicy,
) -> Result<Option<String>, String> {
    let deadline = Instant::now() + IGNORED_PATH_LISTING_BUDGET;
    crate::git::with_git_deadline(IGNORED_PATH_LISTING_BUDGET, || {
        ensure_ignored_content_before(deadline)?;
        let expected_root = worktree
            .canonicalize()
            .map_err(|error| format!("cannot resolve cleanup checkout root: {error}"))?;
        let repo = GitRepo::discover(worktree).map_err(|error| error.to_string())?;
        let discovered_root = repo
            .root()
            .canonicalize()
            .map_err(|error| format!("cannot resolve discovered Git root: {error}"))?;
        if discovered_root != expected_root {
            return Err("Git resolved a different root than the cleanup checkout".into());
        }
        let ignored = repo
            .ignored_paths_readonly_until(deadline)
            .map_err(|error| error.to_string())?;
        // Git supplies ignored roots; never recursively walk their contents.
        // Keep every listed entry in the existing whole-checkout classifier:
        // typed protection here is only for future in-place artifact discovery.
        for entry in ignored {
            ensure_ignored_content_before(deadline)?;
            let relative = if entry.directory_marker {
                format!("{}/", entry.relative)
            } else {
                entry.relative
            };
            let classification = classify_ignored(worktree, &relative, policy);
            ensure_ignored_content_before(deadline)?;
            if let Err(reason) = classification {
                return Ok(Some(format!(
                    "ignored content requires explicit cleanup: {reason}"
                )));
            }
        }
        ensure_ignored_content_before(deadline)?;
        Ok(None)
    })
}

fn ensure_ignored_content_before(deadline: Instant) -> Result<(), String> {
    let deadline =
        crate::git::active_git_deadline().map_or(deadline, |active| active.min(deadline));
    if Instant::now() >= deadline {
        Err("ignored-path classification exceeded its time budget".into())
    } else {
        Ok(())
    }
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

    /// Collect complete canonical cohorts and a complete live-path index.
    /// This pass does no per-cohort Git or ownership proof: the durable cursor
    /// must choose a fair tranche before that work begins.
    fn auto_cleanup_checkout_groups(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<AutoCleanupGroupScan>, BrokerOpError> {
        if deadline_expired(deadline) {
            return Ok(None);
        }
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
        let Some(groups) = group_closed_checkout_sessions(closed, deadline) else {
            return Ok(None);
        };
        Ok(Some(AutoCleanupGroupScan {
            groups,
            live,
            live_worktrees,
        }))
    }

    /// Make the existing ownership and live-session proofs for one complete
    /// cohort. A raw group returned by auto_cleanup_checkout_groups never
    /// authorizes deletion by itself.
    fn auto_cleanup_candidate_for_group(
        &mut self,
        group: &AutoCleanupCheckoutGroup,
        live: &[Session],
        live_worktrees: &[ResolvedWorktreePath],
        deadline: Option<Instant>,
    ) -> Result<AutoCleanupGroupInspection, BrokerOpError> {
        let path = &group.worktree;
        let sessions = &group.sessions;
        let ids = sessions
            .iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        let keep = |reason: String| {
            AutoCleanupGroupInspection::Kept(AutoKeptCheckout {
                worktree: path.to_string_lossy().into_owned(),
                sessions: ids.clone(),
                reason,
            })
        };
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let main_root = canonical(self.main_root());
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        if *path == main_root {
            return Ok(keep("the primary checkout is never removed".into()));
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
            return Ok(keep(
                "no matching spawn-time worktree instance witness".into(),
            ));
        }
        // Only a still-retained spawned row with a persisted witness for this
        // exact Git worktree instance proves broker creation. Adopted rows and
        // legacy spawned rows without a witness cannot authorize cleanup.
        let checkout_result = {
            let _git_deadline = deadline.map(crate::git::limit_git_until);
            GitRepo::discover(path)
        };
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        let checkout = match checkout_result {
            Ok(checkout) => checkout,
            Err(_) => return Ok(keep("not a readable broker-owned worktree".into())),
        };
        let current_witness = {
            let _git_deadline = deadline.map(crate::git::limit_git_until);
            checkout.spawned_worktree_instance_witness()
        };
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        let mut matched_owner = None;
        let mut diagnostic_owner = None;
        let mut unreadable_witness = None;
        for session in spawned_sessions {
            if deadline_expired(deadline) {
                return Ok(AutoCleanupGroupInspection::Deferred);
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
                return Ok(AutoCleanupGroupInspection::Deferred);
            }
            if witness
                .as_ref()
                .is_some_and(|witness| witness.worktree_path != path.to_string_lossy().as_ref())
            {
                continue;
            }
            if current_witness
                .as_ref()
                .is_some_and(|current| witness.as_ref().is_some_and(|witness| witness == current))
            {
                matched_owner = Some((session.clone(), witness));
                break;
            }
            // Keep an unproven spawn row only as a diagnostic candidate. The
            // planner still refuses it before containment or removal.
            if diagnostic_owner.is_none() {
                diagnostic_owner = Some((session.clone(), witness));
            }
        }
        let owner = matched_owner.or(diagnostic_owner);
        let Some((spawned_owner, spawned_instance_witness)) = owner else {
            let reason = unreadable_witness
                .map(|error| format!("spawn-time worktree identity is unreadable: {error}"))
                .unwrap_or_else(|| "no matching spawn-time worktree instance witness".into());
            return Ok(keep(reason));
        };
        let is_owned = {
            let _git_deadline = deadline.map(crate::git::limit_git_until);
            self.is_broker_owned_worktree(&spawned_owner, Path::new(&spawned_owner.worktree_path))
        };
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        if !is_owned {
            return Ok(keep("not a broker-owned worktree".into()));
        }
        // A session still open anywhere in this tree keeps it, whatever state
        // it reports. The full live index was resolved before any groups.
        let candidate_worktree = ResolvedWorktreePath::resolve(path);
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        if let Some((index, evidence)) =
            first_live_worktree_overlap(&candidate_worktree, live_worktrees)
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
            return Ok(keep(reason));
        }
        let candidate = AutoCleanupCandidate {
            worktree: path.clone(),
            spawned_owner,
            spawned_instance_witness,
            sessions: sessions.clone(),
        };
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        Ok(AutoCleanupGroupInspection::Candidate(Box::new(candidate)))
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
    fn auto_cleanup_verdict(
        &mut self,
        candidate: &AutoCleanupCandidate,
        policy: &AutoCleanupPolicy,
        upstream: &(String, String),
        options: AutoCleanupVerdictOptions<'_>,
    ) -> Result<ContainmentProof, NotRemoved> {
        let AutoCleanupVerdictOptions {
            deep,
            deadline,
            selected,
            lease_process_table,
        } = options;
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
        // Reload retention for every verdict, including apply: a policy
        // changed after selection must take effect before any removal.
        let retention = crate::load_retention_policy(self.main_root())
            .map_err(|error| keep(format!("retention policy unreadable: {error}")))?;
        let evaluated_at_ms = now_ms();
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
            if retention.closed_worktree_in_grace(&current, evaluated_at_ms) {
                return Err(keep(format!(
                    "session {} is within the {} hour closed-worktree grace period",
                    current.id, retention.closed_worktree_grace_hours
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
        let leases = self
            .store()
            .unreleased_unexpired_leases_for_sessions_at(&ids, now)
            .map_err(|error| keep(format!("lease liveness unreadable: {error}")))?;
        if !leases.is_empty() {
            let lease_policy = crate::lease_liveness::LeaseLivenessPolicy::load(self.main_root());
            let liveness = crate::lease_liveness::assess(
                self.store(),
                &leases,
                lease_process_table.get_or_snapshot(),
                now,
                lease_policy,
            )
            .map_err(|error| keep(format!("lease liveness unreadable: {error}")))?;
            if let Some(lease) = liveness.iter().find(|lease| lease.liveness_evidence.holds) {
                return Err(keep(format!(
                    "lease on {} is {} and still holds",
                    lease.path,
                    lease.liveness.as_str()
                )));
            }
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

    fn session_retirement_cursor(
        &mut self,
    ) -> Result<(SessionRetirementCursor, Option<String>), BrokerOpError> {
        let raw = self.store().meta_get(SESSION_RETIREMENT_CURSOR_KEY)?;
        Ok((decode_session_retirement_cursor(raw.as_deref()), raw))
    }

    fn persist_session_retirement_cursor(
        &mut self,
        cursor: &SessionRetirementCursor,
        expected_raw: &mut Option<String>,
    ) -> Result<(), BrokerOpError> {
        let value = serde_json::to_string(cursor)
            .expect("serializing the session-retirement cursor is infallible");
        if !self.store().meta_compare_and_set(
            SESSION_RETIREMENT_CURSOR_KEY,
            expected_raw.as_deref(),
            &value,
        )? {
            return Err(BrokerOpError::AutoCleanupCursorChanged);
        }
        *expected_raw = Some(value);
        Ok(())
    }

    /// Retire a bounded keyset page of sessions whose existing abandonment
    /// window has elapsed. This runs only in the mutating maintenance path;
    /// cleanup planning and read-only session snapshots remain unchanged.
    fn retire_abandoned_sessions(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<SessionRetirementSummary, BrokerOpError> {
        let mut summary = SessionRetirementSummary::default();
        if deadline_expired(deadline) {
            summary.deferred = 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::DeadlineExceeded {
                    after_session_id: 0,
                });
            return Ok(summary);
        }

        let policy = match crate::load_retention_policy(self.main_root()) {
            Ok(policy) => policy,
            Err(error) => {
                summary.deferred = 1;
                summary.reasons.push(
                    SessionRetirementDeferredReason::RetentionPolicyUnavailable {
                        message: error.to_string(),
                    },
                );
                return Ok(summary);
            }
        };
        let window_ms = i64::from(policy.session_abandoned_after_hours)
            .saturating_mul(3_600_000);
        if window_ms <= 0 {
            return Ok(summary);
        }
        if deadline_expired(deadline) {
            summary.deferred = 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::DeadlineExceeded {
                    after_session_id: 0,
                });
            return Ok(summary);
        }

        let (mut cursor, mut expected_raw) = self.session_retirement_cursor()?;
        let current_high_water = self.store().open_session_max_id()?;
        if deadline_expired(deadline) {
            summary.deferred = 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::DeadlineExceeded {
                    after_session_id: cursor.after_session_id,
                });
            return Ok(summary);
        }
        if cursor.after_session_id >= cursor.high_water_session_id
            || current_high_water <= cursor.after_session_id
        {
            cursor.after_session_id = 0;
            cursor.high_water_session_id = current_high_water;
            self.persist_session_retirement_cursor(&cursor, &mut expected_raw)?;
        }
        if cursor.high_water_session_id == 0 {
            return Ok(summary);
        }

        let sessions = self.store().open_session_page(
            cursor.after_session_id,
            cursor.high_water_session_id,
            SESSION_RETIREMENT_PAGE_SIZE,
        )?;
        if deadline_expired(deadline) {
            summary.deferred = 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::DeadlineExceeded {
                    after_session_id: cursor.after_session_id,
                });
            return Ok(summary);
        }
        if sessions.is_empty() {
            cursor.after_session_id = cursor.high_water_session_id;
            self.persist_session_retirement_cursor(&cursor, &mut expected_raw)?;
            return Ok(summary);
        }

        let retirement_now = now_ms();
        let retirement_candidates = sessions
            .iter()
            .filter(|session| {
                crate::decide_abandonment(
                    &crate::SessionActivity {
                        session_id: session.id,
                        last_activity_at: session.last_activity_at,
                        created_at: session.created_at,
                        agent_alive: None,
                        closed: false,
                    },
                    retirement_now,
                    window_ms,
                )
                .is_abandoned()
            })
            .map(|session| session.id)
            .collect::<Vec<_>>();
        let mut holder_process_table = crate::session_holder::RecordedHolderProcessTable::default();
        holder_process_table.prime(self.store_ref(), &retirement_candidates, deadline);
        for session in sessions {
            if deadline_expired(deadline) {
                summary.deferred += 1;
                summary
                    .reasons
                    .push(SessionRetirementDeferredReason::DeadlineExceeded {
                        after_session_id: cursor.after_session_id,
                    });
                return Ok(summary);
            }

            let allow_snapshot = match deadline {
                Some(deadline) => {
                    deadline.saturating_duration_since(Instant::now())
                        > crate::session_holder::PROCESS_TABLE_TIMEOUT
                            + Duration::from_millis(100)
                }
                None => true,
            };
            match self.try_close_abandoned_session(
                session.id,
                now_ms(),
                window_ms,
                &mut holder_process_table,
                allow_snapshot,
            )? {
                crate::broker::AbandonedSessionCloseOutcome::Closed => summary.retired += 1,
                crate::broker::AbandonedSessionCloseOutcome::HolderUnavailable => {
                    summary.deferred += 1;
                    summary.reasons.push(
                        SessionRetirementDeferredReason::HolderLivenessUnavailable {
                            session_id: session.id,
                        },
                    );
                }
                crate::broker::AbandonedSessionCloseOutcome::LeaseUnavailable
                | crate::broker::AbandonedSessionCloseOutcome::LeaseHeld => {
                    summary.deferred += 1;
                    summary.reasons.push(
                        SessionRetirementDeferredReason::LeaseLivenessUnavailable {
                            session_id: session.id,
                        },
                    );
                }
                crate::broker::AbandonedSessionCloseOutcome::WorktreeActivityUnavailable => {
                    summary.deferred += 1;
                    summary.reasons.push(
                        SessionRetirementDeferredReason::WorktreeActivityUnavailable {
                            session_id: session.id,
                        },
                    );
                }
                crate::broker::AbandonedSessionCloseOutcome::LifecycleBusy => {
                    summary.deferred += 1;
                    summary.reasons.push(SessionRetirementDeferredReason::LifecycleBusy {
                        session_id: session.id,
                    });
                }
                crate::broker::AbandonedSessionCloseOutcome::DatabaseBusy => {
                    summary.deferred += 1;
                    summary.reasons.push(SessionRetirementDeferredReason::DatabaseBusy {
                        session_id: session.id,
                    });
                    // Keep the cursor on this row. Do not perform another DB
                    // write while the competing writer still holds the lock.
                    return Ok(summary);
                }
                crate::broker::AbandonedSessionCloseOutcome::NotDue
                | crate::broker::AbandonedSessionCloseOutcome::Changed
                | crate::broker::AbandonedSessionCloseOutcome::ActiveProcess => {}
            }
            cursor.after_session_id = session.id;
            self.persist_session_retirement_cursor(&cursor, &mut expected_raw)?;
        }

        if deadline_expired(deadline) {
            summary.deferred += 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::DeadlineExceeded {
                    after_session_id: cursor.after_session_id,
                });
            return Ok(summary);
        }
        let more = self.store().open_session_page(
            cursor.after_session_id,
            cursor.high_water_session_id,
            1,
        )?;
        if more.is_empty() {
            cursor.after_session_id = cursor.high_water_session_id;
            self.persist_session_retirement_cursor(&cursor, &mut expected_raw)?;
        } else {
            summary.deferred += 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::MoreSessionsRemain {
                    after_session_id: cursor.after_session_id,
                    high_water_session_id: cursor.high_water_session_id,
                });
        }
        Ok(summary)
    }

    /// Run one auto-cleanup pass within `deadline`: select, then revalidate
    /// and remove. The caller holds the GC lock for the whole pass.
    pub(crate) fn auto_remove_disposable_checkouts(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<AutoCleanupReport, BrokerOpError> {
        let retirement = self.retire_abandoned_sessions(deadline)?;
        if retirement.reasons.iter().any(|reason| {
            matches!(reason, SessionRetirementDeferredReason::DatabaseBusy { .. })
        }) {
            // A busy abandonment CAS is a reason to defer this pass as a
            // whole. In particular, skip checkout planning and its cursor
            // writes while the other database writer may still be active.
            let (enabled, config_error) = match AutoCleanupPolicy::load(self.main_root()) {
                Ok(policy) => (policy.auto_remove, None),
                Err(error) => (false, Some(error.to_string())),
            };
            return Ok(AutoCleanupReport {
                enabled,
                config_error,
                evaluated_at_ms: now_ms(),
                deferred: retirement.deferred,
                sessions_retired: retirement.retired,
                retirement_deferred_reasons: retirement.reasons,
                ..AutoCleanupReport::default()
            });
        }
        let mut plan = self.auto_cleanup_plan(deadline)?;
        plan.report.sessions_retired = retirement.retired;
        plan.report.deferred += retirement.deferred;
        plan.report.retirement_deferred_reasons = retirement.reasons;
        self.auto_cleanup_apply(plan)
    }

    /// Select the checkouts every proof currently allows removing. Removes
    /// nothing; [`Self::auto_cleanup_apply`] re-checks each before acting.
    pub(crate) fn auto_cleanup_plan(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<AutoCleanupPlan, BrokerOpError> {
        self.auto_cleanup_plan_with_group_inspector(
            deadline,
            |broker, group, live, live_worktrees, deadline| {
                broker.auto_cleanup_candidate_for_group(group, live, live_worktrees, deadline)
            },
        )
    }

    /// Planner seam used to verify that cursor rotation happens before
    /// per-group Git and ownership work.
    fn auto_cleanup_plan_with_group_inspector<F>(
        &mut self,
        deadline: Option<Instant>,
        mut inspect_group: F,
    ) -> Result<AutoCleanupPlan, BrokerOpError>
    where
        F: FnMut(
            &mut Self,
            &AutoCleanupCheckoutGroup,
            &[Session],
            &[ResolvedWorktreePath],
            Option<Instant>,
        ) -> Result<AutoCleanupGroupInspection, BrokerOpError>,
    {
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
        let Some(mut scan) = self.auto_cleanup_checkout_groups(deadline)? else {
            report.deferred += 1;
            return Ok(AutoCleanupPlan::nothing(report));
        };

        // The complete canonical groups and full live index are available
        // before the cursor selects a tranche. Grouping still scans history;
        // the cursor prevents per-group Git and ownership checks from
        // restarting at the beginning when a selected proof uses the budget.
        let (mut cursor, mut expected_cursor) = self.auto_cleanup_cursor()?;
        let lap = auto_cleanup_lap_range(
            &mut scan.groups,
            &mut cursor,
            checkout_group_cursor_session_id,
        );
        let deferred_after_lap = scan.groups.len().saturating_sub(lap.end);
        let groups = scan.groups.drain(lap).collect::<Vec<_>>();
        report.deferred += deferred_after_lap;

        let mut upstream_checked = false;
        let mut upstream = None;
        let mut selected = Vec::new();
        let group_count = groups.len();
        let mut lease_process_table = LeaseProcessTableSnapshot::default();
        for (index, group) in groups.into_iter().enumerate() {
            if deadline_expired(deadline) {
                report.deferred += group_count - index;
                break;
            }
            let cursor_session_id = checkout_group_cursor_session_id(&group);
            let inspection =
                inspect_group(self, &group, &scan.live, &scan.live_worktrees, deadline)?;
            let mut stop_for_deadline = false;
            let inspection = if deadline_expired(deadline) {
                stop_for_deadline = true;
                AutoCleanupGroupInspection::Deferred
            } else {
                inspection
            };
            match inspection {
                AutoCleanupGroupInspection::Candidate(candidate) => {
                    let candidate = *candidate;
                    if !upstream_checked {
                        upstream_checked = true;
                        upstream = {
                            let _git_deadline = deadline.map(crate::git::limit_git_until);
                            self.remote_default_ref_and_tip()
                        };
                    }
                    if deadline_expired(deadline) {
                        report.deferred += 1;
                        stop_for_deadline = true;
                    } else if let Some(upstream) = upstream.as_ref() {
                        let verdict = self.auto_cleanup_verdict(
                            &candidate,
                            &policy,
                            upstream,
                            AutoCleanupVerdictOptions {
                                deep: false,
                                deadline,
                                selected: None,
                                lease_process_table: &mut lease_process_table,
                            },
                        );
                        let verdict = match verdict {
                            Err(NotRemoved::Defer) if !deadline_expired(deadline) => self
                                .auto_cleanup_verdict(
                                    &candidate,
                                    &policy,
                                    upstream,
                                    AutoCleanupVerdictOptions {
                                        deep: true,
                                        deadline,
                                        selected: None,
                                        lease_process_table: &mut lease_process_table,
                                    },
                                ),
                            verdict => verdict,
                        };
                        if deadline_expired(deadline) {
                            report.deferred += 1;
                            stop_for_deadline = true;
                        } else {
                            match verdict {
                                Ok(proof) => selected.push((candidate, proof)),
                                Err(NotRemoved::Defer) => report.deferred += 1,
                                Err(NotRemoved::Keep(reason)) => {
                                    report.kept.push(AutoKeptCheckout {
                                        worktree: candidate.worktree.to_string_lossy().into_owned(),
                                        sessions: candidate.session_ids(),
                                        reason,
                                    })
                                }
                            }
                        }
                    } else {
                        report.kept.push(AutoKeptCheckout {
                            worktree: candidate.worktree.to_string_lossy().into_owned(),
                            sessions: candidate.session_ids(),
                            reason: "no fetched remote default branch (origin/HEAD) to prove containment against".into(),
                        });
                    }
                }
                AutoCleanupGroupInspection::Kept(kept) => report.kept.push(kept),
                AutoCleanupGroupInspection::Deferred => {
                    report.deferred += 1;
                    // A selected-group deferral means its proof exhausted
                    // this tranche. Checkpoint it below, then let the next
                    // pass resume at the following complete group.
                    stop_for_deadline = true;
                }
            }
            cursor.after_session_id = cursor_session_id;
            self.persist_auto_cleanup_cursor(&cursor, &mut expected_cursor)?;
            if stop_for_deadline {
                report.deferred += group_count - index - 1;
                break;
            }
        }
        Ok(AutoCleanupPlan {
            report,
            policy,
            upstream,
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
        let open_file_snapshot = crate::reclaim::open_file_snapshot_under(Path::new("/"));
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
            if let Some(reason) = crate::reclaim::open_file_snapshot_blocker_for_worktree(
                &open_file_snapshot,
                &candidate.worktree,
            ) {
                keep_it(&mut report, reason);
                continue;
            }
            // Selection is not authority to keep using an old cleanup policy.
            // Recheck under the lifecycle lock for each candidate, including
            // opt-out, newly pinned paths and withdrawn regenerable globs.
            match AutoCleanupPolicy::load(self.main_root()) {
                Ok(current) if current == policy => {}
                Ok(current) => {
                    report.enabled = current.auto_remove;
                    keep_it(
                        &mut report,
                        "cleanup policy changed before removal; create a fresh plan".into(),
                    );
                    continue;
                }
                Err(error) => {
                    report.enabled = false;
                    report.config_error = Some(error.clone());
                    keep_it(
                        &mut report,
                        format!("cleanup policy unreadable before removal: {error}"),
                    );
                    continue;
                }
            }
            // Revalidate every proof now, under the lock, right before removal.
            let mut lease_process_table = LeaseProcessTableSnapshot::default();
            let proof = match self.auto_cleanup_verdict(
                &candidate,
                &policy,
                &upstream,
                AutoCleanupVerdictOptions {
                    deep: true,
                    deadline: None,
                    selected: Some(&selected_proof),
                    lease_process_table: &mut lease_process_table,
                },
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
        AUTO_CLEANUP_CURSOR_KEY, AutoCleanupCursor, AutoCleanupGroupInspection, AutoCleanupPolicy,
        LiveOverlapEvidence, ResolvedWorktreePath, SESSION_RETIREMENT_CURSOR_VERSION,
        SessionCleanupState, SessionRetirementCursor, SessionRetirementDeferredReason,
        auto_cleanup_lap_range,
        classify_ignored, decode_auto_cleanup_cursor, decode_session_retirement_cursor,
        first_live_worktree_overlap, group_closed_checkout_sessions, ignored_content_reason,
        resolve_worktree_paths_until,
    };
    #[cfg(unix)]
    use crate::git::GitRepo;
    use std::path::Path;
    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::process::Command;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    fn classify(root: &Path, relative: &str, policy: &AutoCleanupPolicy) -> Result<(), String> {
        classify_ignored(root, relative, policy)
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
    #[test]
    fn bounded_ignored_listing_keeps_existing_whole_checkout_policy() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.name", "Auto cleanup test"]);
        git(
            root,
            &["config", "user.email", "auto-cleanup@example.invalid"],
        );
        std::fs::write(
            root.join(".gitignore"),
            ".aethyme/\nopaque/\nprivate.sqlite\n",
        )
        .unwrap();
        git(root, &["add", ".gitignore"]);
        git(root, &["commit", "-qm", "ignore disposable output"]);
        touch(root, "opaque/nested/user-data.json");
        touch(root, ".aethyme/run/worker.json");

        let policy = AutoCleanupPolicy::default();
        assert_eq!(ignored_content_reason(root, &policy), Ok(None));

        touch(root, "private.sqlite");
        let reason = ignored_content_reason(root, &policy).unwrap().unwrap();
        assert!(reason.contains("private.sqlite"), "{reason}");
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
    fn retirement_fixture() -> (
        tempfile::TempDir,
        crate::test_storage::TestStorage,
        crate::Broker,
        crate::Session,
    ) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Auto cleanup test"]);
        git(&root, &["config", "user.email", "auto-cleanup@example.invalid"]);
        git(&root, &["commit", "--allow-empty", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 72\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let session = broker.start_worktree("retirement fixture", None).unwrap();
        (owner, storage, broker, session)
    }

    #[cfg(unix)]
    #[test]
    fn checked_worktree_activity_uses_the_gitdir_pointer_not_checkout_basename() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        let original = PathBuf::from(&session.worktree_path);
        let renamed = original.with_file_name("checkout-with-different-name");
        let original_git_dir = crate::git::checked_worktree_git_dir(&original)
            .unwrap()
            .expect("the spawned checkout has a gitdir pointer");
        std::fs::rename(&original, &renamed).unwrap();
        let connection = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        connection
            .execute(
                "UPDATE sessions SET worktree_path = ?1 WHERE id = ?2",
                rusqlite::params![renamed.to_string_lossy(), session.id],
            )
            .unwrap();

        let current = broker.store().session(session.id).unwrap();
        let resolved_git_dir = crate::git::checked_worktree_git_dir(&renamed)
            .unwrap()
            .unwrap();
        assert_eq!(resolved_git_dir, original_git_dir);
        assert_ne!(
            resolved_git_dir.file_name(),
            renamed.file_name(),
            "the admin directory can have a name independent of the checkout"
        );
        assert!(
            crate::broker::checked_worktree_activity_ms(broker.main_root(), &current)
                .unwrap()
                .is_some()
        );
    }

    #[cfg(unix)]
    fn age_open_session(broker: &mut crate::Broker, session: &crate::Session) {
        let db_path = broker.store().db_path().to_path_buf();
        let connection = rusqlite::Connection::open(db_path).unwrap();
        let old = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            - 80 * 60 * 60 * 1_000;
        let missing_worktree = PathBuf::from(&session.worktree_path)
            .with_file_name(format!("missing-retirement-worktree-{}", session.id))
            .to_string_lossy()
            .into_owned();
        connection
            .execute(
                "UPDATE sessions SET created_at = ?1, updated_at = ?1,
                 last_activity_at = ?1, worktree_path = ?2 WHERE id = ?3",
                rusqlite::params![old, missing_worktree, session.id],
            )
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn automatic_cleanup_retains_dirty_submodule_files_under_ignore_all_configuration() {
        for untracked in [false, true] {
            let case = if untracked { "untracked" } else { "modified" };
            let owner = tempfile::tempdir().unwrap();
            let root = owner.path().join("repository");
            let source = owner.path().join("submodule-source");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir_all(&source).unwrap();

            git(&source, &["init", "-q", "-b", "main"]);
            git(&source, &["config", "user.name", "Auto cleanup test"]);
            git(
                &source,
                &["config", "user.email", "auto-cleanup@example.invalid"],
            );
            std::fs::write(source.join("tracked.txt"), "initial child content\n").unwrap();
            git(&source, &["add", "tracked.txt"]);
            git(&source, &["commit", "-qm", "initial child commit"]);

            git(&root, &["init", "-q", "-b", "main"]);
            git(&root, &["config", "user.name", "Auto cleanup test"]);
            git(
                &root,
                &["config", "user.email", "auto-cleanup@example.invalid"],
            );
            std::fs::write(root.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
            std::fs::write(root.join("README.md"), "fixture\n").unwrap();
            git(&root, &["add", ".gitignore", "README.md"]);
            git(&root, &["commit", "-qm", "superproject base"]);
            let source_path = source.to_str().unwrap();
            git(
                &root,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "--quiet",
                    source_path,
                    "sub",
                ],
            );
            git(
                &root,
                &["config", "-f", ".gitmodules", "submodule.sub.ignore", "all"],
            );
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "-qm", "add child submodule"]);
            git(&root, &["config", "diff.ignoreSubmodules", "all"]);
            git(&root, &["config", "submodule.sub.ignore", "all"]);
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
            let session = broker
                .start_worktree("dirty child submodule", None)
                .unwrap();
            let worktree = Path::new(&session.worktree_path);
            assert_eq!(
                GitRepo::discover(worktree).unwrap().head_commit().unwrap(),
                GitRepo::discover(&root).unwrap().head_commit().unwrap(),
                "the superproject commit is already the parent checkout HEAD"
            );
            git(
                worktree,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "update",
                    "--init",
                    "--quiet",
                    "--",
                    "sub",
                ],
            );
            broker
                .close_abandoning_unpushed(session.id, "automatic cleanup regression")
                .unwrap();

            let child = worktree.join("sub");
            let dirty_path = if untracked {
                let path = child.join("new.txt");
                std::fs::write(&path, "new child content\n").unwrap();
                path
            } else {
                let path = child.join("tracked.txt");
                std::fs::write(&path, "changed child content\n").unwrap();
                path
            };

            let expected_path = worktree.to_string_lossy().into_owned();
            let report = broker.auto_remove_disposable_checkouts(None).unwrap();
            assert!(report.removed.is_empty(), "{case}");
            assert!(
                report.kept.iter().any(|kept| {
                    kept.worktree == expected_path
                        && kept.reason.contains("uncommitted or untracked path(s)")
                }),
                "{case}: {:?}",
                report.kept
            );
            assert!(worktree.is_dir(), "{case} worktree was removed");
            assert!(dirty_path.is_file(), "{case} child data was removed");
        }
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

        let alias_path = root.join("first-checkout-alias");
        std::os::unix::fs::symlink(&first_path, &alias_path).unwrap();
        let first_record = broker.store().session(first_id).unwrap();
        let mut aliased_second_record = broker.store().session(second_id).unwrap();
        aliased_second_record.worktree_path = alias_path.to_string_lossy().into_owned();
        let groups =
            group_closed_checkout_sessions(vec![first_record, aliased_second_record], None)
                .unwrap();
        assert_eq!(groups.len(), 1, "canonical aliases share a complete group");
        let mut grouped_ids = groups[0]
            .sessions
            .iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        grouped_ids.sort_unstable();
        assert_eq!(
            grouped_ids,
            vec![first_id, second_id],
            "all retained rows in a canonical alias cohort stay together"
        );

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

        let mut inspected_groups = Vec::new();
        let inspected_pass = broker
            .auto_cleanup_plan_with_group_inspector(None, |_, group, _, _, _| {
                inspected_groups.push(
                    group
                        .sessions
                        .iter()
                        .map(|session| session.id)
                        .collect::<Vec<_>>(),
                );
                Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                    worktree: group.worktree.to_string_lossy().into_owned(),
                    sessions: group.sessions.iter().map(|session| session.id).collect(),
                    reason: "injected planner inspection".into(),
                }))
            })
            .unwrap();
        assert_eq!(
            inspected_groups,
            vec![vec![second_id]],
            "only the cursor-selected group receives per-group proof work"
        );
        assert_eq!(inspected_pass.report.deferred, 0);
        let persisted = broker
            .store()
            .meta_get(AUTO_CLEANUP_CURSOR_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(
            decode_auto_cleanup_cursor(Some(&persisted)).after_session_id,
            second_id,
            "the selected group is checkpointed after its inspection"
        );
        broker
            .store()
            .meta_set(AUTO_CLEANUP_CURSOR_KEY, &seed_json)
            .unwrap();

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

        let (third_id, _) = closed_dirty_checkout(&mut broker, "cursor third candidate");
        assert!(second_id < third_id);
        let deferred_cursor = AutoCleanupCursor {
            version: super::AUTO_CLEANUP_CURSOR_VERSION,
            after_session_id: first_id,
            high_water_session_id: third_id,
        };
        let deferred_cursor_json = serde_json::to_string(&deferred_cursor).unwrap();
        broker
            .store()
            .meta_set(AUTO_CLEANUP_CURSOR_KEY, &deferred_cursor_json)
            .unwrap();

        let mut deferred_inspections = Vec::new();
        let deferred_pass = broker
            .auto_cleanup_plan_with_group_inspector(None, |_, group, _, _, _| {
                let session_id = group.sessions[0].id;
                deferred_inspections.push(session_id);
                if session_id == second_id {
                    Ok(AutoCleanupGroupInspection::Deferred)
                } else {
                    Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                        worktree: group.worktree.to_string_lossy().into_owned(),
                        sessions: vec![session_id],
                        reason: "injected resumed inspection".into(),
                    }))
                }
            })
            .unwrap();
        assert_eq!(deferred_inspections, vec![second_id]);
        assert_eq!(deferred_pass.report.deferred, 2);
        let checkpoint = broker
            .store()
            .meta_get(AUTO_CLEANUP_CURSOR_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(
            decode_auto_cleanup_cursor(Some(&checkpoint)).after_session_id,
            second_id,
            "a deferred selected group is checkpointed before the tranche stops"
        );

        let mut resumed_inspections = Vec::new();
        let resumed_pass = broker
            .auto_cleanup_plan_with_group_inspector(None, |_, group, _, _, _| {
                let session_id = group.sessions[0].id;
                resumed_inspections.push(session_id);
                Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                    worktree: group.worktree.to_string_lossy().into_owned(),
                    sessions: vec![session_id],
                    reason: "injected resumed inspection".into(),
                }))
            })
            .unwrap();
        assert_eq!(
            resumed_inspections,
            vec![third_id],
            "a fresh pass resumes at the next uninspected complete group"
        );
        assert_eq!(resumed_pass.report.deferred, 0);
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
    fn unknown_session_retirement_cursor_version_resets_scheduling_state() {
        let cursor = decode_session_retirement_cursor(Some(
            r#"{"version":9,"after_session_id":7,"high_water_session_id":12}"#,
        ));
        assert_eq!(cursor, SessionRetirementCursor::default());

        let checkpoint = SessionRetirementCursor {
            version: SESSION_RETIREMENT_CURSOR_VERSION,
            after_session_id: 7,
            high_water_session_id: 12,
        };
        let encoded = serde_json::to_string(&checkpoint).unwrap();
        assert_eq!(decode_session_retirement_cursor(Some(&encoded)), checkpoint);
    }

    #[test]
    fn session_retirement_report_fields_are_optional_for_legacy_json() {
        let legacy = r#"{"enabled":true,"config_error":null,"evaluated_at_ms":1,"removed":[],"kept":[],"deferred":0}"#;
        let report: super::AutoCleanupReport = serde_json::from_str(legacy).unwrap();
        assert_eq!(report.sessions_retired, 0);
        assert!(report.retirement_deferred_reasons.is_empty());
        let serialized = serde_json::to_value(report).unwrap();
        assert!(serialized.get("sessions_retired").is_none());
        assert!(serialized.get("retirement_deferred_reasons").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn open_session_pages_advance_by_id_without_materializing_the_inventory() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Auto cleanup test"]);
        git(&root, &["config", "user.email", "auto-cleanup@example.invalid"]);
        git(&root, &["commit", "--allow-empty", "-qm", "fixture"]);
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let first = broker.start_worktree("retirement page one", None).unwrap();
        let second = broker.start_worktree("retirement page two", None).unwrap();
        let third = broker.start_worktree("retirement page three", None).unwrap();
        let high_water = broker.store().open_session_max_id().unwrap();

        let first_page = broker
            .store()
            .open_session_page(0, high_water, 1)
            .unwrap();
        assert_eq!(first_page.len(), 1);
        assert_eq!(first_page[0].id, first.id);
        let second_page = broker
            .store()
            .open_session_page(first_page[0].id, high_water, 1)
            .unwrap();
        assert_eq!(second_page.len(), 1);
        assert_eq!(second_page[0].id, second.id);
        let tail = broker
            .store()
            .open_session_page(second_page[0].id, high_water, 128)
            .unwrap();
        assert_eq!(
            tail.iter().map(|session| session.id).collect::<Vec<_>>(),
            vec![third.id]
        );
        assert!(broker
            .store()
            .open_session_page(third.id, high_water, 1)
            .unwrap()
            .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn automatic_retirement_closes_old_unbound_sessions_after_the_existing_window() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        age_open_session(&mut broker, &session);

        let summary = broker.retire_abandoned_sessions(None).unwrap();
        assert_eq!(summary.retired, 1);
        assert_eq!(summary.deferred, 0);
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
    }

    #[cfg(unix)]
    #[test]
    fn automatic_retirement_defers_a_busy_database_writer_without_waiting() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        age_open_session(&mut broker, &session);
        let writer = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let mut process_table = crate::session_holder::RecordedHolderProcessTable::default();

        let started = Instant::now();
        let outcome = broker
            .try_close_abandoned_session(
                session.id,
                now_ms(),
                72 * 3_600_000,
                &mut process_table,
                true,
            )
            .unwrap();
        assert_eq!(outcome, crate::broker::AbandonedSessionCloseOutcome::DatabaseBusy);
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "a competing database writer defers retirement without delaying maintenance"
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Open
        );
    }

    #[cfg(unix)]
    #[test]
    fn busy_retirement_skips_checkout_planning_and_keeps_its_cursor() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        age_open_session(&mut broker, &session);
        let cursor = SessionRetirementCursor {
            version: SESSION_RETIREMENT_CURSOR_VERSION,
            after_session_id: 0,
            high_water_session_id: session.id,
        };
        let raw_cursor = serde_json::to_string(&cursor).unwrap();
        broker
            .store()
            .meta_set(SESSION_RETIREMENT_CURSOR_KEY, &raw_cursor)
            .unwrap();

        let writer = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let started = Instant::now();
        let report = broker.auto_remove_disposable_checkouts(None).unwrap();

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a busy retirement pass must stop before cursor or checkout writes"
        );
        assert_eq!(report.deferred, 1);
        assert!(matches!(
            report.retirement_deferred_reasons.as_slice(),
            [SessionRetirementDeferredReason::DatabaseBusy { session_id }]
                if *session_id == session.id
        ));
        assert_eq!(
            broker.store().meta_get(SESSION_RETIREMENT_CURSOR_KEY).unwrap(),
            Some(raw_cursor)
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Open
        );
    }

    #[cfg(unix)]
    #[test]
    fn automatic_retirement_keeps_a_session_with_unreadable_holder_evidence_open() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        age_open_session(&mut broker, &session);
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some("not-json"),
            )
            .unwrap();

        let summary = broker.retire_abandoned_sessions(None).unwrap();
        assert_eq!(summary.retired, 0);
        assert_eq!(summary.deferred, 1);
        assert!(matches!(
            summary.reasons.as_slice(),
            [SessionRetirementDeferredReason::HolderLivenessUnavailable { session_id }]
                if *session_id == session.id
        ));
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Open
        );
    }

    #[cfg(unix)]
    #[test]
    fn invalid_retention_policy_defers_session_retirement_explicitly() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Auto cleanup test"]);
        git(&root, &["config", "user.email", "auto-cleanup@example.invalid"]);
        git(&root, &["commit", "--allow-empty", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 'invalid'\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();

        let report = broker.auto_remove_disposable_checkouts(None).unwrap();
        assert_eq!(report.sessions_retired, 0);
        assert_eq!(report.deferred, 1);
        assert!(matches!(
            report.retirement_deferred_reasons.as_slice(),
            [SessionRetirementDeferredReason::RetentionPolicyUnavailable { .. }]
        ));
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
    fn ignored_directories_are_opaque_disposable_output_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        assert!(!policy.strict);
        for path in [
            "node_modules/pkg/server.pem",
            ".pnpm-store/.env",
            "unknown-data/local.sqlite",
            ".chau7/sessions/transcript.json",
            ".aethyme/state.json",
            "assets/build/logo.svg",
            "target-notes/plan.md",
        ] {
            touch(root, path);
        }
        for path in [
            "node_modules/",
            ".pnpm-store/",
            "unknown-data/",
            ".chau7/",
            ".aethyme/",
            "assets/build/",
            "target-notes/",
        ] {
            assert_eq!(classify(root, path, &policy), Ok(()), "{path}");
        }
        // No recursive scan: an unresolvable package-fixture link is ignored.
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            root.join("does-not-exist"),
            root.join("node_modules/dangling"),
        )
        .unwrap();
        assert_eq!(classify(root, "node_modules/", &policy), Ok(()));
    }

    #[test]
    fn standalone_ignored_files_and_invalid_paths_remain_protected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        for path in [".env", "private.sqlite", "notes.txt", "node_modules"] {
            touch(root, path);
            assert!(classify(root, path, &policy).is_err(), "{path}");
        }
        touch(root, ".DS_Store");
        assert_eq!(classify(root, ".DS_Store", &policy), Ok(()));
        touch(root, ".aethyme/generated/experience-status.md");
        assert_eq!(
            classify(root, ".aethyme/generated/experience-status.md", &policy),
            Ok(())
        );
        touch(root, ".aethyme/local-state.json");
        assert!(classify(root, ".aethyme/local-state.json", &policy).is_err());
        for path in ["../unknown-data/", "/unknown-data/", "node_modules/../.env"] {
            assert!(classify(root, path, &policy).is_err(), "{path}");
        }
    }

    #[test]
    fn strict_cleanup_requires_no_ignored_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy {
            strict: true,
            ..Default::default()
        };
        touch(root, "node_modules/pkg/index.js");
        touch(root, "unknown-data/local.sqlite");
        touch(root, ".DS_Store");
        touch(root, ".aethyme/generated/experience-status.md");
        for path in [
            "node_modules/",
            "unknown-data/",
            ".DS_Store",
            ".aethyme/generated/experience-status.md",
        ] {
            let reason = classify(root, path, &policy).unwrap_err();
            assert!(reason.contains("strict cleanup"), "{path}: {reason}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn dependency_root_links_are_classified_without_following_targets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::os::unix::fs::symlink(root.join("missing"), root.join("node_modules")).unwrap();
        assert_eq!(classify(root, "node_modules", &Default::default()), Ok(()));
        // A link with an unknown name is not assumed to point at a directory.
        std::os::unix::fs::symlink(root.join("missing"), root.join("unknown-link")).unwrap();
        assert!(classify(root, "unknown-link", &Default::default()).is_err());
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
        write("[cleanup]\nstrict = true\n");
        assert!(AutoCleanupPolicy::load(dir.path()).unwrap().strict);
        assert!(!AutoCleanupPolicy::artifact_sweep_allowed(dir.path()));
        write("[cleanup]\nstrict = false\n");
        assert!(!AutoCleanupPolicy::load(dir.path()).unwrap().strict);
        assert!(AutoCleanupPolicy::artifact_sweep_allowed(dir.path()));
        write("[cleanup]\nstrict = 1\n");
        assert!(AutoCleanupPolicy::load(dir.path()).is_err());
        assert!(!AutoCleanupPolicy::artifact_sweep_allowed(dir.path()));
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
