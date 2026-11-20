//! Git service layer: every git operation the broker performs, in one
//! module, shelling out to the `git` binary (no libgit2/gix until a
//! measured need appears — see docs/aethyme-local-agent-broker.md).
//!
//! No other broker code may run git directly. Keeping the surface here
//! makes the broker's git contract auditable and testable against real
//! throwaway repositories.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
#[cfg(unix)]
use std::io::Read;
use std::io::{Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_PRIVATE_INDEX_ID: AtomicU64 = AtomicU64::new(1);

/// A `core.hooksPath` under which no hook can exist: a path below a device
/// file is never a directory, so Git finds no executable hook there.
#[cfg(unix)]
const NO_HOOKS_PATH: &str = "/dev/null";
#[cfg(not(unix))]
const NO_HOOKS_PATH: &str = "NUL";

/// Bound the effective config copied into a one-shot exact-endpoint query.
/// It is captured from the real checkout, but replayed in an isolated Gitdir
/// after URL rewrite rules and include directives have been removed.
const MAX_PUSH_QUERY_CONFIG_BYTES: usize = 64 * 1024;
const MAX_PUSH_QUERY_CONFIG_ENTRIES: usize = 256;
const PUSH_EVIDENCE_REMOTE: &str = "aethyme-push-evidence";

thread_local! {
    /// A command-scoped deadline for callers that need a tighter bound than
    /// the general git timeout. Thread-local so one finish cannot shorten a
    /// concurrent operation running in another broker thread.
    static GIT_DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
    /// Ignore inherited Git environment and global/system configuration while
    /// synchronously verifying an exact graph snapshot. Thread-local so
    /// concurrent broker operations keep their ordinary Git behavior.
    static GRAPH_VERIFICATION_GIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Counts timed-out Git subprocesses on this thread. Some read helpers
    /// intentionally turn ordinary Git failures into `None`; a caller with a
    /// stronger deadline can use this generation to distinguish a timeout
    /// from an absent ref without changing those best-effort APIs.
    static GIT_TIMEOUT_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    /// Times this thread asked a remote what its branches point at.
    static TEST_REMOTE_QUERIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn test_remote_queries() -> usize {
    TEST_REMOTE_QUERIES.with(std::cell::Cell::get)
}

/// A snapshot for callers that must detect Git timeouts hidden by a
/// best-effort `Option`-returning read helper.
pub(crate) fn git_timeout_generation() -> u64 {
    GIT_TIMEOUT_GENERATION.with(std::cell::Cell::get)
}

fn note_git_timeout() {
    GIT_TIMEOUT_GENERATION.with(|generation| {
        generation.set(generation.get().wrapping_add(1));
    });
}

struct RestoreGitDeadline(Option<Instant>);

impl Drop for RestoreGitDeadline {
    fn drop(&mut self) {
        GIT_DEADLINE.with(|deadline| deadline.set(self.0));
    }
}

/// Apply one wall-clock deadline to all Git subprocesses run synchronously by
/// `operation`, preserving and restoring an outer deadline when nested.
pub(crate) fn with_git_deadline<T>(timeout: Duration, operation: impl FnOnce() -> T) -> T {
    let requested = Instant::now() + timeout;
    let previous = GIT_DEADLINE.with(|deadline| {
        let previous = deadline.get();
        let effective = previous.map_or(requested, |outer| outer.min(requested));
        deadline.set(Some(effective));
        previous
    });
    let _restore = RestoreGitDeadline(previous);
    operation()
}

/// Run `operation` with no command-scoped deadline, restoring the outer one
/// afterwards. For work that must not be killed halfway once started, such as
/// deleting a worktree after its session has closed.
pub(crate) fn without_git_deadline<T>(operation: impl FnOnce() -> T) -> T {
    let previous = GIT_DEADLINE.with(|deadline| deadline.replace(None));
    let _restore = RestoreGitDeadline(previous);
    operation()
}

/// Guard form of the same deadline, for an interactive report that installs
/// it for the whole command rather than one closure (#460). There is one
/// deadline per thread: a finish scope (`with_git_deadline`) and a report
/// scope (`limit_git_until`) nest by taking the earlier instant, and
/// lifting it (`without_git_deadline`) lifts both, so a mutation is never cut
/// off by whichever scope it did not lift.
pub(crate) struct GitDeadlineGuard {
    _restore: RestoreGitDeadline,
}

/// Cap Git commands on the current thread at `deadline`, keeping an earlier
/// outer deadline, until the guard is dropped.
pub(crate) fn limit_git_until(deadline: Instant) -> GitDeadlineGuard {
    let previous = GIT_DEADLINE.with(|slot| {
        let previous = slot.get();
        slot.set(Some(previous.map_or(deadline, |outer| outer.min(deadline))));
        previous
    });
    GitDeadlineGuard {
        _restore: RestoreGitDeadline(previous),
    }
}

pub(crate) fn active_git_deadline() -> Option<Instant> {
    GIT_DEADLINE.with(std::cell::Cell::get)
}

thread_local! {
    /// Worktree registrations listed once per run while a
    /// [`WorktreeInventoryRunScope`] is active on this thread, by the
    /// repository root that was asked. `git worktree list` costs seconds per
    /// call once a repository has hundreds of worktrees (17 s with 857), and
    /// the retirement lane asked it once per checkout it judged.
    static WORKTREE_INVENTORY_RUN_CACHE: std::cell::RefCell<
        Option<std::collections::HashMap<PathBuf, Vec<GitWorktreeInfo>>>,
    > = const { std::cell::RefCell::new(None) };
}

/// Reuses one `git worktree list` per repository root on this thread until
/// dropped. Thread-local by design: a worker thread that does not enter the
/// scope lists afresh, which is slower but never wrong. Callers still verify
/// the registration they act on from the repository's `worktrees/` directory,
/// so the cached listing only ever narrows, never authorizes.
pub(crate) struct WorktreeInventoryRunScope {
    installed: bool,
}

impl WorktreeInventoryRunScope {
    pub(crate) fn enter() -> Self {
        let installed = WORKTREE_INVENTORY_RUN_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.is_none() {
                *cache = Some(std::collections::HashMap::new());
                true
            } else {
                false
            }
        });
        Self { installed }
    }
}

impl Drop for WorktreeInventoryRunScope {
    fn drop(&mut self) {
        if self.installed {
            WORKTREE_INVENTORY_RUN_CACHE.with(|cache| *cache.borrow_mut() = None);
        }
    }
}

/// Drop `worktree` from every cached listing on this thread: its
/// registration was just removed, so a later lookup in the same run must
/// not find it.
pub(crate) fn forget_cached_worktree_registration(worktree: &Path) {
    let canonical = worktree.canonicalize().ok();
    WORKTREE_INVENTORY_RUN_CACHE.with(|cache| {
        if let Some(cache) = cache.borrow_mut().as_mut() {
            for entries in cache.values_mut() {
                entries.retain(|entry| {
                    entry.path != worktree
                        && (canonical.is_none() || entry.path.canonicalize().ok() != canonical)
                });
            }
        }
    });
}

struct RestoreGraphVerificationGit(bool);

impl Drop for RestoreGraphVerificationGit {
    fn drop(&mut self) {
        GRAPH_VERIFICATION_GIT.with(|enabled| enabled.set(self.0));
    }
}

/// Suppress inherited Git environment and global/system configuration for
/// synchronous graph verification and disposable quick tests, restoring any
/// outer scope on drop. The repository's local `.git/config` remains in
/// effect.
pub(crate) struct GraphVerificationGitGuard {
    _restore: RestoreGraphVerificationGit,
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

pub(crate) fn graph_verification_git_guard() -> GraphVerificationGitGuard {
    let previous = GRAPH_VERIFICATION_GIT.with(|enabled| enabled.replace(true));
    GraphVerificationGitGuard {
        _restore: RestoreGraphVerificationGit(previous),
        _not_send: std::marker::PhantomData,
    }
}

fn graph_verification_git_enabled() -> bool {
    GRAPH_VERIFICATION_GIT.with(std::cell::Cell::get)
}

/// The Git isolation in force on one thread, for a worker thread to adopt.
///
/// Both flags are thread-local, so a worker spawned to run Git on behalf of a
/// caller starts without them and would read the inherited `GIT_*`
/// environment and global configuration the caller had fenced off. That is
/// how a parallel lease scan ran an unrelated repository's hook from inside
/// the disposable quick test (2026-11-11, #1580).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GitIsolation {
    graph_verification: bool,
    drop_inherited_environment: bool,
}

/// The calling thread's Git isolation.
pub(crate) fn current_git_isolation() -> GitIsolation {
    GitIsolation {
        graph_verification: graph_verification_git_enabled(),
        drop_inherited_environment: DROP_INHERITED_GIT_ENVIRONMENT.with(std::cell::Cell::get),
    }
}

/// Run every Git command of this thread under `isolation`. Only for a thread
/// that ends with its work, because nothing restores the previous state.
pub(crate) fn adopt_git_isolation_on_this_thread(isolation: GitIsolation) {
    GRAPH_VERIFICATION_GIT.with(|enabled| enabled.set(isolation.graph_verification));
    DROP_INHERITED_GIT_ENVIRONMENT.with(|flag| flag.set(isolation.drop_inherited_environment));
}

/// Errors from git operations. `Git` carries the failing subcommand and
/// stderr so callers can surface actionable messages verbatim.
/// How every patch identity is computed. `--verbatim` keeps whitespace:
/// `--stable` (and Git's internal `--cherry-mark`/`git cherry` ids) strip it,
/// so a commit whose only change is whitespace -- a re-indented Python block,
/// tabs turned into spaces in a Makefile -- would count as already landed and
/// its worktree as safe to remove. Every caller is a safety verdict (landed,
/// unpushed, cleanup-safe, reconcile containment), and a re-indented port
/// reported as not landed is merely kept.
const PATCH_ID_ARGS: [&str; 2] = ["patch-id", "--verbatim"];

/// Diff formatting shared by recovery patches and patch-ID producers.
const PINNED_DIFF_FORMAT_ARGS: [&str; 5] = [
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

/// Additional options that make every patch-ID producer compare the same
/// complete, deterministic change set.
const PATCH_ID_DIFF_ARGS: [&str; 6] = [
    "--full-index",
    "--ignore-submodules=none",
    "--no-renames",
    "--diff-algorithm=myers",
    "--no-indent-heuristic",
    "--unified=3",
];

const MAX_LITERAL_PATHSPEC_BATCH_BYTES: usize = 64 * 1024;
const MAX_LITERAL_PATHSPEC_BATCH_COUNT: usize = 256;

fn patch_id_diff_args() -> Vec<&'static str> {
    let mut args = PINNED_DIFF_FORMAT_ARGS.to_vec();
    args.extend(PATCH_ID_DIFF_ARGS);
    args
}

fn first_parent_patch_id_log_args() -> Vec<&'static str> {
    let mut args = vec!["log", "--no-walk=unsorted", "--stdin", "-p"];
    args.extend(patch_id_diff_args());
    args.extend(["--diff-merges=first-parent", "--format=commit %H"]);
    args
}

/// Which side of a symmetric `left...right` range [`GitRepo::cherry_marked`]
/// lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CherrySide {
    Left,
    Right,
}

/// Patch identities reused only within one landing audit or cleanup plan.
/// Entries are scoped to the repository and are published only after a full,
/// successful Git batch. Empty diffs are cached as `None`.
#[derive(Debug, Default)]
pub(crate) struct PatchIdCache {
    entries: std::collections::BTreeMap<(PathBuf, String), Option<String>>,
    /// Changed-path signatures, see [`GitRepo::commits_sharing_changed_paths`].
    changed_paths: std::collections::BTreeMap<(PathBuf, String), ChangedPathSignature>,
    #[cfg(test)]
    hits: usize,
}

#[cfg(test)]
impl PatchIdCache {
    pub(crate) fn hit_count(&self) -> usize {
        self.hits
    }
}

/// Cap retained entries; exceeding it only causes a subsequent re-computation.
const MAX_PATCH_ID_CACHE_ENTRIES: usize = 65_536;

/// Digest of the paths one commit changes against its first parent.
type ChangedPathSignature = [u8; 32];

fn changed_path_log_args() -> Vec<&'static str> {
    vec![
        "log",
        "--no-walk=unsorted",
        "--stdin",
        "--name-only",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=none",
        "--no-renames",
        "--diff-merges=first-parent",
        // NUL cannot occur in a path line, so it delimits commits exactly.
        "--format=%x00%H",
    ]
}

/// Both side-specific patch-equivalence results for one symmetric range.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PairedCherryMarks {
    /// Commits unique to the left side, newest first.
    pub(crate) left_only: Vec<(String, bool)>,
    /// Commits unique to the right side, newest first.
    pub(crate) right_only: Vec<(String, bool)>,
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("failed to run git {args}: {source}")]
    Spawn {
        args: String,
        source: std::io::Error,
    },

    #[error("git {args} failed: {stderr}")]
    Git { args: String, stderr: String },

    #[error("{path} is not inside a git repository")]
    NotARepository { path: PathBuf },

    /// No `git` on PATH emits unmodified porcelain, so a dirtiness question
    /// has no honest answer. Carried separately from `Git` because nothing
    /// failed: the command would have succeeded and lied.
    #[error("refusing to judge this checkout: {detail}")]
    UntrustedOutput { detail: String },

    /// git ran past its deadline and was killed, or was never started because
    /// the deadline had already passed (#219). Nothing about the repository
    /// is known from it; the host, a remote or a lock is wedged. `detail`
    /// says which limit fired, whether Git started, where and for how long.
    #[error("{}", display_git_timeout(.args, .detail))]
    TimedOut {
        args: String,
        seconds: u64,
        detail: Box<GitTimeoutDetail>,
    },

    #[error("could not parse git worktree list --porcelain -z: {reason}")]
    WorktreeListParse { reason: String },

    /// A committed `.aethyme` configuration file is ambiguous or invalid.
    /// Boxed so the error enum stays small.
    #[error("{0}")]
    Config(Box<aethyme_config::ConfigSourceError>),
}

impl From<aethyme_config::ConfigSourceError> for GitError {
    fn from(error: aethyme_config::ConfigSourceError) -> Self {
        Self::Config(Box::new(error))
    }
}

/// Which limit ended a Git command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitTimeoutSource {
    /// The per-call limit (`AETHYME_GIT_TIMEOUT_SECS` or a caller cap) fired.
    PerCall,
    /// The enclosing command's wall-clock deadline fired first.
    OuterDeadline,
    /// The deadline had already passed, so Git was never started.
    NotStarted,
}

impl GitTimeoutSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerCall => "per_call",
            Self::OuterDeadline => "outer_deadline",
            Self::NotStarted => "not_started",
        }
    }

    fn counter(self) -> &'static str {
        match self {
            Self::PerCall => "git_timeout_per_call",
            Self::OuterDeadline => "git_timeout_outer_deadline",
            Self::NotStarted => "git_timeout_not_started",
        }
    }
}

/// The facts behind a [`GitError::TimedOut`]. `directory` is shortened to its
/// last two path components (or `~` for a home directory), so a message never
/// carries an absolute path or a user name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTimeoutDetail {
    pub source: GitTimeoutSource,
    pub started: bool,
    pub subcommand: String,
    pub directory: Option<String>,
    /// The limit that applied; for a command that never started, the time
    /// the deadline had left.
    pub limit_ms: u64,
    pub elapsed_ms: u64,
}

impl GitTimeoutDetail {
    fn seconds(ms: u64) -> String {
        format!("{:.1} s", ms as f64 / 1000.0)
    }

    /// One line: `git status timed out after 16.2 s (limit 15.0 s, per-call) in repo/wt`.
    pub fn message(&self, args: &str) -> String {
        let subcommand = if self.subcommand.is_empty() {
            args
        } else {
            self.subcommand.as_str()
        };
        let place = self
            .directory
            .as_deref()
            .map(|directory| format!(" in {directory}"))
            .unwrap_or_default();
        match self.source {
            GitTimeoutSource::NotStarted if self.limit_ms == 0 => format!(
                "git {subcommand} was not started: the command's deadline had no time left{place}"
            ),
            GitTimeoutSource::NotStarted => format!(
                "git {subcommand} was not started: the command's deadline had {} left{place}",
                Self::seconds(self.limit_ms)
            ),
            GitTimeoutSource::PerCall => format!(
                "git {subcommand} timed out after {} (limit {}, per-call){place} \
                 (AETHYME_GIT_TIMEOUT_SECS changes the general limit; caller caps may be shorter)",
                Self::seconds(self.elapsed_ms),
                Self::seconds(self.limit_ms),
            ),
            GitTimeoutSource::OuterDeadline => format!(
                "git {subcommand} timed out after {} (limit {}, outer command deadline){place}",
                Self::seconds(self.elapsed_ms),
                Self::seconds(self.limit_ms),
            ),
        }
    }

    /// The same fields for `--json` output, events and keep details.
    pub fn to_json(&self, args: &str) -> serde_json::Value {
        serde_json::json!({
            "source": self.source.as_str(),
            "started": self.started,
            "subcommand": self.subcommand,
            "args": args,
            "directory": self.directory,
            "limit_ms": self.limit_ms,
            "elapsed_ms": self.elapsed_ms,
        })
    }
}

fn display_git_timeout(args: &str, detail: &GitTimeoutDetail) -> String {
    detail.message(args)
}

/// The first argument that is not an option or an option's value
/// (`-c key=value`, `-C path`), which is the Git subcommand.
fn git_subcommand(args: &str) -> String {
    let mut words = args.split_whitespace();
    while let Some(word) = words.next() {
        if matches!(
            word,
            "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace"
        ) {
            words.next();
        } else if !word.starts_with('-') {
            return word.to_owned();
        }
    }
    String::new()
}

/// A directory for a message: its last two components, or `~` for a home
/// directory. Never an absolute path.
fn git_timeout_directory(directory: &Path) -> String {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if home.is_some_and(|home| Path::new(&home) == directory) {
        return "~".to_owned();
    }
    let names = directory
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    match names.as_slice() {
        [] => ".".to_owned(),
        [only] => only.clone(),
        [.., parent, leaf] => format!("{parent}/{leaf}"),
    }
}

impl GitError {
    fn timed_out(
        args: impl Into<String>,
        source: GitTimeoutSource,
        directory: Option<&Path>,
        limit: Duration,
        elapsed: Duration,
    ) -> Self {
        let args: String = args.into();
        let detail = GitTimeoutDetail {
            source,
            started: source != GitTimeoutSource::NotStarted,
            subcommand: git_subcommand(&args),
            directory: directory.map(git_timeout_directory),
            limit_ms: u64::try_from(limit.as_millis()).unwrap_or(u64::MAX),
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        };
        crate::phase_telemetry::add(source.counter(), Duration::ZERO);
        crate::phase_telemetry::attach("git_timeout_last", detail.to_json(&args));
        Self::TimedOut {
            seconds: limit.as_secs().max(1),
            args,
            detail: Box::new(detail),
        }
    }

    /// Git ran and was stopped (or finished past) `limit`. `outer` is true
    /// when the enclosing command's deadline, not the per-call limit, set it.
    pub(crate) fn timed_out_running(
        args: impl Into<String>,
        directory: Option<&Path>,
        started: Instant,
        outer: bool,
        limit: Duration,
    ) -> Self {
        let source = if outer {
            GitTimeoutSource::OuterDeadline
        } else {
            GitTimeoutSource::PerCall
        };
        Self::timed_out(args, source, directory, limit, started.elapsed())
    }

    /// The deadline had `remaining` left, so Git was never started.
    pub(crate) fn timed_out_before_start(
        args: impl Into<String>,
        directory: Option<&Path>,
        remaining: Duration,
    ) -> Self {
        Self::timed_out(
            args,
            GitTimeoutSource::NotStarted,
            directory,
            remaining,
            Duration::ZERO,
        )
    }

    /// The structured form of a timeout, for `--json`, events and keep details.
    pub fn timeout_json(&self) -> Option<serde_json::Value> {
        match self {
            Self::TimedOut { args, detail, .. } => Some(detail.to_json(args)),
            _ => None,
        }
    }
}

/// Whether the enclosing deadline, not a per-call limit of `per_call` from
/// `started`, is what bounds a command.
fn outer_deadline_bounds(started: Instant, per_call: Duration) -> bool {
    active_git_deadline().is_some_and(|outer| {
        started
            .checked_add(per_call)
            .is_none_or(|requested| outer <= requested)
    })
}

/// Whether `deadline` is the enclosing deadline itself.
fn deadline_is_outer(deadline: Instant) -> bool {
    active_git_deadline().is_some_and(|outer| outer <= deadline)
}

/// Overrides [`DEFAULT_GIT_TIMEOUT`], in whole seconds.
pub(crate) const GIT_TIMEOUT_ENV: &str = "AETHYME_GIT_TIMEOUT_SECS";

/// How long one broker git invocation may run before it is killed. Generous
/// on purpose: it exists to turn a hang into an error, not to police a slow
/// but progressing fetch.
const DEFAULT_GIT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Batch metadata and patch-id output must stay finite even for an unusually
/// large repository. Patch bodies themselves are streamed between Git.
const MAX_GIT_BATCH_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// Per-stream cap for private-index tree hashing, whose Git child may invoke
/// repository-configured clean filters.
#[cfg(unix)]
const MAX_WORKING_TREE_HASH_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_GIT_BATCH_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// A complete ignored-path census stays finite. Hitting any cap makes the
/// listing unknown; callers must not treat it as an empty or partial result.
const MAX_IGNORED_PATHS_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_IGNORED_PATHS_ENTRIES: usize = 16_384;
const MAX_IGNORED_PATH_BYTES: usize = 4_096;
/// A cleanup status snapshot must fit in bounded memory. The path-field count
/// includes the second NUL field emitted for a rename or copy.
pub(crate) const MAX_CLEANUP_STATUS_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_CLEANUP_STATUS_PATH_FIELDS: usize = 16_384;
/// Refuse recovery patches larger than this rather than buffering an
/// unbounded diff in memory. The archive source remains untouched on refusal.
const MAX_RECOVERY_PATCH_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
/// A single exact-tree landing search may ask for at most this many objects.
/// The same ceiling is enforced by `objects_at_many` before it allocates input.
pub(crate) const MAX_GIT_BATCH_QUERY_COUNT: usize = 100_000;

/// The deadline for one git invocation, from the value of
/// [`GIT_TIMEOUT_ENV`]. A missing, unparsable or zero value keeps the
/// default: a zero budget would fail every call, which nobody means.
fn git_timeout_from(value: Option<&str>) -> Duration {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_GIT_TIMEOUT)
}

pub(crate) fn git_timeout() -> Duration {
    let configured = git_timeout_from(std::env::var(GIT_TIMEOUT_ENV).ok().as_deref());
    GIT_DEADLINE.with(|deadline| match deadline.get() {
        Some(deadline) => deadline
            .saturating_duration_since(Instant::now())
            .min(configured)
            .max(Duration::from_millis(1)),
        None => configured,
    })
}

/// Parse `git status --porcelain=v1 -z` without interpreting quoted paths.
/// Git emits rename/copy destinations first and their source as the next NUL
/// field. Preserve the established destination-path contract and consume both
/// fields. Malformed output and non-UTF-8 paths refuse an ownership decision.
fn parse_porcelain_entries_z(out: &[u8]) -> Result<Vec<([u8; 2], String)>, GitError> {
    parse_porcelain_entries_z_with_path_limit(out, usize::MAX)
}

fn parse_porcelain_entries_z_with_path_limit(
    out: &[u8],
    max_path_fields: usize,
) -> Result<Vec<([u8; 2], String)>, GitError> {
    const CODES: &[u8] = b" MTADRCU?!";
    let invalid = || GitError::UntrustedOutput {
        detail: "malformed or non-UTF-8 NUL-separated porcelain status".into(),
    };
    let over_limit = || GitError::UntrustedOutput {
        detail: format!("git status returned more than {max_path_fields} path fields"),
    };
    if out.is_empty() {
        return Ok(Vec::new());
    }
    if out.last() != Some(&0) {
        return Err(invalid());
    }
    let mut fields = out[..out.len() - 1].split(|byte| *byte == 0);
    let mut entries = Vec::new();
    let mut path_fields = 0usize;
    while let Some(record) = fields.next() {
        if record.len() < 4
            || record[2] != b' '
            || !CODES.contains(&record[0])
            || !CODES.contains(&record[1])
            || record[..2] == *b"  "
        {
            return Err(invalid());
        }
        let path = std::str::from_utf8(&record[3..]).map_err(|_| invalid())?;
        if path_fields >= max_path_fields {
            return Err(over_limit());
        }
        path_fields += 1;
        let xy = [record[0], record[1]];
        if xy.iter().any(|code| matches!(code, b'R' | b'C')) {
            let source = fields
                .next()
                .filter(|source| !source.is_empty())
                .ok_or_else(invalid)?;
            std::str::from_utf8(source).map_err(|_| invalid())?;
            if path_fields >= max_path_fields {
                return Err(over_limit());
            }
            path_fields += 1;
        }
        entries.push((xy, path.to_owned()));
    }
    Ok(entries)
}

fn git_listing_invalid(args: &[&str]) -> GitError {
    GitError::UntrustedOutput {
        detail: format!(
            "git {} emitted malformed or non-UTF-8 NUL-separated entries",
            args.join(" ")
        ),
    }
}

fn strict_git_listing_records<'a>(out: &'a [u8], args: &[&str]) -> Result<Vec<&'a [u8]>, GitError> {
    let invalid = || git_listing_invalid(args);
    if out.is_empty() {
        return Ok(Vec::new());
    }
    if out.last() != Some(&0) {
        return Err(invalid());
    }
    let records = out[..out.len() - 1]
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>();
    if records.iter().any(|record| record.is_empty()) {
        return Err(invalid());
    }
    Ok(records)
}

fn valid_git_mode(field: &[u8]) -> bool {
    field.len() == 6 && field.iter().all(|byte| (b'0'..=b'7').contains(byte))
}

fn valid_tree_entry_mode_type(mode: &[u8], object_type: &[u8]) -> bool {
    valid_git_mode(mode)
        && match mode {
            b"040000" => object_type == b"tree",
            b"100644" | b"100755" | b"120000" => object_type == b"blob",
            b"160000" => object_type == b"commit",
            _ => false,
        }
}

fn valid_index_entry_mode(mode: &[u8]) -> bool {
    valid_git_mode(mode) && matches!(mode, b"100644" | b"100755" | b"120000" | b"160000")
}

fn valid_git_object_id(field: &[u8]) -> bool {
    matches!(field.len(), 40 | 64) && field.iter().all(u8::is_ascii_hexdigit)
}

fn parse_tree_gitlinks(out: &[u8], args: &[&str]) -> Result<Vec<String>, GitError> {
    let invalid = || git_listing_invalid(args);
    let mut paths = Vec::new();
    for record in strict_git_listing_records(out, args)? {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(invalid)?;
        let mut fields = record[..tab].split(|byte| *byte == b' ');
        let mode = fields.next().ok_or_else(invalid)?;
        let object_type = fields.next().ok_or_else(invalid)?;
        let object_id = fields.next().ok_or_else(invalid)?;
        if fields.next().is_some()
            || !valid_tree_entry_mode_type(mode, object_type)
            || !valid_git_object_id(object_id)
        {
            return Err(invalid());
        }
        let path = std::str::from_utf8(&record[tab + 1..]).map_err(|_| invalid())?;
        if path.is_empty() {
            return Err(invalid());
        }
        if mode == b"160000" {
            paths.push(path.to_owned());
        }
    }
    Ok(paths)
}

fn parse_index_gitlinks(out: &[u8], args: &[&str]) -> Result<Vec<String>, GitError> {
    let invalid = || git_listing_invalid(args);
    let mut paths = Vec::new();
    for record in strict_git_listing_records(out, args)? {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(invalid)?;
        let mut fields = record[..tab].split(|byte| *byte == b' ');
        let mode = fields.next().ok_or_else(invalid)?;
        let object_id = fields.next().ok_or_else(invalid)?;
        let stage = fields.next().ok_or_else(invalid)?;
        if fields.next().is_some()
            || !valid_index_entry_mode(mode)
            || !valid_git_object_id(object_id)
            || !matches!(stage, b"0" | b"1" | b"2" | b"3")
        {
            return Err(invalid());
        }
        let path = std::str::from_utf8(&record[tab + 1..]).map_err(|_| invalid())?;
        if path.is_empty() {
            return Err(invalid());
        }
        if mode == b"160000" {
            paths.push(path.to_owned());
        }
    }
    Ok(paths)
}

fn only_unstaged_tracked_deletions(
    entries: &[([u8; 2], String)],
    safe_ignored: impl Fn(&str) -> bool,
) -> bool {
    let has_deletion = entries.iter().any(|(xy, _)| *xy == [b' ', b'D']);
    has_deletion
        && entries
            .iter()
            .all(|(xy, path)| *xy == [b' ', b'D'] || (*xy == [b'!', b'!'] && safe_ignored(path)))
}

#[cfg(test)]
mod interrupted_finish_status_tests {
    use super::only_unstaged_tracked_deletions;

    #[test]
    fn interrupted_finish_accepts_only_unstaged_tracked_deletions() {
        let no_ignored = |_: &str| false;
        assert!(only_unstaged_tracked_deletions(
            &[(*b" D", "gone".into())],
            no_ignored
        ));
        assert!(!only_unstaged_tracked_deletions(&[], no_ignored));
        assert!(!only_unstaged_tracked_deletions(
            &[(*b"D ", "staged".into())],
            no_ignored
        ));
        assert!(!only_unstaged_tracked_deletions(
            &[(*b" M", "modified".into())],
            no_ignored
        ));
        assert!(!only_unstaged_tracked_deletions(
            &[(*b"??", "untracked".into())],
            no_ignored
        ));
        assert!(!only_unstaged_tracked_deletions(
            &[(*b"!!", "ignored".into())],
            |_| true
        ));
        assert!(!only_unstaged_tracked_deletions(
            &[(*b"UU", "conflict".into())],
            no_ignored
        ));
        assert!(!only_unstaged_tracked_deletions(
            &[(*b" D", "gone".into()), (*b"??", "untracked".into()),],
            no_ignored
        ));
        assert!(only_unstaged_tracked_deletions(
            &[
                (*b" D", "gone".into()),
                (*b"!!", ".aethyme/generated/experience-status.md".into()),
                (*b"!!", "target/".into()),
            ],
            |path| path.starts_with(".aethyme/generated/") || path == "target/"
        ));
        assert!(!only_unstaged_tracked_deletions(
            &[(*b" D", "gone".into()), (*b"!!", ".env".into()),],
            |path| path == "target/"
        ));
    }
}

/// Extract paths from newline-separated path listings (`git diff
/// --name-only`, `git diff-tree --name-only`, `git ls-files`), keeping
/// only lines that can actually be git-emitted paths — the same shim
/// hazard class as [`parse_porcelain_paths`] (#43). Observed in the
/// wild: a `--- Changes ---` section header decorating dirty diff
/// listings, which a naive per-line parse turns into a phantom implicit
/// lease that EVERY dirty session appears to hold — warning all session
/// pairs against each other.
///
/// Grammar: these listings never contain blank lines or diff-rendering
/// artifacts (`--- `/`+++ ` file headers, `diff --git`, `@@` hunks),
/// and git C-quotes any path containing bytes outside printable ASCII
/// (`core.quotePath` applies to path listings), so an unquoted line
/// with such bytes (the `ok ✓` decoration class) is not a path either.
/// Trade-off, same stance as #43: a repo file literally named like a
/// decoration line is dropped; with quoting disabled by hand, raw
/// non-ASCII paths are too. Both are vanishingly rare next to the
/// observed shim corruption.
fn parse_path_lines(out: &str) -> Vec<String> {
    out.lines()
        .filter(|line| {
            if line.is_empty()
                || *line == "---"
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line.starts_with("diff --git ")
                || line.starts_with("@@ ")
            {
                return false;
            }
            line.starts_with('"') || line.bytes().all(|b| (0x20..0x7f).contains(&b))
        })
        .map(str::to_string)
        .collect()
}

/// Extract paths from NUL-separated (`-z`) git output — the stronger
/// sibling of [`parse_path_lines`] for the implicit-lease surface. Real
/// paths never contain NUL or newline, so a chunk with an embedded
/// newline betrays a shim decoration glued onto a path and is dropped;
/// unlike the line grammar, an all-printable-ASCII decoration (e.g.
/// `3 files changed`) cannot slip through as a phantom path, and
/// non-ASCII paths arrive raw instead of C-quoted. Conservative by
/// design: a decoration can swallow the one path it was glued to, but
/// no decoration ever becomes a lease.
fn parse_nul_paths(out: &str) -> Vec<String> {
    out.split('\0')
        .filter(|chunk| !chunk.is_empty() && !chunk.contains('\n'))
        .map(str::to_string)
        .collect()
}

/// Decode raw `-z` path output for cleanup-authority checks. Unlike the
/// implicit-lease parser above, a newline is valid filename content here and
/// must be preserved. Malformed framing and non-UTF-8 names refuse the check.
fn parse_nul_paths_strict(out: &[u8], args: &[&str]) -> Result<Vec<String>, GitError> {
    let invalid = || GitError::UntrustedOutput {
        detail: format!(
            "git {} emitted malformed or non-UTF-8 NUL-separated paths",
            args.join(" ")
        ),
    };
    if out.is_empty() {
        return Ok(Vec::new());
    }
    if out.last() != Some(&0) {
        return Err(invalid());
    }

    out[..out.len() - 1]
        .split(|byte| *byte == 0)
        .map(|path| {
            if path.is_empty() {
                return Err(invalid());
            }
            std::str::from_utf8(path)
                .map(str::to_string)
                .map_err(|_| invalid())
        })
        .collect()
}

/// The `git` this process runs, and why it is trusted.
///
/// `Command::new("git")` resolves through the inherited PATH, which on a
/// developer machine is not a neutral place: wrapper scripts that decorate
/// output live there. One observed wrapper appends `ok \u{2713}` to every
/// command, so `git status --porcelain` returns six bytes on a clean
/// repository -- and every dirtiness predicate in the broker answers "dirty",
/// forever, for every checkout on the machine (#176).
///
/// [`parse_porcelain_paths`] already discards decoration a wrapper *adds*
/// (#43). It cannot see what a wrapper *removes*, and a suppressed status
/// line makes a dirty tree read clean -- which would authorize deleting work.
/// So rather than parse harder, find a `git` that does not need parsing
/// around: probe PATH candidates in order and keep the first whose porcelain
/// output on a known-empty repository is actually empty.
struct GitProgram {
    /// What to spawn. Always usable: when no candidate proved trustworthy
    /// this is plain `git`, because the majority of git calls here read refs
    /// and commit -- work that a decorating wrapper does not corrupt, and
    /// that a machine with such a wrapper still needs to do.
    program: PathBuf,
    trust: GitOutputTrust,
    /// The accepted candidate's verdict came from the per-user probe cache
    /// rather than from a probe this process ran. (Rejections ahead of it may
    /// also be cached; a cached rejection only ever skips a candidate.)
    verdict_from_cache: bool,
}

/// What this machine's `git` was proven to do with porcelain output.
///
/// Three states, not two, and the third is the point: a probe that could not
/// run proves nothing. Treating "unknown" as "untrustworthy" would refuse to
/// judge a checkout in any sandbox without a writable temporary directory --
/// an environment where nothing is actually wrong. Refusal is reserved for
/// evidence.
#[derive(Debug)]
pub enum GitOutputTrust {
    /// A candidate emitted exactly nothing on a known-empty repository.
    ///
    /// `bypassed` names the candidates ahead of it that were rejected. The
    /// broker is safe once it has an honest git, but the machine is not:
    /// gate commands run through `sh -c` with the caller's PATH, so a wrapper
    /// the broker routed around still breaks them. Carrying the list is what
    /// lets `certify` stay loud about a repaired-for-us-only machine -- and,
    /// since #177, what lets [`sanitized_subprocess_path`] keep the wrapper
    /// out of the gate's PATH instead of only reporting it.
    Undecorated {
        resolved: PathBuf,
        bypassed: Vec<BypassedGit>,
    },
    /// Every candidate that ran decorated its output. This is the wrapper.
    Decorated { detail: String },
    /// No candidate could be probed, so the question stays open.
    Indeterminate { detail: String },
}

/// A `git` on PATH that the probe did not select, and why.
///
/// Structured rather than pre-formatted because the two consumers need
/// different halves of it. `aethyme certify` prints `reason` to an operator.
/// [`sanitized_subprocess_path`] needs `path`, and needs it only for the
/// candidates flagged `dishonest`: a stale PATH entry that could not be
/// executed is not evidence of a wrapper, and removing its directory from a
/// subprocess PATH would take unrelated tools with it.
#[derive(Debug, Clone)]
pub struct BypassedGit {
    /// The rejected `git` itself, absolute.
    pub path: PathBuf,
    /// Operator-facing explanation, already naming `path`.
    pub reason: String,
    /// The probe caught this one rewriting porcelain output or exit codes,
    /// as opposed to merely failing to run.
    pub dishonest: bool,
}

impl std::fmt::Display for BypassedGit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

static GIT_PROGRAM: OnceLock<GitProgram> = OnceLock::new();

/// Set by commands that can discard work: their Git program is resolved by a
/// probe this process runs, never from the per-user cache.
static FRESH_GIT_PROBE_REQUIRED: AtomicBool = AtomicBool::new(false);

/// The outcome of re-proving a cached verdict, once per process.
static FRESH_GIT_CONFIRMATION: OnceLock<Result<(), String>> = OnceLock::new();

fn git_program() -> &'static GitProgram {
    GIT_PROGRAM.get_or_init(resolve_git_program)
}

/// Make this process resolve its Git program with a probe of its own rather
/// than the per-user cache. For commands that can delete or discard work
/// (`finish`, `close`, `cleanup`, `gc`, `reclaim`, `storage`, `worktrees` and
/// the detached GC worker), called before their first Git command. If Git was already
/// resolved from the cache, the next [`ensure_fresh_git_verdict`] re-proves
/// it, so calling this late is never weaker, only slower.
pub fn require_fresh_git_probe() {
    FRESH_GIT_PROBE_REQUIRED.store(true, Ordering::SeqCst);
}

/// Refuse unless the Git program this process uses was accepted by a probe
/// this process ran.
///
/// Every deletion primitive calls this -- the pinned tree removal
/// (`gc::tree_removal`), `removal::remove_tree`, `git worktree remove` and
/// branch-ref deletion -- as does `paths_equal` (the evidence for discarding a
/// superseded promotion), so a new deleter cannot forget it. It is a
/// no-op when the verdict was probed fresh (always, for the commands that call
/// [`require_fresh_git_probe`]); after a cache hit it re-runs the whole
/// resolution once per process, refreshes or drops the cache entry, and
/// refuses if the program in use is no longer the one an honest probe picks.
/// This is the line that keeps a wrapper whose behaviour changed without its
/// file changing from ever authorising a deletion through a cached verdict.
pub(crate) fn ensure_fresh_git_verdict() -> Result<(), GitError> {
    #[cfg(test)]
    if let Some(result) = test_cached_verdict::confirm() {
        return result.map_err(|detail| GitError::UntrustedOutput { detail });
    }
    let program = git_program();
    if !program.verdict_from_cache {
        return Ok(());
    }
    FRESH_GIT_CONFIRMATION
        .get_or_init(|| {
            crate::phase_telemetry::add("git_probe_fresh_forced", Duration::ZERO);
            let fresh = resolve_git_program_with(
                &path_candidates("git"),
                std::env::var_os("PATH").as_deref(),
                &crate::git_probe_cache::CacheLocation::for_process(),
                true,
                porcelain_probe,
            );
            confirm_cached_verdict(program, &fresh)
        })
        .clone()
        .map_err(|detail| GitError::UntrustedOutput { detail })
}

/// [`ensure_fresh_git_verdict`] for deletion primitives that speak `io`.
pub(crate) fn ensure_fresh_git_verdict_io() -> std::io::Result<()> {
    ensure_fresh_git_verdict().map_err(std::io::Error::other)
}

/// For loops that delete on Git evidence and report per-run failures: `true`
/// when the verdict is confirmed, else the refusal is recorded and nothing in
/// the loop may run.
pub(crate) fn fresh_git_verdict_or_report(failures: &mut Vec<String>) -> bool {
    match ensure_fresh_git_verdict() {
        Ok(()) => true,
        Err(error) => {
            failures.push(format!("nothing removed: {error}"));
            false
        }
    }
}

/// Test seam: make [`ensure_fresh_git_verdict`] on this thread behave as if the
/// process had resolved Git from the cache, against candidates and a cache file
/// the test controls. The process-wide cache is disabled under test, so without
/// this no test could observe the fresh confirmation running -- or prove that
/// removing a call to it breaks anything.
#[cfg(test)]
pub(crate) mod test_cached_verdict {
    use super::*;
    use std::cell::RefCell;

    struct Seam {
        candidates: Vec<PathBuf>,
        path_env: std::ffi::OsString,
        cache: crate::git_probe_cache::CacheLocation,
        confirmed: Option<Result<(), String>>,
        confirmations: usize,
    }

    thread_local! {
        static SEAM: RefCell<Option<Seam>> = const { RefCell::new(None) };
    }

    /// Run `body` with the seam installed; returns how many fresh
    /// confirmations ran (0 or 1: once per "process") alongside its result.
    pub(crate) fn with<T>(
        candidates: Vec<PathBuf>,
        path_env: &str,
        cache: crate::git_probe_cache::CacheLocation,
        body: impl FnOnce() -> T,
    ) -> (T, usize) {
        SEAM.with(|seam| {
            *seam.borrow_mut() = Some(Seam {
                candidates,
                path_env: path_env.into(),
                cache,
                confirmed: None,
                confirmations: 0,
            });
        });
        let result = body();
        let confirmations = SEAM.with(|seam| {
            seam.borrow_mut()
                .take()
                .map_or(0, |seam| seam.confirmations)
        });
        (result, confirmations)
    }

    pub(super) fn confirm() -> Option<Result<(), String>> {
        SEAM.with(|seam| {
            let mut guard = seam.borrow_mut();
            let seam = guard.as_mut()?;
            if let Some(confirmed) = &seam.confirmed {
                return Some(confirmed.clone());
            }
            let cached = resolve_git_program_with(
                &seam.candidates,
                Some(&seam.path_env),
                &seam.cache,
                false,
                porcelain_probe,
            );
            let result = if cached.verdict_from_cache {
                seam.confirmations += 1;
                let fresh = resolve_git_program_with(
                    &seam.candidates,
                    Some(&seam.path_env),
                    &seam.cache,
                    true,
                    porcelain_probe,
                );
                confirm_cached_verdict(&cached, &fresh)
            } else {
                Ok(())
            };
            seam.confirmed = Some(result.clone());
            Some(result)
        })
    }
}

/// `Ok` when a fresh resolution accepts the same program the cached one did.
fn confirm_cached_verdict(cached: &GitProgram, fresh: &GitProgram) -> Result<(), String> {
    match (&cached.trust, &fresh.trust) {
        (
            GitOutputTrust::Undecorated { resolved: was, .. },
            GitOutputTrust::Undecorated { resolved: now, .. },
        ) if was == now => Ok(()),
        (
            GitOutputTrust::Undecorated { resolved: was, .. },
            GitOutputTrust::Undecorated { resolved: now, .. },
        ) => Err(format!(
            "this process trusted {} from the git probe cache, but a fresh probe now selects {}; \
             refusing to discard work on the cached verdict, rerun the command",
            was.display(),
            now.display()
        )),
        (
            GitOutputTrust::Undecorated { resolved: was, .. },
            GitOutputTrust::Decorated { detail },
        )
        | (
            GitOutputTrust::Undecorated { resolved: was, .. },
            GitOutputTrust::Indeterminate { detail },
        ) => Err(format!(
            "this process trusted {} from the git probe cache, but a fresh probe no longer \
             accepts it ({detail}); refusing to discard work on the cached verdict",
            was.display()
        )),
        // A cached verdict is only ever an acceptance.
        _ => Ok(()),
    }
}

/// What `aethyme certify` reports about git output, from the same probe the
/// dirtiness predicates consult -- so the diagnostic and the refusal can
/// never disagree about the same machine.
pub fn git_output_trust() -> &'static GitOutputTrust {
    &git_program().trust
}

/// A [`Command`] for the `git` this process proved, rather than the one the
/// ambient PATH happens to offer.
///
/// Every direct spawn in this crate goes through here, including the ones
/// whose subcommand no known wrapper touches. Routing only the sites that a
/// wrapper is observed to intercept would be a list to maintain against
/// wrappers nobody has written yet, and #178 is what that list costs when it
/// falls behind: three call sites kept `Command::new("git")` after #176
/// resolved an honest binary, and one of them authorized discarding work.
/// The probe already answers "which binary"; the cheap invariant is that no
/// call site answers it again.
pub(crate) fn git_command() -> Command {
    let mut command = Command::new(&git_program().program);
    if graph_verification_git_enabled() {
        configure_isolated_graph_git_command(&mut command);
    } else if DROP_INHERITED_GIT_ENVIRONMENT.with(std::cell::Cell::get) {
        remove_git_environment(&mut command);
    }
    command
}

thread_local! {
    static DROP_INHERITED_GIT_ENVIRONMENT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Every Git command this thread runs ignores `GIT_*` from the process
/// environment. For a thread reading another checkout: Git hands a hook its own
/// `GIT_DIR` and `GIT_INDEX_FILE`, which would answer for the committing worktree
/// whatever directory the command runs in. Only for a thread that ends with its
/// read, because nothing restores it.
pub(crate) fn drop_inherited_git_environment_on_this_thread() {
    DROP_INHERITED_GIT_ENVIRONMENT.with(|flag| flag.set(true));
}

fn resolve_git_program() -> GitProgram {
    let _span = crate::phase_telemetry::span("git_program_resolve");
    let fresh = FRESH_GIT_PROBE_REQUIRED.load(Ordering::SeqCst);
    if fresh {
        crate::phase_telemetry::add("git_probe_fresh_forced", Duration::ZERO);
    }
    resolve_git_program_with(
        &path_candidates("git"),
        std::env::var_os("PATH").as_deref(),
        &crate::git_probe_cache::CacheLocation::for_process(),
        fresh,
        porcelain_probe,
    )
}

/// Pick the first candidate that is not a lying wrapper.
///
/// Unless `fresh`, a candidate whose exact key has an unexpired entry in
/// `cache` is decided without probing: a cached acceptance selects it, a
/// cached rejection skips it exactly as a fresh rejection would (fail safe:
/// a skipped candidate is never used). Every other candidate is probed, and
/// the verdict is recorded -- acceptance or rejection -- only if the key did
/// not change while the probe ran. A probe that could not run (`Unusable`)
/// records nothing and drops whatever the cache held for that candidate.
fn resolve_git_program_with(
    candidates: &[PathBuf],
    path_env: Option<&std::ffi::OsStr>,
    cache: &crate::git_probe_cache::CacheLocation,
    fresh: bool,
    probe: impl Fn(&Path) -> PorcelainProbe,
) -> GitProgram {
    use crate::git_probe_cache::{self as probe_cache, CachedVerdict};

    let mut dishonest: Vec<BypassedGit> = Vec::new();
    let mut unusable: Vec<BypassedGit> = Vec::new();
    for candidate in candidates.iter().cloned() {
        let key = probe_cache::program_key(&candidate, path_env);
        if !fresh {
            let lookup = match &key {
                Some(key) => probe_cache::lookup(cache, key, std::time::SystemTime::now()),
                None => probe_cache::Lookup::Miss(probe_cache::MissReason::Unkeyable),
            };
            match lookup {
                probe_cache::Lookup::Hit(CachedVerdict::Accepted) => {
                    crate::phase_telemetry::add("git_probe_cache_hit", Duration::ZERO);
                    let mut bypassed = dishonest;
                    bypassed.append(&mut unusable);
                    return GitProgram {
                        program: candidate.clone(),
                        trust: GitOutputTrust::Undecorated {
                            resolved: candidate,
                            bypassed,
                        },
                        verdict_from_cache: true,
                    };
                }
                probe_cache::Lookup::Hit(CachedVerdict::Rejected { reason }) => {
                    crate::phase_telemetry::add("git_probe_cache_hit_rejected", Duration::ZERO);
                    dishonest.push(BypassedGit {
                        reason: format!("{reason} (cached verdict)"),
                        path: candidate,
                        dishonest: true,
                    });
                    continue;
                }
                probe_cache::Lookup::Miss(reason) => {
                    crate::phase_telemetry::add(reason.counter(), Duration::ZERO);
                }
            }
        }
        let verdict = {
            let _span = crate::phase_telemetry::span("git_probe_run");
            probe(&candidate)
        };
        // Record only a verdict about the file that was actually probed.
        let recordable = key
            .clone()
            .filter(|key| probe_cache::program_key(&candidate, path_env).as_ref() == Some(key));
        let rejection = match verdict {
            PorcelainProbe::Undecorated => {
                if let Some(key) = &recordable {
                    probe_cache::record(
                        cache,
                        key,
                        CachedVerdict::Accepted,
                        std::time::SystemTime::now(),
                    );
                }
                let mut bypassed = dishonest;
                bypassed.append(&mut unusable);
                return GitProgram {
                    program: candidate.clone(),
                    trust: GitOutputTrust::Undecorated {
                        resolved: candidate,
                        bypassed,
                    },
                    verdict_from_cache: false,
                };
            }
            PorcelainProbe::Decorated { bytes } => format!(
                "{} emitted {bytes} bytes of porcelain on a clean temporary repository",
                candidate.display()
            ),
            PorcelainProbe::StatusRewritten { detail } => {
                format!("{}: {detail}", candidate.display())
            }
            // PATH routinely holds stale entries, and one that cannot run is
            // not evidence of a wrapper. Keep looking, and remember nothing:
            // a fixed environment recovers on the next process.
            PorcelainProbe::Unusable { reason } => {
                probe_cache::forget(cache, &candidate);
                unusable.push(BypassedGit {
                    reason: format!("{}: {reason}", candidate.display()),
                    path: candidate,
                    dishonest: false,
                });
                continue;
            }
        };
        if let Some(key) = &recordable {
            probe_cache::record(
                cache,
                key,
                CachedVerdict::Rejected {
                    reason: rejection.clone(),
                },
                std::time::SystemTime::now(),
            );
        }
        dishonest.push(BypassedGit {
            reason: rejection,
            path: candidate,
            dishonest: true,
        });
    }
    let trust = if !dishonest.is_empty() {
        GitOutputTrust::Decorated {
            detail: format!(
                "a PATH wrapper is rewriting git output or exit codes ({}); \
                 remove it from PATH, then rerun",
                join_reasons(&dishonest)
            ),
        }
    } else if !unusable.is_empty() {
        GitOutputTrust::Indeterminate {
            detail: format!(
                "no git on PATH could be probed ({})",
                join_reasons(&unusable)
            ),
        }
    } else {
        GitOutputTrust::Indeterminate {
            detail: "git was not found on PATH".to_string(),
        }
    };
    GitProgram {
        program: PathBuf::from("git"),
        trust,
        verdict_from_cache: false,
    }
}

/// The operator-facing half of a set of rejected candidates, in probe order.
pub fn join_reasons(bypassed: &[BypassedGit]) -> String {
    bypassed
        .iter()
        .map(BypassedGit::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// PATH for a subprocess the broker spawns on an operator's behalf, with the
/// directory of every `git` the probe caught lying removed.
///
/// [`git_command`] protects this crate's own git calls by resolving one
/// trustworthy binary (#176, #178). It does nothing for gate commands, which
/// run through `sh -c` and resolve `git` -- and `cargo`, and everything else
/// -- through whatever PATH the operator happened to have. A wrapper the
/// broker routed around still corrupts them.
///
/// That matters more than it looks, because of where the verdict lands. A gate
/// whose tools were replaced by a wrapper does not fail as an environment
/// error; it fails like a test failure, and `fail` / `test_failure` is the one
/// combination the tree-hash cache reuses. So the gate inherits a wrong answer
/// and then remembers it against the tree (#177).
///
/// Removing a directory is deliberately blunt: if it held the only copy of a
/// tool the gate needs, the gate now exits 127 and classifies as an
/// environment error, which is loud and is never cached. The failure mode of
/// this repair is the honest one.
///
/// `None` means leave PATH exactly as inherited -- either nothing was proven,
/// or nothing proven needs removing. An untouched PATH is never rebuilt from a
/// probe that concluded nothing.
pub fn sanitized_subprocess_path() -> Option<std::ffi::OsString> {
    let GitOutputTrust::Undecorated { bypassed, .. } = &git_program().trust else {
        return None;
    };
    let remove: std::collections::BTreeSet<PathBuf> = bypassed
        .iter()
        .filter(|entry| entry.dishonest)
        .filter_map(|entry| entry.path.parent().map(Path::to_path_buf))
        .collect();
    if remove.is_empty() {
        return None;
    }
    path_without(&std::env::var_os("PATH")?, &remove)
}

/// `inherited` with every directory in `remove` dropped, preserving order.
///
/// Split out from [`sanitized_subprocess_path`] so the filtering is testable
/// against a constructed PATH: the caller's half depends on a `OnceLock` probe
/// of the real machine, which is exactly the thing a test cannot arrange.
fn path_without(
    inherited: &std::ffi::OsStr,
    remove: &std::collections::BTreeSet<PathBuf>,
) -> Option<std::ffi::OsString> {
    let kept: Vec<PathBuf> = std::env::split_paths(inherited)
        .filter(|dir| !absolute_path_dir(dir).is_some_and(|dir| remove.contains(&dir)))
        .collect();
    std::env::join_paths(kept).ok()
}

/// Marks the line [`subprocess_path_note`] writes into a subprocess log.
///
/// Public because the line is aethyme's, not the command's: anything replaying
/// a gate log back to an operator has to be able to tell them apart, or
/// aethyme's own header gets reported as output the gate produced.
pub const SUBPROCESS_PATH_NOTE_PREFIX: &str = "aethyme gate environment: ";

/// What [`sanitized_subprocess_path`] did, for the subprocess's own log.
///
/// Always a line, including when nothing was changed. A gate log that says
/// which git the command will resolve is how a wrapper-corrupted verdict stops
/// being indistinguishable from a real one after the fact; saying nothing when
/// nothing was removed would leave exactly the ambiguity this is for.
pub fn subprocess_path_note() -> String {
    match &git_program().trust {
        GitOutputTrust::Undecorated { resolved, bypassed } => {
            let removed: Vec<String> = bypassed
                .iter()
                .filter(|entry| entry.dishonest)
                .filter_map(|entry| entry.path.parent())
                .map(|dir| dir.display().to_string())
                .collect();
            if removed.is_empty() {
                format!(
                    "aethyme gate environment: git resolves to {}; PATH inherited unchanged\n",
                    resolved.display()
                )
            } else {
                format!(
                    "{SUBPROCESS_PATH_NOTE_PREFIX}git resolves to {}; removed {} from PATH because a git there rewrites output ({})\n",
                    resolved.display(),
                    removed.join(", "),
                    join_reasons(bypassed)
                )
            }
        }
        GitOutputTrust::Decorated { detail } => format!(
            "{SUBPROCESS_PATH_NOTE_PREFIX}WARNING every git on PATH rewrites output, so this gate's tools are not trustworthy and its verdict may not be about the code ({detail})\n"
        ),
        GitOutputTrust::Indeterminate { detail } => format!(
            "{SUBPROCESS_PATH_NOTE_PREFIX}git trust unproven ({detail}); PATH inherited unchanged\n"
        ),
    }
}

/// A PATH entry as an absolute directory, or `None` when it cannot be one.
///
/// Shared with [`path_candidates`] so the sanitizer compares the same
/// directories the probe walked. Resolving these differently is how a removal
/// silently misses the entry it was built to remove.
fn absolute_path_dir(dir: &Path) -> Option<PathBuf> {
    if dir.is_absolute() {
        Some(dir.to_path_buf())
    } else {
        Some(std::env::current_dir().ok()?.join(dir))
    }
}

/// Refuse rather than guess -- but only on proof.
///
/// Called only by the dirtiness predicates, which is deliberate: those are
/// the answers that authorize removing a worktree or refuse to publish one,
/// and a wrong answer there costs work rather than bytes. Every other git
/// call stays available on a wrapped machine, because making the whole broker
/// unusable would not protect anything -- it would only remove the tool the
/// operator needs to recover.
fn require_trustworthy_porcelain() -> Result<(), GitError> {
    match &git_program().trust {
        GitOutputTrust::Undecorated { .. } | GitOutputTrust::Indeterminate { .. } => Ok(()),
        GitOutputTrust::Decorated { detail } => Err(GitError::UntrustedOutput {
            detail: detail.clone(),
        }),
    }
}

/// Executable files named `program` on PATH, in PATH order.
fn path_candidates(program: &str) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .filter_map(|dir| {
            let candidate = absolute_path_dir(&dir)?.join(program);
            is_executable_file(&candidate).then_some(candidate)
        })
        .collect()
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn remove_git_environment(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if is_git_environment_key(&key) {
            command.env_remove(key);
        }
    }
}

/// What one candidate did when asked for porcelain it could not embellish.
#[derive(Debug, PartialEq, Eq)]
enum PorcelainProbe {
    Undecorated,
    Decorated {
        bytes: usize,
    },
    /// Said the right bytes and the wrong thing: an exit code that does not
    /// match the repository state the probe built. Separate from
    /// [`PorcelainProbe::Decorated`] only so the rejection can say which
    /// kind of dishonesty it found; both refuse the candidate.
    StatusRewritten {
        detail: String,
    },
    Unusable {
        reason: String,
    },
}

/// Run `program` against a freshly created empty repository and require
/// `git status --porcelain` to say nothing at all.
///
/// The repository is built with an empty template and an empty global config
/// so the answer describes the binary rather than the machine's hooks or
/// aliases: a `status.showUntrackedFiles` setting or a template hook that
/// prints would otherwise be indistinguishable from a wrapper.
fn porcelain_probe(program: &Path) -> PorcelainProbe {
    // A recognisable name: a process killed mid-probe (or one that replaces
    // itself) leaves this directory behind, and the background worker removes
    // stale ones by this prefix and shape (`git_probe_cache`).
    let probe = match tempfile::Builder::new()
        .prefix(crate::git_probe_cache::PROBE_DIRECTORY_PREFIX)
        .tempdir()
    {
        Ok(probe) => probe,
        Err(err) => {
            return PorcelainProbe::Unusable {
                reason: format!("could not create a probe directory ({err})"),
            };
        }
    };
    let template = probe.path().join("empty-template");
    let repo = probe.path().join("repo");
    let config = probe.path().join("empty-gitconfig");
    if let Err(err) = std::fs::create_dir(&template)
        .and_then(|()| std::fs::create_dir(&repo))
        .and_then(|()| std::fs::write(&config, []))
    {
        return PorcelainProbe::Unusable {
            reason: format!("could not prepare the probe repository ({err})"),
        };
    }

    let isolated = |program: &Path| {
        let mut command = Command::new(program);
        command.current_dir(&repo);
        remove_git_environment(&mut command);
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("GIT_AUTHOR_NAME", "aethyme probe")
            .env("GIT_AUTHOR_EMAIL", "probe@aethyme.invalid")
            .env("GIT_COMMITTER_NAME", "aethyme probe")
            .env("GIT_COMMITTER_EMAIL", "probe@aethyme.invalid");
        command
    };

    match isolated(program)
        .args(["init", "-q"])
        .arg(format!("--template={}", template.display()))
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            return PorcelainProbe::Unusable {
                reason: format!("git init exited {}", output.status),
            };
        }
        Err(err) => {
            return PorcelainProbe::Unusable {
                reason: format!("could not execute it ({err})"),
            };
        }
    }

    // `-z` so the answer cannot be a trailing newline argument: a clean
    // repository's NUL-separated porcelain is exactly zero bytes.
    match isolated(program)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
    {
        Ok(output) if !output.status.success() => {
            return PorcelainProbe::Unusable {
                reason: format!("git status exited {}", output.status),
            };
        }
        Ok(output) if output.stdout.is_empty() => {}
        Ok(output) => {
            return PorcelainProbe::Decorated {
                bytes: output.stdout.len(),
            };
        }
        Err(err) => {
            return PorcelainProbe::Unusable {
                reason: format!("could not execute git status ({err})"),
            };
        }
    }

    // Everything above proves a property of stdout, and an exit code is not
    // stdout. The wrapper behind #178 returns 0 from `git diff --quiet` where
    // real git returns 1 -- emitting no bytes at all, so the emptiness check
    // above sees a perfectly honest binary. `paths_equal` read that status and
    // reported differing content as identical, which is the sole automatic
    // evidence an unattended cleanup uses to discard local work.
    //
    // So ask the same binary a question whose answer this function already
    // knows, in both directions: a committed file that matches, then the same
    // file modified. A candidate that cannot tell those apart is refused for
    // the same reason a decorating one is.
    let tracked = repo.join("probe.txt");
    if std::fs::write(&tracked, "before\n").is_err() {
        return PorcelainProbe::Unusable {
            reason: "could not write the probe file".to_string(),
        };
    }
    for args in [
        &["add", "probe.txt"][..],
        &["commit", "-q", "-m", "probe"][..],
    ] {
        match isolated(program).args(args).output() {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                return PorcelainProbe::Unusable {
                    reason: format!("git {} exited {}", args.join(" "), output.status),
                };
            }
            Err(err) => {
                return PorcelainProbe::Unusable {
                    reason: format!("could not execute git {} ({err})", args.join(" ")),
                };
            }
        }
    }

    // `--` so a file named like a revision cannot change what is compared.
    //
    // `output()` rather than `status()`, which every other probe command here
    // already uses. `status()` hands the child this process's own stdout, so a
    // wrapper that gets the exit codes right but prints while doing it writes
    // its banner straight into the broker's stdout -- and the first
    // `aethyme ... --json` of the run then emits those bytes ahead of its
    // JSON. The probe is the first thing any command does, so it is the worst
    // possible place to inherit a stream.
    //
    // Capturing also makes the check stricter for free: a `--quiet` that
    // prints anything is not quiet, whatever it exits.
    // `Err` carries the verdict rather than a string: printing on `--quiet` is
    // decoration and marks the candidate dishonest, while failing to spawn is
    // merely unusable, and only the first makes the dirtiness predicates
    // refuse the machine. Collapsing them into one message would file a lying
    // wrapper as a broken one.
    let quiet_exit = |program: &Path| match isolated(program)
        .args(["diff", "--quiet", "--", "probe.txt"])
        .output()
    {
        Ok(output) if !output.stdout.is_empty() => Err(PorcelainProbe::Decorated {
            bytes: output.stdout.len(),
        }),
        Ok(output) => output
            .status
            .code()
            .ok_or_else(|| PorcelainProbe::Unusable {
                reason: format!(
                    "git diff --quiet was terminated without an exit code ({})",
                    output.status
                ),
            }),
        Err(err) => Err(PorcelainProbe::Unusable {
            reason: format!("could not execute git diff --quiet ({err})"),
        }),
    };

    match quiet_exit(program) {
        Ok(0) => {}
        Ok(code) => {
            return PorcelainProbe::StatusRewritten {
                detail: format!(
                    "git diff --quiet exited {code} on an unmodified tracked file, \
                     where git exits 0"
                ),
            };
        }
        Err(verdict) => return verdict,
    }

    if std::fs::write(&tracked, "after\n").is_err() {
        return PorcelainProbe::Unusable {
            reason: "could not modify the probe file".to_string(),
        };
    }
    match quiet_exit(program) {
        Ok(1) => {}
        // The dangerous direction, and the reason this check exists: 0 here
        // means "identical", for a file this function just changed.
        Ok(code) => {
            return PorcelainProbe::StatusRewritten {
                detail: format!(
                    "git diff --quiet exited {code} on a modified tracked file, \
                     where git exits 1"
                ),
            };
        }
        Err(verdict) => return verdict,
    }

    // Everything above proves a property of `status` and of exit codes.
    // `paths_equal` reads neither: it reads the stdout of
    // `git diff --name-only -z <left> <right> -- <paths>`, and treats empty
    // output as proof that the two commits agree. A wrapper that answers
    // `status` honestly, exits correctly from `diff --quiet`, and discards
    // stdout for `diff --name-only` alone passes every check above, is filed
    // `Undecorated`, and then makes `paths_equal` report differing commits as
    // identical -- the sole automatic evidence for classifying a promotion
    // `SupersededUpstream`, on the strength of which an unattended cleanup
    // discards local work.
    //
    // So ask this binary the exact question, in the shape the caller parses,
    // with an answer this function already knows. The probe file is modified
    // on disk; commit it and the two commits differ in exactly one path.
    for args in [
        &["add", "probe.txt"][..],
        &["commit", "-q", "-m", "probe-changed"][..],
    ] {
        match isolated(program).args(args).output() {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                return PorcelainProbe::Unusable {
                    reason: format!("git {} exited {}", args.join(" "), output.status),
                };
            }
            Err(err) => {
                return PorcelainProbe::Unusable {
                    reason: format!("could not execute git {} ({err})", args.join(" ")),
                };
            }
        }
    }

    let name_only = |program: &Path, left: &str, right: &str| match isolated(program)
        .args(["diff", "--name-only", "-z", left, right, "--", "probe.txt"])
        .output()
    {
        Ok(output) if !output.status.success() => Err(PorcelainProbe::Unusable {
            reason: format!(
                "git diff --name-only exited {} comparing {left} and {right}",
                output.status
            ),
        }),
        Ok(output) => Ok(output.stdout),
        Err(err) => Err(PorcelainProbe::Unusable {
            reason: format!("could not execute git diff --name-only ({err})"),
        }),
    };

    // The load-bearing direction: two commits that differ in `probe.txt` and
    // nothing else. Real git answers with exactly one NUL-terminated path.
    match name_only(program, "HEAD~1", "HEAD") {
        Ok(out) if out == b"probe.txt\0" => {}
        Ok(out) => {
            return PorcelainProbe::StatusRewritten {
                detail: format!(
                    "git diff --name-only -z reported {:?} for two commits differing \
                     only in probe.txt, where git reports \"probe.txt\\0\"; a wrapper \
                     that empties this output makes differing commits look identical",
                    String::from_utf8_lossy(&out)
                ),
            };
        }
        Err(verdict) => return verdict,
    }

    // And the direction whose emptiness is trusted, so that "empty" has been
    // observed to mean "equal" for this binary rather than assumed to.
    match name_only(program, "HEAD", "HEAD") {
        Ok(out) if out.is_empty() => PorcelainProbe::Undecorated,
        Ok(out) => PorcelainProbe::Decorated { bytes: out.len() },
        Err(verdict) => verdict,
    }
}

/// A Git operation a worktree is part-way through, and whether its index
/// still holds unresolved conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InProgressOperation {
    /// `merge`, `rebase`, `cherry-pick` or `revert`; `None` when only the
    /// index records unmerged entries (a conflicted `stash pop`, say).
    pub operation: Option<&'static str>,
    pub unresolved_conflicts: bool,
}

impl InProgressOperation {
    /// "session 7 is mid-merge with unresolved conflicts", and the like.
    pub fn describe(self, session_id: i64) -> String {
        match (self.operation, self.unresolved_conflicts) {
            (Some(operation), true) => {
                format!("session {session_id} is mid-{operation} with unresolved conflicts")
            }
            (Some(operation), false) => format!("session {session_id} is mid-{operation}"),
            (None, _) => format!("session {session_id} has unresolved conflicts"),
        }
    }
}

/// The operation `worktree` is in the middle of, read from its Git directory
/// without running Git, so `status` can check every session without forking.
pub fn worktree_operation_in_progress(worktree: &Path) -> Option<&'static str> {
    let git_dir = worktree_git_dir(worktree)?;
    [
        ("MERGE_HEAD", "merge"),
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
        ("REVERT_HEAD", "revert"),
    ]
    .into_iter()
    .find(|(marker, _)| git_dir.join(marker).exists())
    .map(|(_, operation)| operation)
}

/// A worktree's own Git directory: `.git` itself, or the directory a linked
/// worktree's `.git` file points to.
fn worktree_git_dir(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    if std::fs::symlink_metadata(&dot_git).ok()?.is_dir() {
        return Some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = PathBuf::from(
        text.lines()
            .find_map(|line| line.strip_prefix("gitdir:"))?
            .trim(),
    );
    Some(if target.is_absolute() {
        target
    } else {
        worktree.join(target)
    })
}

/// Resolve a checkout's `.git` directory without invoking Git. A valid `.git`
/// directory or gitdir file is canonicalized; missing targets are reported as
/// known absence, while malformed or unreadable metadata remains an error.
pub(crate) fn checked_worktree_git_dir(worktree: &Path) -> Result<Option<PathBuf>, std::io::Error> {
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

/// Refuse ambient repository/path/history selectors before graph verification
/// can snapshot anything, unless its isolating scope is already active.
/// Scoped Git commands strip every inherited `GIT_*` variable, so harmless
/// configuration, pager, and trace settings can be accepted without affecting
/// the boundary. Diagnostics deliberately contain no environment values.
pub(crate) fn graph_verification_environment_refusal() -> Option<&'static str> {
    if graph_verification_git_enabled() {
        return None;
    }
    std::env::vars_os()
        .any(|(key, _)| unsafe_graph_git_environment_key(&key))
        .then_some("graph verification refuses inherited Git routing/history overrides")
}

fn is_git_environment_key(key: &std::ffi::OsStr) -> bool {
    key.to_string_lossy()
        .to_ascii_uppercase()
        .starts_with("GIT_")
}

fn unsafe_graph_git_environment_key(key: &std::ffi::OsStr) -> bool {
    matches!(
        key.to_string_lossy().to_ascii_uppercase().as_str(),
        "GIT_DIR"
            | "GIT_WORK_TREE"
            | "GIT_COMMON_DIR"
            | "GIT_INDEX_FILE"
            | "GIT_OBJECT_DIRECTORY"
            | "GIT_OBJECT_DIRECTORY_RELATIVE"
            | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
            | "GIT_NAMESPACE"
            | "GIT_REPLACE_REF_BASE"
            | "GIT_GRAFT_FILE"
            | "GIT_SHALLOW_FILE"
            | "GIT_CEILING_DIRECTORIES"
            | "GIT_DISCOVERY_ACROSS_FILESYSTEM"
            | "GIT_PREFIX"
    )
}

/// Read-only discovery can honor alternate global or system configuration.
/// Inherited routing and inline configuration can redirect the checkout.
pub(crate) fn repository_routing_environment_overridden() -> bool {
    std::env::vars_os().any(|(key, _)| {
        unsafe_graph_git_environment_key(&key)
            || matches!(
                key.to_str(),
                Some("GIT_CONFIG_COUNT" | "GIT_CONFIG_PARAMETERS")
            )
    })
}

fn unsafe_session_commit_git_environment_key(key: &std::ffi::OsStr) -> bool {
    let normalized = key.to_string_lossy().to_ascii_uppercase();
    unsafe_graph_git_environment_key(std::ffi::OsStr::new(&normalized))
        || normalized == "GIT_EXEC_PATH"
        || normalized == "GIT_CONFIG"
        || normalized.starts_with("GIT_CONFIG_")
}

/// Refuse commit commands when inherited Git environment can redirect
/// repository discovery, history, the index, or Git configuration. This is
/// checked before CLI broker discovery as well as immediately before commit.
pub(crate) fn session_commit_git_environment_refusal() -> Option<&'static str> {
    std::env::vars_os()
        .any(|(key, _)| unsafe_session_commit_git_environment_key(&key))
        .then_some(
            "broker commit refuses inherited Git routing or configuration overrides; unset them before retrying",
        )
}

fn graph_snapshot_error(message: &str) -> GitError {
    GitError::Spawn {
        args: "isolate exact graph source".into(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, message),
    }
}

fn graph_snapshot_object_format(commit: &str) -> Result<&'static str, GitError> {
    match commit.len() {
        40 if commit.bytes().all(|byte| byte.is_ascii_hexdigit()) => Ok("sha1"),
        64 if commit.bytes().all(|byte| byte.is_ascii_hexdigit()) => Ok("sha256"),
        _ => Err(graph_snapshot_error(
            "graph snapshot requires an exact commit object ID",
        )),
    }
}

fn isolated_graph_git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(&git_program().program);
    configure_isolated_graph_git_command(&mut command);
    command.current_dir(cwd).args(args);
    command
}

/// Add the command-line and environment fences used for private graph
/// snapshots. This does not mutate the broker process environment.
fn configure_isolated_graph_git_command(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if is_git_environment_key(&key) {
            command.env_remove(key);
        }
    }
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let hooks_path = format!("core.hooksPath={null_device}");
    command
        .args([
            "--no-replace-objects",
            "-c",
            hooks_path.as_str(),
            "-c",
            "core.fsmonitor=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "submodule.recurse=false",
            "-c",
            "core.autocrlf=false",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", null_device)
        .env("GIT_CONFIG_GLOBAL", null_device)
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env(
            "GIT_GRAFT_FILE",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        );
}

fn run_isolated_graph_git(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let mut command = isolated_graph_git_command(cwd, args);
    let budget = active_git_deadline().map_or_else(git_timeout, |deadline| {
        git_timeout().min(deadline.saturating_duration_since(Instant::now()))
    });
    let started = Instant::now();
    match crate::bounded_output::output_limited(&mut command, budget, 16 * 1024 * 1024).map_err(
        |source| GitError::Spawn {
            args: args.join(" "),
            source,
        },
    )? {
        crate::bounded_output::LimitedOutput::Completed(output) => {
            if !output.status.success() {
                return Err(GitError::Git {
                    args: args.join(" "),
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                });
            }
            Ok(output.stdout)
        }
        crate::bounded_output::LimitedOutput::Deadline => Err(GitError::timed_out_running(
            args.join(" "),
            Some(cwd),
            started,
            outer_deadline_bounds(started, git_timeout()),
            budget,
        )),
        crate::bounded_output::LimitedOutput::OutputLimit => Err(graph_snapshot_error(
            "graph snapshot Git output exceeds 16 MiB",
        )),
    }
}

fn parse_graph_snapshot_inventory(bytes: &[u8]) -> Result<(u64, u64), GitError> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(graph_snapshot_error("incomplete graph snapshot inventory"));
    }
    let mut logical_bytes = 0u64;
    let mut allocated_bytes = 0u64;
    let mut inodes = 1u64;
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| graph_snapshot_error("invalid graph snapshot inventory row"))?;
        if tab + 1 == record.len() {
            return Err(graph_snapshot_error(
                "graph snapshot inventory has an empty path",
            ));
        }
        let header = std::str::from_utf8(&record[..tab])
            .map_err(|_| graph_snapshot_error("invalid graph snapshot inventory header"))?;
        let fields = header.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() != 4
            || !matches!(fields[2].len(), 40 | 64)
            || !fields[2].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(graph_snapshot_error("invalid graph snapshot object header"));
        }
        let size = match (fields[0], fields[1], fields[3]) {
            ("040000", "tree", "-") => 0,
            ("100644" | "100755" | "120000", "blob", size) => size
                .parse::<u64>()
                .map_err(|_| graph_snapshot_error("unknown graph snapshot blob size"))?,
            _ => {
                return Err(graph_snapshot_error(
                    "unsupported graph snapshot tree entry",
                ));
            }
        };
        logical_bytes = logical_bytes
            .checked_add(size)
            .ok_or_else(|| graph_snapshot_error("graph snapshot size overflow"))?;
        if logical_bytes > 2 * 1024 * 1024 * 1024 || inodes >= 100_000 {
            return Err(graph_snapshot_error(
                "graph snapshot exceeds the 2 GiB or 100000 entry limit",
            ));
        }
        let allocation = size
            .div_ceil(4096)
            .max(1)
            .checked_mul(4096)
            .ok_or_else(|| graph_snapshot_error("graph snapshot allocation overflow"))?;
        allocated_bytes = allocated_bytes
            .checked_add(allocation)
            .ok_or_else(|| graph_snapshot_error("graph snapshot allocation overflow"))?;
        inodes += 1;
    }
    Ok((allocated_bytes, inodes))
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String, GitError> {
    if active_git_deadline().is_some() || GRAPH_VERIFICATION_GIT.with(std::cell::Cell::get) {
        return run_git_inner(cwd, None, args);
    }
    crate::status_reads::git_identity(cwd, args, || run_git_inner(cwd, None, args))
}

/// Run Git within `budget` alone: unlike [`run_git`], an enclosing command
/// deadline does not shorten it. For the primary-checkout move, which must
/// not be cut off between its steps by a short inspection budget.
fn run_git_own_bound(cwd: &Path, args: &[&str], budget: Duration) -> Result<String, GitError> {
    let mut command = git_command();
    command.args(args).current_dir(cwd);
    let started = Instant::now();
    let output = crate::bounded_output::output_within(&mut command, budget).map_err(|source| {
        GitError::Spawn {
            args: args.join(" "),
            source,
        }
    })?;
    let Some(output) = output else {
        note_git_timeout();
        return Err(GitError::timed_out_running(
            args.join(" "),
            Some(cwd),
            started,
            false,
            budget,
        ));
    };
    if !output.status.success() {
        return Err(GitError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

fn run_git_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let mut command = git_command();
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0");
    let output = run_git_command_output(command, args, git_timeout())?;
    Ok(output.stdout)
}

/// Run Git with bounded, file-backed stdin. Path lists use NUL separators so
/// repository names do not need to be UTF-8 or line-safe.
fn run_git_bytes_with_stdin(cwd: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>, GitError> {
    let args_display = args.join(" ");
    if input.len() > MAX_GIT_BATCH_INPUT_BYTES {
        return Err(GitError::Git {
            args: args_display,
            stderr: format!("Git stdin exceeded {MAX_GIT_BATCH_INPUT_BYTES} bytes"),
        });
    }
    let started = Instant::now();
    let deadline = effective_git_deadline(&args_display, git_timeout(), started)?;
    let mut input_file = tempfile::tempfile().map_err(|source| GitError::Spawn {
        args: format!("stage stdin for git {args_display}"),
        source,
    })?;
    input_file
        .write_all(input)
        .and_then(|()| input_file.seek(SeekFrom::Start(0)).map(|_| ()))
        .map_err(|source| GitError::Spawn {
            args: format!("stage stdin for git {args_display}"),
            source,
        })?;
    let budget = remaining_git_budget(&args_display, deadline)?;
    let mut command = git_command();
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0");
    let output = crate::bounded_output::output_with_input_limited(
        &mut command,
        input_file,
        budget,
        MAX_GIT_BATCH_OUTPUT_BYTES,
    )
    .map_err(|source| GitError::Spawn {
        args: args_display.clone(),
        source,
    })?;
    let output = match output {
        crate::bounded_output::LimitedOutput::Completed(output) => output,
        crate::bounded_output::LimitedOutput::Deadline => {
            note_git_timeout();
            return Err(GitError::timed_out_running(
                args_display,
                Some(cwd),
                started,
                deadline_is_outer(deadline),
                budget,
            ));
        }
        crate::bounded_output::LimitedOutput::OutputLimit => {
            return Err(GitError::Git {
                args: args_display,
                stderr: format!(
                    "Git output exceeded {MAX_GIT_BATCH_OUTPUT_BYTES} bytes per stream"
                ),
            });
        }
    };
    if !output.status.success() {
        return Err(GitError::Git {
            args: args_display,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(output.stdout)
}

fn checkout_worker_default_from_config(
    config: Result<String, GitError>,
    available_cpus: usize,
) -> Option<&'static str> {
    match config {
        Err(GitError::Git { stderr, .. }) if stderr.is_empty() && available_cpus >= 8 => {
            Some("checkout.workers=8")
        }
        Err(GitError::Git { stderr, .. }) if stderr.is_empty() && available_cpus >= 4 => {
            Some("checkout.workers=4")
        }
        // A configured value, including an invalid one, belongs to Git.
        // An unreadable config must not be mistaken for an absent value.
        _ => None,
    }
}

fn git_config_bool_or_unset(cwd: &Path, key: &str) -> Result<Option<bool>, GitError> {
    match run_git(cwd, &["config", "--bool", "--get", key]) {
        Ok(value) if value == "true" => Ok(Some(true)),
        Ok(value) if value == "false" => Ok(Some(false)),
        Ok(value) => Err(GitError::UntrustedOutput {
            detail: format!("git config returned invalid boolean for {key}: {value:?}"),
        }),
        Err(GitError::Git { stderr, .. }) if stderr.is_empty() => Ok(None),
        Err(error) => Err(error),
    }
}

fn nul_terminated_paths<'a>(bytes: &'a [u8], source: &str) -> Result<Vec<&'a [u8]>, GitError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.last() != Some(&0) {
        return Err(GitError::UntrustedOutput {
            detail: format!("git {source} returned a non-NUL-terminated path list"),
        });
    }
    let mut paths = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let Some(trailing) = paths.pop() else {
        return Err(GitError::UntrustedOutput {
            detail: format!("git {source} returned an empty path list record"),
        });
    };
    if !trailing.is_empty() || paths.iter().any(|path| path.is_empty()) {
        return Err(GitError::UntrustedOutput {
            detail: format!("git {source} returned an empty path entry"),
        });
    }
    Ok(paths)
}

fn nul_path_list(paths: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for path in paths {
        bytes.extend_from_slice(path);
        bytes.push(0);
    }
    bytes
}

fn tagged_index_paths(bytes: &[u8], wanted_tag: u8) -> Result<Vec<&[u8]>, GitError> {
    let entries = nul_terminated_paths(bytes, "ls-files -t")?;
    let mut paths = Vec::new();
    for entry in entries {
        if entry.len() < 3 || entry[1] != b' ' {
            return Err(GitError::UntrustedOutput {
                detail: "git ls-files -t returned a malformed index entry".into(),
            });
        }
        match entry[0] {
            b'H' | b'S' => {}
            tag => {
                return Err(GitError::UntrustedOutput {
                    detail: format!("git ls-files -t returned unexpected index tag {tag:?}"),
                });
            }
        }
        if entry[0] == wanted_tag {
            paths.push(&entry[2..]);
        }
    }
    Ok(paths)
}

fn missing_worktree_path_list(root: &Path, paths: &[&[u8]]) -> Result<Vec<u8>, GitError> {
    let mut missing = Vec::new();
    for path in paths {
        if !worktree_path_exists_or_is_obstructed(root, path)? {
            missing.extend_from_slice(path);
            missing.push(0);
        }
    }
    Ok(missing)
}

fn worktree_path_exists_or_is_obstructed(root: &Path, path: &[u8]) -> Result<bool, GitError> {
    #[cfg(unix)]
    let relative = Path::new(std::ffi::OsStr::from_bytes(path));
    #[cfg(not(unix))]
    let relative = Path::new(
        std::str::from_utf8(path).map_err(|_| GitError::UntrustedOutput {
            detail: "cannot inspect a non-UTF-8 Git path on this platform".into(),
        })?,
    );

    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(GitError::UntrustedOutput {
            detail: "Git index contains an unsafe absolute or parent path".into(),
        });
    }

    let mut candidate = root.to_path_buf();
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        candidate.push(component.as_os_str());
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if index + 1 < component_count => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Ok(true);
                }
            }
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => return Ok(true),
            Err(source) => {
                return Err(GitError::Spawn {
                    args: format!("inspect sparse checkout path {}", candidate.display()),
                    source,
                });
            }
        }
    }
    Ok(false)
}

fn run_git_bytes_with_index_for_tree_hash(
    cwd: &Path,
    index_file: &str,
    args: &[&str],
    writers_quiescent: &mut bool,
) -> Result<Vec<u8>, GitError> {
    let mut command = git_command();
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_INDEX_FILE", index_file);

    #[cfg(unix)]
    {
        let output =
            run_git_process_output_in_owned_group(command, args, git_timeout(), writers_quiescent)?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(output.stdout)
    }
    #[cfg(not(unix))]
    {
        // This platform cannot attest owned-group quiescence. Its automatic
        // artifact-use consumers remain disabled.
        *writers_quiescent = false;
        let output = run_git_command_output(command, args, git_timeout())?;
        Ok(output.stdout)
    }
}

fn untracked_hash_pathspecs(output: &[u8]) -> Result<Vec<u8>, GitError> {
    const ARGS: &str = "ls-files --others --exclude-standard -z";
    let malformed = || GitError::UntrustedOutput {
        detail: format!("git {ARGS} emitted malformed NUL-separated paths"),
    };
    if output.is_empty() {
        return Ok(Vec::new());
    }
    if output.last() != Some(&0) {
        return Err(malformed());
    }

    let mut pathspecs = Vec::with_capacity(output.len());
    for path in output[..output.len() - 1].split(|byte| *byte == 0) {
        if path.is_empty() {
            return Err(malformed());
        }
        let path_text = String::from_utf8_lossy(path);
        if crate::runtime_paths::is_broker_runtime_path(&path_text) {
            continue;
        }
        pathspecs.extend_from_slice(path);
        pathspecs.push(0);
    }
    Ok(pathspecs)
}

#[cfg(test)]
mod untracked_hash_pathspec_tests {
    use super::untracked_hash_pathspecs;

    #[test]
    fn accepts_empty_output_and_rejects_malformed_nul_framing() {
        assert_eq!(untracked_hash_pathspecs(b"").unwrap(), Vec::<u8>::new());
        assert!(untracked_hash_pathspecs(b"unterminated").is_err());
        assert!(untracked_hash_pathspecs(b"\0").is_err());
        assert!(untracked_hash_pathspecs(b"one\0\0").is_err());
    }

    #[test]
    fn filters_runtime_paths_and_preserves_literal_non_utf8_names() {
        let output = b".aethyme/run/worker.json\0.aethyme/broker.db-wal\0.aethyme/run\0:(exclude)**.txt\0non-utf8-\xff.txt\0";
        assert_eq!(
            untracked_hash_pathspecs(output).unwrap(),
            b".aethyme/run\0:(exclude)**.txt\0non-utf8-\xff.txt\0"
        );
    }
}

/// Like [`run_git`] but against a private index file (GIT_INDEX_FILE) so
/// staging operations never disturb the checkout's real index.
fn run_git_with_index(cwd: &Path, index_file: &str, args: &[&str]) -> Result<String, GitError> {
    run_git_inner(cwd, Some(index_file), args)
}

fn run_git_with_index_for_tree_hash(
    cwd: &Path,
    index_file: &str,
    args: &[&str],
    writers_quiescent: &mut bool,
) -> Result<String, GitError> {
    #[cfg(unix)]
    {
        run_git_with_index_in_owned_group(cwd, index_file, args, writers_quiescent)
    }
    #[cfg(not(unix))]
    {
        // This platform cannot attest owned-group quiescence. Its automatic
        // artifact-use consumers remain disabled.
        *writers_quiescent = false;
        run_git_with_index(cwd, index_file, args)
    }
}

#[cfg(unix)]
fn run_git_with_index_in_owned_group(
    cwd: &Path,
    index_file: &str,
    args: &[&str],
    writers_quiescent: &mut bool,
) -> Result<String, GitError> {
    let mut command = git_command();
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_INDEX_FILE", index_file);
    let output =
        run_git_process_output_in_owned_group(command, args, git_timeout(), writers_quiescent)?;
    if !output.status.success() {
        return Err(GitError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

fn run_git_inner(cwd: &Path, index_file: Option<&str>, args: &[&str]) -> Result<String, GitError> {
    let mut command = git_command();
    // No opportunistic index refresh: `git status` and friends otherwise
    // rewrite a checkout's index when file stats moved, and the broker reads
    // that index's mtime as evidence the session's agent is working. Merely
    // looking at a stale session (status, lease refresh) made it read as
    // active again for the next two hours.
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0");
    if let Some(index) = index_file {
        command.env("GIT_INDEX_FILE", index);
    }
    run_git_command(command, args, git_timeout())
}

/// Run a prepared git `command` within `budget`, killing it on expiry.
fn run_git_command(command: Command, args: &[&str], budget: Duration) -> Result<String, GitError> {
    let output = run_git_command_output(command, args, budget)?;
    // trim_end ONLY: porcelain status lines carry significant leading spaces.
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

fn run_git_command_output(
    command: Command,
    args: &[&str],
    budget: Duration,
) -> Result<std::process::Output, GitError> {
    let output = run_git_process_output(command, args, budget)?;
    if !output.status.success() {
        return Err(GitError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(output)
}

#[derive(Debug, Clone)]
struct GitConfigEntry {
    key: String,
    value: String,
}

/// Capture the effective config once, with a strict memory and entry bound.
/// `--includes` expands conditional includes in the source checkout; replay
/// later drops the include directives themselves so they cannot be resolved
/// again against the temporary Gitdir.
fn capture_bounded_git_config(root: &Path) -> Result<Vec<GitConfigEntry>, GitError> {
    const DISPLAY_ARGS: &str = "config --null --list --includes";
    let mut command = git_command();
    command
        .args(["config", "--null", "--list", "--includes"])
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0");
    let started = Instant::now();
    let output = match crate::bounded_output::output_limited(
        &mut command,
        git_timeout(),
        MAX_PUSH_QUERY_CONFIG_BYTES,
    )
    .map_err(|source| GitError::Spawn {
        args: DISPLAY_ARGS.into(),
        source,
    })? {
        crate::bounded_output::LimitedOutput::Completed(output) => output,
        crate::bounded_output::LimitedOutput::Deadline => {
            return Err(GitError::timed_out_running(
                DISPLAY_ARGS,
                Some(root),
                started,
                outer_deadline_bounds(started, git_timeout()),
                git_timeout(),
            ));
        }
        crate::bounded_output::LimitedOutput::OutputLimit => {
            return Err(GitError::Git {
                args: DISPLAY_ARGS.into(),
                stderr: "effective Git configuration exceeded its bounded capture limit".into(),
            });
        }
    };
    if !output.status.success() {
        return Err(GitError::Git {
            args: DISPLAY_ARGS.into(),
            stderr: "effective Git configuration could not be captured".into(),
        });
    }

    parse_bounded_git_config(&output.stdout)
}

fn parse_bounded_git_config(bytes: &[u8]) -> Result<Vec<GitConfigEntry>, GitError> {
    const DISPLAY_ARGS: &str = "config --null --list --includes";
    if bytes.len() > MAX_PUSH_QUERY_CONFIG_BYTES {
        return Err(GitError::Git {
            args: DISPLAY_ARGS.into(),
            stderr: "effective Git configuration exceeded its bounded capture limit".into(),
        });
    }
    if !bytes.is_empty() && bytes.last() != Some(&0) {
        return Err(GitError::Git {
            args: DISPLAY_ARGS.into(),
            stderr: "effective Git configuration was truncated".into(),
        });
    }
    let mut entries = Vec::new();
    if bytes.is_empty() {
        return Ok(entries);
    }
    for record in bytes[..bytes.len() - 1].split(|byte| *byte == 0) {
        if entries.len() >= MAX_PUSH_QUERY_CONFIG_ENTRIES {
            return Err(GitError::Git {
                args: DISPLAY_ARGS.into(),
                stderr: "effective Git configuration exceeded its bounded entry limit".into(),
            });
        }
        let Some(separator) = record.iter().position(|byte| *byte == b'\n') else {
            return Err(GitError::Git {
                args: DISPLAY_ARGS.into(),
                stderr: "effective Git configuration contained a valueless or malformed entry"
                    .into(),
            });
        };
        let key = std::str::from_utf8(&record[..separator]).map_err(|_| GitError::Git {
            args: DISPLAY_ARGS.into(),
            stderr: "effective Git configuration contained a non-UTF-8 key".into(),
        })?;
        let value = std::str::from_utf8(&record[separator + 1..]).map_err(|_| GitError::Git {
            args: DISPLAY_ARGS.into(),
            stderr: "effective Git configuration contained a non-UTF-8 value".into(),
        })?;
        if key.is_empty() {
            return Err(GitError::Git {
                args: DISPLAY_ARGS.into(),
                stderr: "effective Git configuration contained an empty key".into(),
            });
        }
        entries.push(GitConfigEntry {
            key: key.into(),
            value: value.into(),
        });
    }
    Ok(entries)
}

fn is_remote_config_key(key: &str) -> bool {
    key.get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("remote."))
}

fn is_rewrite_or_include_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    (lower.starts_with("url.")
        && (lower.ends_with(".insteadof") || lower.ends_with(".pushinsteadof")))
        || lower == "include.path"
        || (lower.starts_with("includeif.") && lower.ends_with(".path"))
}

fn is_repository_context_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "core.bare" | "core.worktree" | "core.repositoryformatversion"
    ) || lower.starts_with("extensions.")
}

fn selected_remote_option<'a>(key: &'a str, remote: &str) -> Option<&'a str> {
    if !is_remote_config_key(key) {
        return None;
    }
    let section = key.get(7..)?;
    let (name, option) = section.rsplit_once('.')?;
    (name == remote).then_some(option)
}

fn push_query_config(
    entries: &[GitConfigEntry],
    remote: &str,
    exact_url: &str,
) -> Result<Vec<GitConfigEntry>, GitError> {
    if exact_url.len() > MAX_PUSH_QUERY_CONFIG_BYTES {
        return Err(GitError::Git {
            args: "ls-remote --refs <endpoint>".into(),
            stderr: "push endpoint exceeded its bounded replay limit".into(),
        });
    }
    let mut replay = Vec::with_capacity(entries.len() + 1);
    for entry in entries {
        if is_rewrite_or_include_key(&entry.key) || is_repository_context_key(&entry.key) {
            continue;
        }
        if is_remote_config_key(&entry.key) {
            if let Some(option) = selected_remote_option(&entry.key, remote) {
                if option.eq_ignore_ascii_case("url") || option.eq_ignore_ascii_case("pushurl") {
                    continue;
                }
                replay.push(GitConfigEntry {
                    key: format!("remote.{PUSH_EVIDENCE_REMOTE}.{option}"),
                    value: entry.value.clone(),
                });
            }
            continue;
        }
        replay.push(entry.clone());
    }
    replay.push(GitConfigEntry {
        key: format!("remote.{PUSH_EVIDENCE_REMOTE}.url"),
        value: exact_url.into(),
    });
    let replay_bytes = replay.iter().try_fold(0usize, |total, entry| {
        total
            .checked_add(entry.key.len())
            .and_then(|total| total.checked_add(entry.value.len()))
    });
    if replay.len() > MAX_PUSH_QUERY_CONFIG_ENTRIES
        || replay_bytes.is_none_or(|bytes| bytes > MAX_PUSH_QUERY_CONFIG_BYTES)
    {
        return Err(GitError::Git {
            args: "ls-remote --refs <endpoint>".into(),
            stderr: "effective Git configuration exceeded its bounded replay limit".into(),
        });
    }
    Ok(replay)
}

/// Remove inherited repository/config routing and then use an isolated bare
/// Gitdir. Other effective config is explicitly replayed by the caller, so
/// credential helpers and transport settings retain their source-checkout
/// values while URL rewrite rules cannot be applied a second time.
fn clear_git_repository_context(command: &mut Command, empty_global: &Path) {
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_PREFIX",
        "GIT_CONFIG",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(variable);
    }
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", empty_global);
}

/// Exact remote refs read for a push, and one advisory ref read alongside
/// them. See [`GitRepo::remote_ref_oids_with_advisory`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteRefObservation {
    pub(crate) refs: BTreeMap<String, Option<String>>,
    pub(crate) advisory: Option<Option<String>>,
}

enum AdvisoryLine {
    /// The advisory ref itself.
    Exact,
    /// A longer ref only the advisory pattern matched by path tail.
    TailOnly,
    /// Not the advisory pattern's line: the destinations' rules apply.
    NotAdvisory,
}

/// Whether an unrequested `ls-remote` line came from the advisory pattern.
///
/// Git prints every ref whose path ends in `/<pattern>`. A line that a
/// destination pattern could also have produced keeps failing the read as it
/// always did, so the advisory pattern never changes the destinations' outcome.
fn advisory_line(advisory: Option<&str>, destinations: &[String], line_ref: &str) -> AdvisoryLine {
    let Some(advisory) = advisory else {
        return AdvisoryLine::NotAdvisory;
    };
    let tail_of = |pattern: &str| line_ref.ends_with(&format!("/{pattern}"));
    if destinations
        .iter()
        .any(|destination| tail_of(destination.as_str()))
    {
        return AdvisoryLine::NotAdvisory;
    }
    if line_ref == advisory {
        AdvisoryLine::Exact
    } else if tail_of(advisory) {
        AdvisoryLine::TailOnly
    } else {
        AdvisoryLine::NotAdvisory
    }
}

#[cfg(test)]
mod advisory_ref_tests {
    use super::{AdvisoryLine, GitRepo, advisory_line};
    use std::path::Path;
    use std::process::Command;

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn only_lines_the_advisory_pattern_alone_could_produce_are_set_aside() {
        let destinations = ["refs/heads/agent/x".to_string()];
        let main = Some("refs/heads/main");
        assert!(matches!(
            advisory_line(main, &destinations, "refs/heads/main"),
            AdvisoryLine::Exact
        ));
        assert!(matches!(
            advisory_line(main, &destinations, "refs/heads/y/refs/heads/main"),
            AdvisoryLine::TailOnly
        ));
        // A line a destination pattern produced keeps its old meaning.
        assert!(matches!(
            advisory_line(main, &destinations, "refs/heads/y/refs/heads/agent/x"),
            AdvisoryLine::NotAdvisory
        ));
        assert!(matches!(
            advisory_line(main, &destinations, "refs/heads/other"),
            AdvisoryLine::NotAdvisory
        ));
        assert!(matches!(
            advisory_line(None, &destinations, "refs/heads/main"),
            AdvisoryLine::NotAdvisory
        ));
    }

    /// The default branch read alongside a push's own refs never changes
    /// their evidence, and a longer ref that merely ends like it is ignored.
    #[test]
    fn the_advisory_ref_rides_the_same_read_without_changing_the_destinations() {
        let root = tempfile::tempdir().unwrap();
        let remote = root.path().join("remote.git");
        let local = root.path().join("local");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&local, &["init", "-q", "-b", "main"]);
        git(&local, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(
            &local,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&local, &["push", "-q", "origin", "main"]);
        let main = git(&local, &["rev-parse", "HEAD"]);
        git(&local, &["commit", "-q", "--allow-empty", "-m", "session"]);
        let session = git(&local, &["rev-parse", "HEAD"]);
        git(&local, &["push", "-q", "origin", "HEAD:refs/heads/agent/x"]);
        git(
            &local,
            &["push", "-q", "origin", "HEAD:refs/heads/y/refs/heads/main"],
        );
        let repo = GitRepo::discover(&local).unwrap();
        let destinations = vec![
            "refs/heads/agent/x".to_string(),
            "refs/heads/agent/missing".to_string(),
        ];

        let plain = repo.remote_ref_oids("origin", &destinations).unwrap();
        let observed = repo
            .remote_ref_oids_with_advisory("origin", &destinations, "refs/heads/main")
            .unwrap();
        assert_eq!(observed.refs, plain);
        assert_eq!(
            plain["refs/heads/agent/x"].as_deref(),
            Some(session.as_str())
        );
        assert_eq!(plain["refs/heads/agent/missing"], None);
        assert_eq!(observed.advisory, Some(Some(main)));

        git(&remote, &["update-ref", "-d", "refs/heads/main"]);
        let observed = repo
            .remote_ref_oids_with_advisory("origin", &destinations, "refs/heads/main")
            .unwrap();
        assert_eq!(observed.refs, plain);
        assert_eq!(observed.advisory, Some(None), "absent, not ambiguous");
    }
}

fn exact_push_endpoint_output(
    root: &Path,
    remote: &str,
    exact_url: &str,
    destinations: &[String],
    display_args: &str,
) -> Result<std::process::Output, GitError> {
    if exact_url.len() > MAX_PUSH_QUERY_CONFIG_BYTES
        || exact_url.is_empty()
        || exact_url.chars().any(char::is_control)
    {
        return Err(GitError::Git {
            args: display_args.into(),
            stderr: "push endpoint was empty or malformed".into(),
        });
    }
    let captured = capture_bounded_git_config(root)?;
    let replay = push_query_config(&captured, remote, exact_url)?;
    let private = tempfile::tempdir().map_err(|source| GitError::Spawn {
        args: display_args.into(),
        source,
    })?;
    let empty_global = private.path().join("empty-global-config");
    fs::write(&empty_global, []).map_err(|source| GitError::Spawn {
        args: display_args.into(),
        source,
    })?;
    let empty_template = private.path().join("empty-template");
    fs::create_dir(&empty_template).map_err(|source| GitError::Spawn {
        args: display_args.into(),
        source,
    })?;
    let isolated_git_dir = private.path().join("gitdir");

    let mut init = git_command();
    clear_git_repository_context(&mut init, &empty_global);
    init.args(["init", "--quiet", "--bare", "--template"])
        .arg(&empty_template)
        .arg(&isolated_git_dir)
        .current_dir(private.path());
    let init_output = run_git_process_output(
        init,
        &["init --quiet --bare --template <private> <private>"],
        git_timeout(),
    )?;
    if !init_output.status.success() {
        return Err(GitError::Git {
            args: "init --quiet --bare --template <private> <private>".into(),
            stderr: "could not initialize isolated remote-query Gitdir".into(),
        });
    }

    let mut command = git_command();
    command
        .args(["ls-remote", "--refs", PUSH_EVIDENCE_REMOTE])
        .args(destinations)
        .current_dir(root);
    clear_git_repository_context(&mut command, &empty_global);
    command.env("GIT_DIR", &isolated_git_dir);
    for (index, entry) in replay.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), &entry.key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), &entry.value);
    }
    command.env("GIT_CONFIG_COUNT", replay.len().to_string());
    run_git_process_output(command, &[display_args], git_timeout())
}

#[cfg(test)]
mod exact_push_endpoint_config_tests {
    use super::{
        GitConfigEntry, MAX_PUSH_QUERY_CONFIG_BYTES, MAX_PUSH_QUERY_CONFIG_ENTRIES,
        PUSH_EVIDENCE_REMOTE, is_repository_context_key, parse_bounded_git_config,
        push_query_config,
    };

    fn entry(key: &str, value: &str) -> GitConfigEntry {
        GitConfigEntry {
            key: key.into(),
            value: value.into(),
        }
    }

    #[test]
    fn replay_drops_rewrites_and_includes_but_keeps_auth_and_selected_remote_options() {
        let entries = vec![
            entry("url.file:///peer-b.insteadof", "file:///peer-a"),
            entry("url.file:///peer-b.pushinsteadof", "push-peer-a"),
            entry("include.path", "/private/config/auth.inc"),
            entry(
                "includeif.gitdir:/repo/.path",
                "/private/config/conditional.inc",
            ),
            entry("credential.helper", "credential-helper --use-keychain"),
            entry("core.sshcommand", "ssh -F relative-config"),
            entry("core.bare", "false"),
            entry("core.worktree", "/source/repository"),
            entry("core.repositoryformatversion", "1"),
            entry("extensions.objectformat", "sha256"),
            entry(
                "http.https://peer.example/.extraheader",
                "Authorization: secret",
            ),
            entry("remote.publication.url", "push-peer-a"),
            entry("remote.publication.pushurl", "file:///peer-b"),
            entry("remote.publication.uploadpack", "git-upload-pack --strict"),
            entry("remote.publication.proxy", "http://proxy.example"),
            entry("remote.other.uploadpack", "unexpected-helper"),
        ];

        let replay = push_query_config(&entries, "publication", "file:///peer-a").unwrap();
        assert!(!replay.iter().any(|entry| {
            let key = entry.key.to_ascii_lowercase();
            key.starts_with("url.")
                || key == "include.path"
                || (key.starts_with("includeif.") && key.ends_with(".path"))
        }));
        assert!(replay.iter().any(|entry| {
            entry.key == "credential.helper" && entry.value == "credential-helper --use-keychain"
        }));
        assert!(replay.iter().any(|entry| {
            entry.key == "core.sshcommand" && entry.value == "ssh -F relative-config"
        }));
        assert!(
            !replay
                .iter()
                .any(|entry| { is_repository_context_key(&entry.key) })
        );
        assert!(replay.iter().any(|entry| {
            entry.key == "http.https://peer.example/.extraheader"
                && entry.value == "Authorization: secret"
        }));
        assert!(
            replay
                .iter()
                .any(|entry| { entry.key == format!("remote.{PUSH_EVIDENCE_REMOTE}.uploadpack") })
        );
        assert!(
            replay
                .iter()
                .any(|entry| entry.key == format!("remote.{PUSH_EVIDENCE_REMOTE}.proxy"))
        );
        assert_eq!(
            replay
                .iter()
                .filter(|entry| entry.key == format!("remote.{PUSH_EVIDENCE_REMOTE}.url"))
                .map(|entry| entry.value.as_str())
                .collect::<Vec<_>>(),
            vec!["file:///peer-a"]
        );
        assert!(
            !replay
                .iter()
                .any(|entry| entry.value == "unexpected-helper")
        );
        assert!(!replay.iter().any(|entry| {
            entry.key == "remote.publication.url" || entry.key == "remote.publication.pushurl"
        }));
    }

    #[test]
    fn config_capture_refuses_oversize_truncated_valueless_and_non_utf8_data() {
        assert!(parse_bounded_git_config(&vec![b'x'; MAX_PUSH_QUERY_CONFIG_BYTES + 1]).is_err());
        assert!(parse_bounded_git_config(b"core.example\nvalue").is_err());
        assert!(parse_bounded_git_config(b"core.flag\0").is_err());
        assert!(parse_bounded_git_config(b"core.example\n\xff\0").is_err());
        assert_eq!(
            parse_bounded_git_config(b"credential.helper\n\0").unwrap()[0]
                .value
                .as_str(),
            ""
        );

        let mut too_many = Vec::new();
        for _ in 0..=MAX_PUSH_QUERY_CONFIG_ENTRIES {
            too_many.extend_from_slice(b"core.example\nvalue\0");
        }
        assert!(parse_bounded_git_config(&too_many).is_err());
    }

    #[test]
    fn replay_refuses_a_query_config_that_exceeds_the_byte_bound() {
        let oversized = "x".repeat(MAX_PUSH_QUERY_CONFIG_BYTES + 1);
        assert!(push_query_config(&[], "publication", &oversized).is_err());

        let full_capture = vec![entry(
            "core.value",
            &"x".repeat(MAX_PUSH_QUERY_CONFIG_BYTES),
        )];
        assert!(push_query_config(&full_capture, "publication", "x").is_err());

        let full_entry_count = (0..MAX_PUSH_QUERY_CONFIG_ENTRIES)
            .map(|index| entry(&format!("core.option{index}"), "value"))
            .collect::<Vec<_>>();
        assert!(push_query_config(&full_entry_count, "publication", "x").is_err());
    }
}

/// Run Git within its ordinary timeout, or the remaining part of the
/// enclosing interactive inspection deadline. Unlike
/// [`run_git_command_output`], this preserves non-zero exit status for
/// commands where it is an expected answer (such as `merge-base --is-ancestor`).
fn run_git_process_output(
    mut command: Command,
    args: &[&str],
    budget: Duration,
) -> Result<std::process::Output, GitError> {
    let per_call = budget;
    let budget = match active_git_deadline() {
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                note_git_timeout();
                return Err(GitError::timed_out_before_start(
                    args.join(" "),
                    command.get_current_dir(),
                    Duration::ZERO,
                ));
            }
            budget.min(remaining)
        }
        None => budget,
    };
    let started = Instant::now();
    let output = crate::bounded_output::output_within(&mut command, budget).map_err(|source| {
        GitError::Spawn {
            args: args.join(" "),
            source,
        }
    })?;
    let Some(output) = output else {
        note_git_timeout();
        return Err(GitError::timed_out_running(
            args.join(" "),
            command.get_current_dir(),
            started,
            outer_deadline_bounds(started, per_call),
            budget,
        ));
    };
    Ok(output)
}

#[cfg(unix)]
fn run_git_process_output_in_owned_group(
    mut command: Command,
    args: &[&str],
    budget: Duration,
    writers_quiescent: &mut bool,
) -> Result<std::process::Output, GitError> {
    let per_call = budget;
    let budget = match active_git_deadline() {
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                note_git_timeout();
                return Err(GitError::timed_out_before_start(
                    args.join(" "),
                    command.get_current_dir(),
                    Duration::ZERO,
                ));
            }
            budget.min(remaining)
        }
        None => budget,
    };
    let deadline = Instant::now()
        .checked_add(budget)
        .ok_or_else(|| GitError::Spawn {
            args: args.join(" "),
            source: std::io::Error::other("git process-group deadline overflow"),
        })?;
    *writers_quiescent = false;
    let started = Instant::now();
    let output = crate::bounded_output::output_limited_in_owned_group(
        &mut command,
        budget,
        MAX_WORKING_TREE_HASH_OUTPUT_BYTES,
    )
    .map_err(|source| GitError::Spawn {
        args: args.join(" "),
        source,
    })?;
    // Every LimitedOutput variant from this runner has verified owned-group
    // cleanup, including deadline and output-cap failures. An I/O or cleanup
    // error returned above leaves the proof false.
    *writers_quiescent = true;
    match output {
        crate::bounded_output::LimitedOutput::Completed(output) if Instant::now() < deadline => {
            Ok(output)
        }
        crate::bounded_output::LimitedOutput::Completed(_)
        | crate::bounded_output::LimitedOutput::Deadline => {
            note_git_timeout();
            Err(GitError::timed_out_running(
                args.join(" "),
                command.get_current_dir(),
                started,
                outer_deadline_bounds(started, per_call),
                budget,
            ))
        }
        crate::bounded_output::LimitedOutput::OutputLimit => Err(GitError::UntrustedOutput {
            detail: format!(
                "git {} exceeded the private-index hash output cap of {} bytes per stream",
                args.join(" "),
                MAX_WORKING_TREE_HASH_OUTPUT_BYTES
            ),
        }),
    }
}

fn effective_git_deadline(
    args: &str,
    budget: Duration,
    started: Instant,
) -> Result<Instant, GitError> {
    let requested = started.checked_add(budget).ok_or_else(|| GitError::Spawn {
        args: args.to_string(),
        source: std::io::Error::other("git deadline overflow"),
    })?;
    let deadline = active_git_deadline().map_or(requested, |outer| requested.min(outer));
    if Instant::now() >= deadline {
        note_git_timeout();
        return Err(GitError::timed_out_before_start(
            args.to_string(),
            None,
            Duration::ZERO,
        ));
    }
    Ok(deadline)
}

fn remaining_git_budget(args: &str, deadline: Instant) -> Result<Duration, GitError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        note_git_timeout();
        return Err(GitError::timed_out_before_start(
            args.to_string(),
            None,
            Duration::ZERO,
        ));
    }
    Ok(remaining)
}
/// A commit's tree and parents, in Git's stored parent order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitShape {
    pub tree: String,
    pub parents: Vec<String>,
}

/// Errors while staging a bounded `--stdin` list for Git.
#[derive(Debug)]
enum GitBatchInputError {
    TooLarge,
    TooManyQueries,
    TimedOut,
    Io(std::io::Error),
}

/// Check the complete input size before creating or writing the temporary file.
fn checked_git_batch_input_len(
    commits: &[String],
    deadline: Instant,
) -> Result<(), GitBatchInputError> {
    let mut total = 0_usize;
    for commit in commits {
        if Instant::now() >= deadline {
            return Err(GitBatchInputError::TimedOut);
        }
        let line_bytes = commit
            .len()
            .checked_add(1)
            .ok_or(GitBatchInputError::TooLarge)?;
        total = total
            .checked_add(line_bytes)
            .filter(|bytes| *bytes <= MAX_GIT_BATCH_INPUT_BYTES)
            .ok_or(GitBatchInputError::TooLarge)?;
    }
    if Instant::now() >= deadline {
        return Err(GitBatchInputError::TimedOut);
    }
    Ok(())
}

const GIT_BATCH_INPUT_CHUNK_BYTES: usize = 8 * 1024;

fn append_git_batch_bytes(
    file: &mut std::fs::File,
    buffer: &mut Vec<u8>,
    mut bytes: &[u8],
    deadline: Instant,
) -> Result<(), GitBatchInputError> {
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(GitBatchInputError::TimedOut);
        }
        let room = GIT_BATCH_INPUT_CHUNK_BYTES - buffer.len();
        let count = room.min(bytes.len());
        buffer.extend_from_slice(&bytes[..count]);
        bytes = &bytes[count..];
        if buffer.len() == GIT_BATCH_INPUT_CHUNK_BYTES {
            file.write_all(buffer).map_err(GitBatchInputError::Io)?;
            if Instant::now() >= deadline {
                return Err(GitBatchInputError::TimedOut);
            }
            buffer.clear();
        }
    }
    Ok(())
}

/// Stage commit names incrementally without allocating a second unbounded list.
fn git_batch_input(
    commits: &[String],
    deadline: Instant,
) -> Result<std::fs::File, GitBatchInputError> {
    checked_git_batch_input_len(commits, deadline)?;
    let mut file = tempfile::tempfile().map_err(GitBatchInputError::Io)?;
    if Instant::now() >= deadline {
        return Err(GitBatchInputError::TimedOut);
    }
    let mut buffer = Vec::with_capacity(GIT_BATCH_INPUT_CHUNK_BYTES);
    for commit in commits {
        if Instant::now() >= deadline {
            return Err(GitBatchInputError::TimedOut);
        }
        append_git_batch_bytes(&mut file, &mut buffer, commit.as_bytes(), deadline)?;
        append_git_batch_bytes(&mut file, &mut buffer, b"\n", deadline)?;
    }
    if Instant::now() >= deadline {
        return Err(GitBatchInputError::TimedOut);
    }
    if !buffer.is_empty() {
        file.write_all(&buffer).map_err(GitBatchInputError::Io)?;
        if Instant::now() >= deadline {
            return Err(GitBatchInputError::TimedOut);
        }
    }
    file.seek(SeekFrom::Start(0))
        .map_err(GitBatchInputError::Io)?;
    if Instant::now() >= deadline {
        return Err(GitBatchInputError::TimedOut);
    }
    Ok(file)
}

/// Stage `cat-file --batch-check` requests without constructing the complete
/// query stream in memory. The count, bytes and deadline are checked before
/// Git is started so an oversized exact-tree scan cannot turn into an
/// unbounded write or wait.
fn git_batch_check_input(
    queries: &[(&str, &str)],
    deadline: Instant,
) -> Result<std::fs::File, GitBatchInputError> {
    if queries.len() > MAX_GIT_BATCH_QUERY_COUNT {
        return Err(GitBatchInputError::TooManyQueries);
    }

    checked_git_batch_check_input_len(queries, MAX_GIT_BATCH_INPUT_BYTES, deadline)?;

    let mut file = tempfile::tempfile().map_err(GitBatchInputError::Io)?;
    let mut buffer = Vec::with_capacity(GIT_BATCH_INPUT_CHUNK_BYTES);
    for (rev, path) in queries {
        append_git_batch_bytes(&mut file, &mut buffer, rev.as_bytes(), deadline)?;
        append_git_batch_bytes(&mut file, &mut buffer, b":", deadline)?;
        append_git_batch_bytes(&mut file, &mut buffer, path.as_bytes(), deadline)?;
        append_git_batch_bytes(&mut file, &mut buffer, b"\n", deadline)?;
    }
    if !buffer.is_empty() {
        file.write_all(&buffer).map_err(GitBatchInputError::Io)?;
    }
    if Instant::now() >= deadline {
        return Err(GitBatchInputError::TimedOut);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(GitBatchInputError::Io)?;
    if Instant::now() >= deadline {
        return Err(GitBatchInputError::TimedOut);
    }
    Ok(file)
}

/// Check the complete query stream size before creating or writing the
/// temporary file. Each line includes the revision, path, separator and
/// newline, so long paths count against the same input bound.
fn checked_git_batch_check_input_len(
    queries: &[(&str, &str)],
    max_bytes: usize,
    deadline: Instant,
) -> Result<usize, GitBatchInputError> {
    let mut total = 0_usize;
    for (rev, path) in queries {
        if Instant::now() >= deadline {
            return Err(GitBatchInputError::TimedOut);
        }
        let line_bytes = rev
            .len()
            .checked_add(path.len())
            .and_then(|bytes| bytes.checked_add(2))
            .ok_or(GitBatchInputError::TooLarge)?;
        if line_bytes > max_bytes {
            return Err(GitBatchInputError::TooLarge);
        }
        total = total
            .checked_add(line_bytes)
            .filter(|bytes| *bytes <= max_bytes)
            .ok_or(GitBatchInputError::TooLarge)?;
    }
    Ok(total)
}

fn git_batch_input_error(
    args: &str,
    _started: Instant,
    _deadline: Instant,
    error: GitBatchInputError,
) -> GitError {
    match error {
        GitBatchInputError::TooLarge => GitError::Git {
            args: args.to_string(),
            stderr: format!("Git batch input exceeded {MAX_GIT_BATCH_INPUT_BYTES} bytes"),
        },
        GitBatchInputError::TooManyQueries => GitError::Git {
            args: args.to_string(),
            stderr: format!("Git batch exceeded {MAX_GIT_BATCH_QUERY_COUNT} queries"),
        },
        GitBatchInputError::TimedOut => {
            note_git_timeout();
            GitError::timed_out_before_start(args.to_string(), None, Duration::ZERO)
        }
        GitBatchInputError::Io(source) => GitError::Spawn {
            args: args.to_string(),
            source,
        },
    }
}

/// Uncommitted work in a checkout, counted the way `git status` shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UncommittedSummary {
    /// Tracked paths with staged or unstaged changes.
    pub modified: Vec<String>,
    /// Untracked entries; an untracked directory is one entry, not its files.
    pub untracked: Vec<String>,
}

impl UncommittedSummary {
    pub fn is_empty(&self) -> bool {
        self.modified.is_empty() && self.untracked.is_empty()
    }

    /// Modified paths plus untracked entries.
    pub fn len(&self) -> usize {
        self.modified.len() + self.untracked.len()
    }

    /// A short human description, e.g. `3 modified, 1 untracked (target/)`.
    pub fn describe(&self, max_paths: usize) -> String {
        let mut parts = Vec::new();
        if !self.modified.is_empty() {
            parts.push(format!("{} modified", self.modified.len()));
        }
        if !self.untracked.is_empty() {
            parts.push(format!("{} untracked", self.untracked.len()));
        }
        let shown = self
            .modified
            .iter()
            .chain(&self.untracked)
            .take(max_paths)
            .map(String::as_str)
            .collect::<Vec<_>>();
        let more = self.len().saturating_sub(shown.len());
        let mut sample = shown.join(", ");
        if more > 0 {
            sample.push_str(&format!(", +{more} more"));
        }
        format!("{} ({sample})", parts.join(", "))
    }
}

/// Result of a `git merge-tree --write-tree` simulation.
#[derive(Debug)]
pub struct MergeSimulation {
    /// The merged tree oid (written even when conflicted).
    pub tree: String,
    /// Conflicted paths; empty means the merge is clean.
    pub conflicts: Vec<String>,
}

/// One commit from a `git log` walk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LoggedCommit {
    pub sha: String,
    /// Committer time, Unix epoch milliseconds.
    pub committed_at_ms: i64,
    pub subject: String,
}

/// Default branch advertised by a configured Git remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDefaultBranch {
    pub ref_name: String,
    pub sha: String,
}

/// Reason the broker records on worktrees it locks on a configured, possibly
/// removable root; only a lock with exactly this reason is released by it.
pub const BROKER_WORKTREE_LOCK_REASON: &str =
    "aethyme: worktree on a configured, possibly removable root";

/// Set `AETHYME_TREE_HASH_SEED=off` to always take the full tree hash.
const TREE_HASH_SEED_ENV: &str = "AETHYME_TREE_HASH_SEED";

/// Settings every seeded tree-hash Git command runs with, whatever the
/// repository or user configures: stat matching as strict as Git's default,
/// and no source of "unchanged" other than the stat data itself.
const SEEDED_GIT_CONFIG: [&str; 10] = [
    "-c",
    "core.ignoreStat=false",
    "-c",
    "core.trustctime=true",
    "-c",
    "core.checkStat=default",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.untrackedCache=false",
];

/// Why the seeded tree hash gave way to the full one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeedFallback {
    SwitchedOff,
    /// The checkout's index or Git directory could not be read or copied.
    Unreadable,
    /// The index changed while it was copied.
    IndexMoving,
    /// The copy's modification time could not be made the original's: a
    /// later one would stop Git re-reading entries edited in the second the
    /// original was written.
    TimestampNotKept,
    /// An entry is assume-unchanged or skip-worktree.
    EntryFlags,
    /// The index names another tree than HEAD (staged changes).
    IndexNotHead,
    /// An attribute or setting converts content between files and Git.
    ContentConversion,
    /// A Git command failed or printed something unexpected.
    GitFailed,
}

impl SeedFallback {
    pub(crate) fn counter(self) -> &'static str {
        match self {
            Self::SwitchedOff => "tree_hash_seed_fallback_switched_off",
            Self::Unreadable => "tree_hash_seed_fallback_unreadable",
            Self::IndexMoving => "tree_hash_seed_fallback_index_moving",
            Self::TimestampNotKept => "tree_hash_seed_fallback_timestamp_not_kept",
            Self::EntryFlags => "tree_hash_seed_fallback_entry_flags",
            Self::IndexNotHead => "tree_hash_seed_fallback_index_not_head",
            Self::ContentConversion => "tree_hash_seed_fallback_content_conversion",
            Self::GitFailed => "tree_hash_seed_fallback_git_failed",
        }
    }
}

/// Copy `index` to `destination` and give the copy the original's
/// modification time, so Git's racy-clean rule judges the copy's entries
/// against the moment the original was written. A copy taken while the
/// index was being rewritten is refused.
fn copy_index_with_its_timestamp(index: &Path, destination: &Path) -> Result<(), SeedFallback> {
    let before = std::fs::metadata(index).map_err(|_| SeedFallback::Unreadable)?;
    let modified = before.modified().map_err(|_| SeedFallback::Unreadable)?;
    let bytes = std::fs::read(index).map_err(|_| SeedFallback::Unreadable)?;
    let after = std::fs::metadata(index).map_err(|_| SeedFallback::Unreadable)?;
    if bytes.len() as u64 != before.len()
        || after.len() != before.len()
        || after.modified().ok() != Some(modified)
    {
        return Err(SeedFallback::IndexMoving);
    }
    std::fs::write(destination, &bytes).map_err(|_| SeedFallback::Unreadable)?;
    std::fs::File::options()
        .write(true)
        .open(destination)
        .and_then(|file| file.set_modified(modified))
        .map_err(|_| SeedFallback::TimestampNotKept)?;
    // The racy-clean rule judges entries against this timestamp: a copy that
    // reads back later than the original would trust entries Git must re-read.
    let kept = std::fs::metadata(destination)
        .and_then(|metadata| metadata.modified())
        .map_err(|_| SeedFallback::TimestampNotKept)?;
    if kept > modified {
        return Err(SeedFallback::TimestampNotKept);
    }
    Ok(())
}

/// Whether any tracked path has an attribute that makes Git convert content
/// on its way in or out (`text`, `eol`, `crlf`, `filter`, `ident`,
/// `working-tree-encoding`): set, or set to a value. Unspecified and unset
/// (`-text`, `binary`) convert nothing. Git resolves every attribute source
/// (the tree's files, `info/attributes`, the user's and the system's).
fn attributes_convert_content(
    root: &Path,
    index_file: &str,
    paths: &[u8],
) -> Result<bool, SeedFallback> {
    let _span = crate::phase_telemetry::span("tree_hash_seed_attributes");
    let mut command = git_command();
    command
        .args([
            "check-attr",
            "--stdin",
            "-z",
            "text",
            "eol",
            "crlf",
            "filter",
            "ident",
            "working-tree-encoding",
        ])
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_INDEX_FILE", index_file)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn().map_err(|_| SeedFallback::GitFailed)?;
    let mut stdin = child.stdin.take().ok_or(SeedFallback::GitFailed)?;
    let input = paths.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child
        .wait_with_output()
        .map_err(|_| SeedFallback::GitFailed)?;
    let written = writer.join().map_err(|_| SeedFallback::GitFailed)?;
    if written.is_err() || !output.status.success() {
        return Err(SeedFallback::GitFailed);
    }
    // `-z` output: <path> NUL <attribute> NUL <value> NUL, repeated.
    let fields: Vec<&[u8]> = output.stdout.split(|byte| *byte == 0).collect();
    let records = fields.len() / 3;
    if records == 0 && !output.stdout.is_empty() {
        return Err(SeedFallback::GitFailed);
    }
    for record in 0..records {
        let value = fields[record * 3 + 2];
        if value != b"unspecified" && value != b"unset" {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The entry count in an index file's header (bytes 8 to 12, big-endian).
fn index_entry_count(index: &Path) -> Option<u64> {
    let mut header = [0_u8; 12];
    let mut file = std::fs::File::open(index).ok()?;
    file.read_exact(&mut header).ok()?;
    header
        .starts_with(b"DIRC")
        .then_some(u64::from(u32::from_be_bytes([
            header[8], header[9], header[10], header[11],
        ])))
}

fn count_nul_records(bytes: &[u8]) -> u64 {
    bytes.iter().filter(|byte| **byte == 0).count() as u64
}

static TREE_HASH_TRACKED_FILES: AtomicU64 = AtomicU64::new(0);
static TREE_HASH_UNTRACKED_FILES: AtomicU64 = AtomicU64::new(0);
static TREE_HASHES: AtomicU64 = AtomicU64::new(0);

/// Add one tree hash's file counts to the command's `tree_hash_files`
/// attachment (totals for the whole command, so the last write is complete).
fn record_tree_hash_files(tracked: u64, untracked: u64) {
    let tracked = TREE_HASH_TRACKED_FILES.fetch_add(tracked, Ordering::Relaxed) + tracked;
    let untracked = TREE_HASH_UNTRACKED_FILES.fetch_add(untracked, Ordering::Relaxed) + untracked;
    let hashes = TREE_HASHES.fetch_add(1, Ordering::Relaxed) + 1;
    crate::phase_telemetry::attach(
        "tree_hash_files",
        serde_json::json!({
            "hashes": hashes,
            "tracked_entries": tracked,
            "untracked_files": untracked,
        }),
    );
}

/// Completion evidence is separate from the hash result: a failed operation
/// can still prove that its repository-selected child processes were stopped.
#[derive(Debug)]
pub(crate) struct WorkingTreeHashObservation {
    pub(crate) result: Result<String, GitError>,
    pub(crate) writers_quiescent: bool,
}

/// A handle on one git checkout (the main repository or a linked
/// worktree). Constructed via [`GitRepo::discover`].
pub struct GitRepo {
    /// Top level of *this* checkout.
    root: PathBuf,
}

/// The shape of one entry in a complete Git ignored-path listing.
///
/// `DirectoryRoot` only proves that Git returned a collapsed, real directory
/// root outside broker-owned paths. It is a discovery result, not an
/// authorization to remove the directory; liveness, ownership, and fresh
/// resource checks remain separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitIgnoredPathKind {
    DirectoryRoot,
    Protected,
    Other,
}

/// One path from `ls-files --others --ignored --directory -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitIgnoredPathEntry {
    /// Relative path without Git's trailing slash marker.
    pub(crate) relative: String,
    /// Whether Git marked the record as a collapsed directory.
    pub(crate) directory_marker: bool,
    /// A conservative path shape classification for a future in-place
    /// artifact lane; whole-checkout cleanup intentionally keeps its existing
    /// classifier. This is never a deletion verdict.
    #[allow(dead_code)]
    pub(crate) kind: GitIgnoredPathKind,
}

/// One entry from Git's authoritative worktree list output. Optional lock and
/// prune reasons are separate from their flags because Git permits reason-free
/// as well as reason-bearing forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeInfo {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked: bool,
    pub lock_reason: Option<String>,
    pub prunable: bool,
    pub prunable_reason: Option<String>,
}

/// One leaf in an exact committed tree. Used when a recovery path must prove
/// that surviving tracked files still match the pinned commit after a linked
/// worktree's own Git metadata has disappeared.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitTreeFile {
    pub(crate) path: String,
    pub(crate) mode: String,
    pub(crate) object_id: String,
}

/// One effective git config entry with the scope and file that set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    pub scope: String,
    pub origin: String,
    pub value: String,
}

/// Per-process memory of "which repository is this path in", enabled only by the
/// one-shot CLI: one command asked Git the same two questions 28 times. Every
/// hit re-checks the checkout's `.git` entry, so a removed or replaced checkout
/// asks Git again rather than getting an old answer.
mod repository_identity_cache {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};

    static ENABLED: AtomicBool = AtomicBool::new(false);

    /// A main checkout's `.git` directory changes constantly as Git works, so it
    /// is compared by identity alone; a worktree's `.git` file also by content.
    #[derive(Clone, PartialEq, Eq)]
    struct GitEntry {
        file_type: std::fs::FileType,
        identity: (u64, u64),
        file: Option<(Option<std::time::SystemTime>, u64)>,
    }

    fn git_entry(root: &Path) -> Option<GitEntry> {
        let metadata = std::fs::symlink_metadata(root.join(".git")).ok()?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino())
        };
        #[cfg(not(unix))]
        let identity = (0, 0);
        Some(GitEntry {
            file_type: metadata.file_type(),
            identity,
            file: (!metadata.is_dir()).then(|| (metadata.modified().ok(), metadata.len())),
        })
    }

    /// Git reads these from the environment, and they change the answers.
    pub(super) fn environment_allows_memory() -> bool {
        std::env::var_os("GIT_DIR").is_none()
            && std::env::var_os("GIT_WORK_TREE").is_none()
            && std::env::var_os("GIT_COMMON_DIR").is_none()
    }

    #[derive(Default)]
    pub(super) struct Memory {
        roots: HashMap<PathBuf, (PathBuf, GitEntry)>,
        common_dirs: HashMap<PathBuf, (PathBuf, GitEntry)>,
    }

    impl Memory {
        pub(super) fn root_for(&self, path: &Path) -> Option<PathBuf> {
            let (root, entry) = self.roots.get(path)?;
            // A repository created inside the path since would change the answer.
            let same_checkout = root == path || !path.join(".git").exists();
            (same_checkout && git_entry(root).as_ref() == Some(entry)).then(|| root.clone())
        }

        pub(super) fn remember_root(&mut self, path: &Path, root: &Path) {
            if let Some(entry) = git_entry(root) {
                self.roots
                    .insert(path.to_path_buf(), (root.to_path_buf(), entry));
            }
        }

        pub(super) fn common_dir_for(&self, root: &Path) -> Option<PathBuf> {
            let (common, entry) = self.common_dirs.get(root)?;
            (git_entry(root).as_ref() == Some(entry) && common.is_dir()).then(|| common.clone())
        }

        pub(super) fn remember_common_dir(&mut self, root: &Path, common: &Path) {
            if let Some(entry) = git_entry(root) {
                self.common_dirs
                    .insert(root.to_path_buf(), (common.to_path_buf(), entry));
            }
        }
    }

    #[cfg(test)]
    thread_local! {
        /// Lets one test use the memory without switching it on for the
        /// other tests that share its process.
        pub(super) static ENABLED_ON_THIS_THREAD: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
    }

    fn enabled() -> bool {
        #[cfg(test)]
        if ENABLED_ON_THIS_THREAD.with(std::cell::Cell::get) {
            return true;
        }
        ENABLED.load(Ordering::Relaxed)
    }

    fn with_memory<T>(use_memory: impl FnOnce(&mut Memory) -> T) -> Option<T> {
        if !enabled() || !environment_allows_memory() {
            return None;
        }
        static MEMORY: OnceLock<Mutex<Memory>> = OnceLock::new();
        let mut memory = MEMORY.get_or_init(Mutex::default).lock().ok()?;
        Some(use_memory(&mut memory))
    }

    pub fn enable() {
        ENABLED.store(true, Ordering::Relaxed);
    }

    pub(super) fn root_for(path: &Path) -> Option<PathBuf> {
        with_memory(|memory| memory.root_for(path)).flatten()
    }

    pub(super) fn remember_root(path: &Path, root: &Path) {
        with_memory(|memory| memory.remember_root(path, root));
    }

    pub(super) fn common_dir_for(root: &Path) -> Option<PathBuf> {
        with_memory(|memory| memory.common_dir_for(root)).flatten()
    }

    pub(super) fn remember_common_dir(root: &Path, common: &Path) {
        with_memory(|memory| memory.remember_common_dir(root, common));
    }

    #[cfg(test)]
    mod tests {
        use super::Memory;

        #[test]
        fn a_changed_or_removed_checkout_is_not_answered_from_memory() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("checkout");
            let nested = root.join("src");
            std::fs::create_dir_all(&nested).unwrap();
            let git_file = root.join(".git");
            let common = tmp.path().join("common");
            std::fs::create_dir(&common).unwrap();
            std::fs::write(&git_file, "gitdir: one\n").unwrap();

            let mut memory = Memory::default();
            memory.remember_root(&nested, &root);
            memory.remember_common_dir(&root, &common);
            assert_eq!(memory.root_for(&nested).as_deref(), Some(root.as_path()));
            assert_eq!(
                memory.common_dir_for(&root).as_deref(),
                Some(common.as_path())
            );

            std::fs::create_dir(nested.join(".git")).unwrap();
            assert_eq!(
                memory.root_for(&nested),
                None,
                "a repository appeared inside"
            );
            std::fs::remove_dir(nested.join(".git")).unwrap();

            std::fs::write(&git_file, "gitdir: a longer replacement\n").unwrap();
            assert_eq!(memory.root_for(&nested), None, "the checkout was replaced");
            assert_eq!(memory.common_dir_for(&root), None);

            memory.remember_common_dir(&root, &common);
            std::fs::remove_dir(&common).unwrap();
            assert_eq!(memory.common_dir_for(&root), None, "common dir removed");

            memory.remember_root(&nested, &root);
            std::fs::remove_file(&git_file).unwrap();
            assert_eq!(memory.root_for(&nested), None, "the checkout was removed");
        }
    }
}

/// For a one-shot command that owns its process; see the module above.
pub use repository_identity_cache::enable as enable_repository_identity_memory;

impl GitRepo {
    /// Discover the repository containing `path`.
    pub fn discover(path: &Path) -> Result<Self, GitError> {
        if let Some(root) = repository_identity_cache::root_for(path) {
            return Ok(Self { root });
        }
        let root =
            run_git(path, &["rev-parse", "--show-toplevel"]).map_err(|error| match error {
                GitError::Git { args, stderr }
                    if args == "rev-parse --show-toplevel"
                        && stderr.to_ascii_lowercase().contains("not a git repository") =>
                {
                    GitError::NotARepository {
                        path: path.to_path_buf(),
                    }
                }
                // A timeout, spawn failure, or unrelated Git error says nothing
                // about whether the path belongs to a repository. Preserve the
                // original invocation and failure instead of disguising it as an
                // invalid checkout.
                other => other,
            })?;
        let root = PathBuf::from(root);
        repository_identity_cache::remember_root(path, &root);
        Ok(Self { root })
    }

    /// Gates cannot infer all tracked inputs from a command, so refuse to
    /// certify a checkout whose index omits any tracked paths.
    pub(crate) fn require_complete_gate_checkout(&self) -> Result<(), GitError> {
        let output = run_git_bytes(&self.root, &["ls-files", "-t", "-z"])?;
        for entry in nul_terminated_paths(&output, "ls-files -t")? {
            if entry.len() < 3 || entry[1] != b' ' {
                return Err(GitError::UntrustedOutput {
                    detail: "git ls-files -t returned a malformed index entry".into(),
                });
            }
            if entry[0] == b'S' {
                return Err(GitError::UntrustedOutput {
                    detail: format!(
                        "gates cannot verify a sparse checkout with omitted tracked paths (for example {:?}); use a complete checkout for verification",
                        String::from_utf8_lossy(&entry[2..])
                    ),
                });
            }
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path of the repository's **common** git dir — the one
    /// directory every linked worktree shares. Installed git hooks live
    /// in `<common>/hooks`, which is what makes them worktree-wide.
    pub fn git_common_dir(&self) -> Result<PathBuf, GitError> {
        if let Some(common) = repository_identity_cache::common_dir_for(&self.root) {
            return Ok(common);
        }
        let common = run_git(&self.root, &["rev-parse", "--git-common-dir"])?;
        let common = Self::absolute_common_dir(&self.root, &common);
        repository_identity_cache::remember_common_dir(&self.root, &common);
        Ok(common)
    }

    /// Resolve a checkout's common directory without a separate root lookup.
    /// The worktree bit is required: a bare repository also has a common
    /// directory, but cannot supply the checkout evidence ownership needs.
    ///
    /// A status of 373 sessions asked this 850 times on 2026-11-12, one Git
    /// process each, which was most of its time. Two answers need no Git: one
    /// already given in this process (by Git or by a registration) for a
    /// checkout whose `.git` entry is unchanged, and a linked worktree's own
    /// registration, which names the common directory in files. Anything else,
    /// and any registration that is not consistent in both directions, is
    /// still put to Git. An expired deadline reaches Git and fails there.
    pub(crate) fn worktree_common_dir(path: &Path) -> Result<PathBuf, GitError> {
        let deadline_open = active_git_deadline().is_none_or(|deadline| deadline > Instant::now());
        if deadline_open && let Some(common) = repository_identity_cache::common_dir_for(path) {
            crate::phase_telemetry::add("worktree_common_dir_remembered", Duration::ZERO);
            return Ok(common);
        }
        if deadline_open && let Some(common) = Self::registered_worktree_common_dir(path) {
            crate::phase_telemetry::add("worktree_common_dir_registered", Duration::ZERO);
            // One status read the same registration about 1,600 times; the
            // memory re-checks only the `.git` file and the common directory.
            repository_identity_cache::remember_common_dir(path, &common);
            return Ok(common);
        }
        let _span = crate::phase_telemetry::span("worktree_common_dir_probe");
        let args = ["rev-parse", "--is-inside-work-tree", "--git-common-dir"];
        let output = run_git(path, &args)?;
        let Some(("true", common)) = output.split_once('\n') else {
            return Err(GitError::Git {
                args: args.join(" "),
                stderr: "cannot establish a Git worktree and common directory".into(),
            });
        };
        if common.is_empty() {
            return Err(GitError::Git {
                args: args.join(" "),
                stderr: "Git returned an empty common directory".into(),
            });
        }
        let common = Self::absolute_common_dir(path, common);
        repository_identity_cache::remember_common_dir(path, &common);
        Ok(common)
    }

    /// The common directory a linked worktree's registration names, when the
    /// registration is complete: `<path>/.git` points at a Git directory that
    /// has a `HEAD`, points back at this `.git` file and names a common
    /// directory that exists. `None` leaves the question to Git; that covers
    /// main checkouts, paths inside a checkout, a `GIT_DIR`-style environment
    /// and every registration that is damaged or half removed.
    pub(crate) fn registered_worktree_common_dir(path: &Path) -> Option<PathBuf> {
        const POINTER_LIMIT: u64 = 4096;
        fn pointer(file: &Path) -> Option<String> {
            let metadata = std::fs::symlink_metadata(file).ok()?;
            if !metadata.is_file() || metadata.len() > POINTER_LIMIT {
                return None;
            }
            let text = std::fs::read_to_string(file).ok()?;
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        if !repository_identity_cache::environment_allows_memory() {
            return None;
        }
        let git_file = path.join(".git");
        let git_dir = pointer(&git_file)?;
        let git_dir = path.join(git_dir.strip_prefix("gitdir:")?.trim());
        if !git_dir.join("HEAD").is_file() {
            return None;
        }
        let back = PathBuf::from(pointer(&git_dir.join("gitdir"))?);
        if back.canonicalize().ok()? != git_file.canonicalize().ok()? {
            return None;
        }
        let common = git_dir
            .join(pointer(&git_dir.join("commondir"))?)
            .canonicalize()
            .ok()?;
        (common.join("HEAD").is_file() && common.join("objects").is_dir()).then_some(common)
    }

    fn absolute_common_dir(path: &Path, common: &str) -> PathBuf {
        let mut common_dir = PathBuf::from(common);
        if common_dir.is_relative() {
            common_dir = path.join(common_dir);
        }
        // Canonicalize to strip the `..` segments git emits from linked
        // worktrees.
        common_dir.canonicalize().unwrap_or(common_dir)
    }

    /// Capture the filesystem identity tuple for this linked-worktree instance. It is
    /// suitable as spawn-time cleanup provenance only when captured from the
    /// `GitRepo` returned by worktree creation and stored with that session.
    /// Unsupported platforms and ambiguous filesystem state return `None`;
    /// callers must then leave automatic removal disabled for that session.
    pub(crate) fn spawned_worktree_instance_witness(
        &self,
    ) -> Option<crate::events::SpawnedWorktreeInstanceWitness> {
        #[cfg(unix)]
        {
            self.spawned_worktree_instance_witness_unix()
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    #[cfg(unix)]
    fn spawned_worktree_instance_witness_unix(
        &self,
    ) -> Option<crate::events::SpawnedWorktreeInstanceWitness> {
        use std::os::unix::fs::MetadataExt;

        fn directory_identity(path: &Path) -> Option<(u64, u64)> {
            use std::os::unix::fs::MetadataExt;

            let metadata = fs::symlink_metadata(path).ok()?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return None;
            }
            Some((metadata.dev(), metadata.ino()))
        }

        let root = self.root.canonicalize().ok()?;
        let (worktree_device, worktree_inode) = directory_identity(&root)?;
        let pointer_path = root.join(".git");
        let mut pointer_options = fs::OpenOptions::new();
        pointer_options.read(true);
        use std::os::unix::fs::OpenOptionsExt;
        pointer_options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let pointer = pointer_options.open(&pointer_path).ok()?;
        let pointer_metadata = pointer.metadata().ok()?;
        if !pointer_metadata.is_file() {
            return None;
        }
        let git_pointer_device = pointer_metadata.dev();
        let git_pointer_inode = pointer_metadata.ino();
        let mut pointer_bytes = Vec::new();
        pointer.take(4097).read_to_end(&mut pointer_bytes).ok()?;
        if pointer_bytes.len() > 4096 {
            return None;
        }
        let pointer_text = std::str::from_utf8(&pointer_bytes).ok()?;
        let pointed_admin = pointer_text
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))?
            .trim();
        let pointed_admin = PathBuf::from(pointed_admin);
        let pointed_admin = if pointed_admin.is_absolute() {
            pointed_admin
        } else {
            root.join(pointed_admin)
        };
        let pointed_admin_metadata = fs::symlink_metadata(&pointed_admin).ok()?;
        if pointed_admin_metadata.file_type().is_symlink() || !pointed_admin_metadata.is_dir() {
            return None;
        }
        let pointer_admin = pointed_admin.canonicalize().ok()?;
        let git_admin = PathBuf::from(
            run_git(&root, &["rev-parse", "--absolute-git-dir"])
                .ok()?
                .trim(),
        )
        .canonicalize()
        .ok()?;
        if pointer_admin != git_admin {
            return None;
        }
        let (git_admin_device, git_admin_inode) = directory_identity(&git_admin)?;

        let git_common = self.git_common_dir().ok()?.canonicalize().ok()?;
        let (git_common_device, git_common_inode) = directory_identity(&git_common)?;
        if git_admin == git_common {
            return None;
        }
        let (generation_device, generation_inode, root_generation_sha256) =
            crate::root_generation::read_path_with_identity(&root)?;
        if (generation_device, generation_inode) != (worktree_device, worktree_inode) {
            return None;
        }
        #[cfg(target_os = "macos")]
        let volume_uuids = (|| {
            use crate::worktree_registry::stable_volume_uuid_for_object as volume;
            Some(crate::events::SpawnedWorktreeVolumeUuids {
                worktree: volume(&root, worktree_device, worktree_inode).ok()?,
                git_pointer: volume(&pointer_path, git_pointer_device, git_pointer_inode).ok()?,
                git_admin: volume(&git_admin, git_admin_device, git_admin_inode).ok()?,
                git_common: volume(&git_common, git_common_device, git_common_inode).ok()?,
            })
        })();
        #[cfg(not(target_os = "macos"))]
        let volume_uuids = None;
        let root_metadata = fs::symlink_metadata(&root).ok()?;
        if root_metadata.file_type().is_symlink()
            || !root_metadata.is_dir()
            || (root_metadata.dev(), root_metadata.ino()) != (worktree_device, worktree_inode)
        {
            return None;
        }

        Some(crate::events::SpawnedWorktreeInstanceWitness {
            schema_version: if volume_uuids.is_some() { 3 } else { 2 },
            worktree_path: root.to_str()?.to_string(),
            worktree_device,
            worktree_inode,
            git_pointer_device,
            git_pointer_inode,
            git_admin_path: git_admin.to_str()?.to_string(),
            git_admin_device,
            git_admin_inode,
            git_common_path: git_common.to_str()?.to_string(),
            git_common_device,
            git_common_inode,
            root_generation_sha256: Some(root_generation_sha256),
            volume_uuids,
        })
    }

    /// Capture the exact linked-worktree and Git-admin identities needed to
    /// recognize a finish removal that stopped after deleting tracked files.
    /// Unsupported platforms and ambiguous filesystem state return `None`,
    /// which disables interrupted cleanup recovery.
    pub(crate) fn finish_cleanup_recovery_proof(
        &self,
        _branch_ref: &str,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        #[cfg(unix)]
        {
            self.finish_cleanup_recovery_proof_unix(_branch_ref)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    #[cfg(unix)]
    fn finish_cleanup_recovery_proof_unix(
        &self,
        branch_ref: &str,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        let branch = branch_ref.strip_prefix("refs/heads/")?;
        if self.current_branch().ok()?.trim() != branch {
            return None;
        }
        let head = self.head_commit().ok()?;
        let branch_tip = self.resolve_ref(branch_ref)?;
        if head != branch_tip {
            return None;
        }
        let witness = self.spawned_worktree_instance_witness()?;

        Some(crate::events::FinishCleanupRecoveryProof {
            schema_version: 3,
            worktree_path: witness.worktree_path,
            worktree_device: witness.worktree_device,
            worktree_inode: witness.worktree_inode,
            git_pointer_device: witness.git_pointer_device,
            git_pointer_inode: witness.git_pointer_inode,
            git_admin_path: witness.git_admin_path,
            git_admin_device: witness.git_admin_device,
            git_admin_inode: witness.git_admin_inode,
            git_common_path: witness.git_common_path,
            git_common_device: witness.git_common_device,
            git_common_inode: witness.git_common_inode,
            branch_ref: branch_ref.to_string(),
            head,
            branch_tip,
            root_generation_sha256: witness.root_generation_sha256,
            disposable_ignored_root_prefixes: Vec::new(),
            volume_uuids: witness.volume_uuids,
            ownership_reproof: None,
        })
    }

    /// Root of the **main** checkout, even when `self` is a linked
    /// worktree. Broker state (`.aethyme/broker.db`) always lives here so
    /// all worktrees coordinate through one database.
    pub fn main_root(&self) -> Result<PathBuf, GitError> {
        // `<main>/.git` → `<main>`.
        let common_dir = self.git_common_dir()?;
        Ok(common_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(common_dir))
    }

    /// Index entries (mode, blob id, stage, path) under `pathspec`, NUL
    /// separated, from `git ls-files -s -z`. Read-only; content-addressed, so
    /// two checkouts holding the same files answer the same bytes.
    pub(crate) fn index_entries_under(&self, pathspec: &str) -> Result<Vec<u8>, GitError> {
        run_git_bytes(&self.root, &["ls-files", "-s", "-z", "--", pathspec])
    }

    /// Working-tree changes under `pathspec` (untracked files included), from
    /// `git status --porcelain=v1 -z`. Read-only: optional locks are off.
    pub(crate) fn status_entries_under(&self, pathspec: &str) -> Result<Vec<u8>, GitError> {
        run_git_bytes(
            &self.root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--",
                pathspec,
            ],
        )
    }

    pub fn current_branch(&self) -> Result<String, GitError> {
        run_git(&self.root, &["rev-parse", "--abbrev-ref", "HEAD"])
    }

    /// Resolve a symbolic ref without falling back to the checked-out HEAD.
    pub fn symbolic_ref(&self, name: &str) -> Option<String> {
        run_git(&self.root, &["symbolic-ref", "--quiet", name])
            .ok()
            .filter(|value| !value.is_empty())
    }

    /// Configured tracking ref and its currently fetched commit. This is
    /// read-only: status and reconciliation never perform an implicit fetch.
    pub fn tracking_upstream(&self) -> Option<(String, String)> {
        let upstream = run_git(
            &self.root,
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
        )
        .ok()?;
        let commit = self.resolve_ref(&upstream)?;
        Some((upstream, commit))
    }

    /// Whether remote-default configuration exists even when its fetched ref
    /// is missing. Unlike `tracking_upstream`, this needs no upstream object.
    pub(crate) fn has_configured_remote_default(&self) -> Result<bool, GitError> {
        if self.symbolic_ref("refs/remotes/origin/HEAD").is_some() {
            return Ok(true);
        }
        let branch = self.current_branch()?;
        let reference = format!("refs/heads/{branch}");
        let upstream = run_git(
            &self.root,
            &[
                "for-each-ref",
                "--count=1",
                "--format=%(upstream)",
                &reference,
            ],
        )?;
        Ok(upstream.starts_with("refs/remotes/"))
    }

    /// The fetched default branch, as `(<remote>/<branch>, commit)`.
    ///
    /// Prefers `refs/remotes/origin/HEAD`, which names the remote's default
    /// branch whatever the main checkout has checked out. `@{upstream}` alone
    /// answers for the *current* branch, so a main checkout left on a feature
    /// branch measured every start base against that feature branch. Falls
    /// back to `@{upstream}` when `origin/HEAD` is not set. Read-only: this is
    /// as fresh as the last fetch, and never fetches.
    pub fn upstream_default(&self) -> Option<(String, String)> {
        if let Some(target) = self.symbolic_ref("refs/remotes/origin/HEAD")
            && let Some(commit) = self.resolve_ref(&target)
        {
            let short = target
                .strip_prefix("refs/remotes/")
                .unwrap_or(&target)
                .to_string();
            return Some((short, commit));
        }
        self.tracking_upstream()
    }

    /// The fetched default branch for policy reads, distinguishing a proven
    /// absent ref from Git errors. A configured but unresolvable default or
    /// upstream is not treated as permission to fall back to working-copy
    /// policy.
    pub(crate) fn upstream_default_checked(&self) -> Result<Option<(String, String)>, GitError> {
        if let Some(found) = self.origin_head_default_checked()? {
            return Ok(Some(found));
        }

        let Some(branch_ref) = self.symbolic_ref_checked("HEAD")? else {
            return Ok(None);
        };
        let Some(branch) = branch_ref.strip_prefix("refs/heads/") else {
            return Err(GitError::UntrustedOutput {
                detail: "the checked-out branch ref is not under refs/heads".into(),
            });
        };
        let branch_ref = format!("refs/heads/{branch}");
        let upstream_ref = run_git(
            &self.root,
            &[
                "for-each-ref",
                "--count=1",
                "--format=%(upstream)",
                &branch_ref,
            ],
        )?;
        if upstream_ref.is_empty() {
            return Ok(None);
        }
        let upstream_name = run_git(
            &self.root,
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
        )?;
        if upstream_name.is_empty() {
            return Err(GitError::UntrustedOutput {
                detail: "Git reported a configured upstream without its name".into(),
            });
        }
        let commit = self.resolve_commit_checked(&upstream_ref)?;
        Ok(Some((upstream_name, commit)))
    }

    /// The default branch `refs/remotes/origin/HEAD` names, as
    /// `(origin/<branch>, commit)`, with no fallback: `Ok(None)` when
    /// `origin/HEAD` is absent. For policy that only the remote's default
    /// branch may supply, never the checked-out branch's upstream. A target
    /// outside `refs/remotes/origin/`, or an `origin/HEAD` that is not
    /// symbolic, is an error.
    pub(crate) fn origin_head_default_checked(&self) -> Result<Option<(String, String)>, GitError> {
        const DEFAULT_REF: &str = "refs/remotes/origin/HEAD";
        let listing = self.run_git_output_limited(
            &[
                "for-each-ref",
                "--count=1",
                "--format=%(refname)%00%(symref)%00%(objecttype)%00%(objectname)",
                DEFAULT_REF,
            ],
            4096,
        )?;
        // Git can omit a dangling symbolic ref. Keep the checked slow
        // path for absence, unusual targets and malformed evidence.
        if listing.stderr.is_empty()
            && let Ok(row) = std::str::from_utf8(&listing.stdout)
            && let Some(row) = row.strip_suffix('\n')
            && let [DEFAULT_REF, target, "commit", oid] =
                row.split('\0').collect::<Vec<_>>().as_slice()
            && !target.contains('\n')
            && let Some(short) = target.strip_prefix("refs/remotes/")
            && short.starts_with("origin/")
            && valid_git_object_id(oid.as_bytes())
        {
            return Ok(Some((short.to_string(), oid.to_string())));
        }
        let default_ref = self.symbolic_ref_checked(DEFAULT_REF)?;
        if let Some(target) = default_ref {
            if !target.starts_with("refs/remotes/origin/") {
                return Err(GitError::UntrustedOutput {
                    detail: "origin/HEAD does not target an origin remote-tracking branch".into(),
                });
            }
            let commit = self.resolve_commit_checked(&target)?;
            let short = target
                .strip_prefix("refs/remotes/")
                .unwrap_or(&target)
                .to_string();
            return Ok(Some((short, commit)));
        }
        if !listing.stdout.is_empty() || !listing.stderr.is_empty() {
            return Err(GitError::UntrustedOutput {
                detail: "refs/remotes/origin/HEAD exists but is not a symbolic default ref".into(),
            });
        }
        Ok(None)
    }

    fn symbolic_ref_checked(&self, name: &str) -> Result<Option<String>, GitError> {
        let args = ["symbolic-ref", "--quiet", name];
        let mut command = git_command();
        command
            .args(args)
            .current_dir(&self.root)
            .env("GIT_OPTIONAL_LOCKS", "0");
        let output = run_git_process_output(command, &args, git_timeout())?;
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_string();
            if value.is_empty() {
                return Err(GitError::UntrustedOutput {
                    detail: format!("git {} returned an empty symbolic ref", args.join(" ")),
                });
            }
            return Ok(Some(value));
        }
        if output.status.code() == Some(1) && output.stderr.is_empty() {
            return Ok(None);
        }
        Err(GitError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }

    fn resolve_commit_checked(&self, reference: &str) -> Result<String, GitError> {
        let revision = format!("{reference}^{{commit}}");
        let value = run_git(&self.root, &["rev-parse", "--verify", "--quiet", &revision])?;
        if value.is_empty() {
            return Err(GitError::UntrustedOutput {
                detail: format!("Git resolved {reference:?} to an empty commit"),
            });
        }
        Ok(value)
    }

    pub fn head_commit(&self) -> Result<String, GitError> {
        run_git(&self.root, &["rev-parse", "HEAD"])
    }

    /// Whether one repository-relative path exists in a local commit/ref.
    /// This never fetches and is suitable for read-only certification.
    pub fn path_exists_at(&self, revision: &str, path: &str) -> Result<bool, GitError> {
        let object = format!("{revision}:{path}");
        let output = git_command()
            .args(["cat-file", "-e", &object])
            .current_dir(&self.root)
            .output()
            .map_err(|source| GitError::Spawn {
                args: format!("cat-file -e {object}"),
                source,
            })?;
        Ok(output.status.success())
    }

    /// Resolve a ref (branch name, tag, ...) to a commit, `None` when it
    /// does not exist.
    /// Common ancestor of `a` and `b`. Re-added 2026-07-17: removed as
    /// dead code the same day #41 made it live (session_change_base
    /// derives lease baselines from merge-base(HEAD, integration)).
    pub fn merge_base(&self, a: &str, b: &str) -> Result<String, GitError> {
        Ok(run_git(&self.root, &["merge-base", a, b])?
            .trim()
            .to_string())
    }

    /// Whether this repository is a shallow clone. History before the
    /// shallow boundary is missing locally, so `merge_base` fails without a
    /// message for any commit older than it.
    pub fn is_shallow(&self) -> bool {
        run_git(&self.root, &["rev-parse", "--is-shallow-repository"])
            .is_ok_and(|output| output.trim() == "true")
    }

    /// The exact owned-DAG path must not derive ancestry from shallow
    /// boundaries, replacement refs or grafts. Errors inspecting these
    /// controls propagate rather than proving an absent history override.
    pub(crate) fn has_complete_unmodified_history(&self) -> Result<bool, GitError> {
        if ["GIT_REPLACE_REF_BASE", "GIT_GRAFT_FILE", "GIT_SHALLOW_FILE"]
            .iter()
            .any(|name| std::env::var_os(name).is_some())
        {
            return Ok(false);
        }
        if run_git(&self.root, &["rev-parse", "--is-shallow-repository"])? != "false"
            || !run_git(
                &self.root,
                &["for-each-ref", "--format=%(refname)", "refs/replace/"],
            )?
            .is_empty()
        {
            return Ok(false);
        }
        let grafts = self.git_common_dir()?.join("info/grafts");
        match std::fs::symlink_metadata(&grafts) {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(source) => Err(GitError::Spawn {
                args: format!("inspect history grafts {}", grafts.display()),
                source,
            }),
        }
    }

    /// A git config value (`git config --get`), `None` when unset.
    pub fn config_get(&self, key: &str) -> Option<String> {
        run_git(&self.root, &["config", "--get", key])
            .ok()
            .filter(|value| !value.is_empty())
    }

    /// A path-typed git config value with where it is set: the effective
    /// entry's scope (`local`, `global`, `worktree`, `system`, `command`),
    /// its origin (`file:<path>`), and the value with `~/` expanded as git
    /// expands it. `None` when unset.
    pub fn config_get_path_with_origin(&self, key: &str) -> Option<ConfigEntry> {
        let line = run_git(
            &self.root,
            &[
                "config",
                "--show-scope",
                "--show-origin",
                "--type=path",
                "--get",
                key,
            ],
        )
        .ok()?;
        let mut fields = line.splitn(3, '\t');
        let scope = fields.next()?.to_string();
        let origin = fields.next()?.to_string();
        let value = fields.next()?.to_string();
        (!value.is_empty()).then_some(ConfigEntry {
            scope,
            origin,
            value,
        })
    }

    /// Configured URL for `remote`.
    pub fn remote_url(&self, remote: &str) -> Result<String, GitError> {
        run_git(&self.root, &["remote", "get-url", remote])
    }

    /// Configured push URL for `remote`, after Git's URL rewrite rules.
    pub fn remote_push_url(&self, remote: &str) -> Result<String, GitError> {
        run_git(&self.root, &["remote", "get-url", "--push", remote])
    }

    /// Every configured push URL for `remote`, after Git's URL rewrite rules.
    ///
    /// A remote may have several `pushurl` entries. Callers coordinating a
    /// publication must inspect the complete set rather than inheriting Git's
    /// multi-destination behavior accidentally.
    pub fn remote_push_urls(&self, remote: &str) -> Result<Vec<String>, GitError> {
        Ok(run_git(
            &self.root,
            &["remote", "get-url", "--push", "--all", remote],
        )?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
    }

    /// Configured remote names in Git's deterministic display order.
    pub fn remotes(&self) -> Result<Vec<String>, GitError> {
        Ok(run_git(&self.root, &["remote"])?
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Resolve a configured remote to one credential-free coordination target.
    pub fn resolve_remote_target(
        &self,
        remote: &str,
        caller_assertion: Option<&str>,
    ) -> Result<crate::ResolvedRemoteTarget, crate::RemoteTargetError> {
        crate::remote_target::resolve_remote_target(self, remote, caller_assertion)
    }

    /// Resolve the one configured remote targeted by a Git command.
    pub fn resolve_remote_command_target(
        &self,
        args: &[String],
        caller_assertion: Option<&str>,
    ) -> Result<crate::ResolvedRemoteTarget, crate::RemoteTargetError> {
        crate::remote_target::resolve_remote_command_target(self, args, caller_assertion)
    }

    /// Resolve one explicit push source expression to the exact object ID Git
    /// would advertise to the remote. The object is not peeled: annotated tag
    /// refs must compare against their tag object, not the tagged commit.
    pub fn resolve_push_source(&self, source: &str) -> Result<String, GitError> {
        run_git(
            &self.root,
            &["rev-parse", "--verify", &format!("{source}^{{object}}")],
        )
    }

    /// Fully-qualified ref name a revision denotes, e.g. `main` or `HEAD` ->
    /// `refs/heads/main`. Returns `None` when the revision is not a ref (a raw
    /// SHA), is ambiguous, or does not resolve.
    ///
    /// Used to make a push refspec written without an explicit destination
    /// planable. Git pushes such a refspec to the ref of the same name on the
    /// remote, so the destination is derivable locally and does not need to be
    /// guessed from the command line.
    pub fn full_ref_name(&self, revision: &str) -> Option<String> {
        let name = run_git(
            &self.root,
            &[
                "rev-parse",
                "--symbolic-full-name",
                "--verify",
                "--quiet",
                revision,
            ],
        )
        .ok()?;
        let name = name.trim();
        // A raw SHA resolves to itself rather than a ref name; only a real ref
        // yields a destination we may push to.
        (!name.is_empty() && name.starts_with("refs/")).then(|| name.to_string())
    }

    /// Whether `treeish` has an entry at `path` (a file or a directory):
    /// one `ls-tree` lookup, no checkout read.
    pub(crate) fn tree_has_path(&self, treeish: &str, path: &str) -> Result<bool, GitError> {
        let listed = run_git(&self.root, &["ls-tree", "--name-only", treeish, "--", path])?;
        Ok(!listed.trim().is_empty())
    }

    /// Whether the index or the working tree holds any path under `path`
    /// that Git would hash: tracked entries and untracked, non-ignored files.
    pub(crate) fn index_or_untracked_under(&self, path: &str) -> Result<bool, GitError> {
        let listed = run_git(
            &self.root,
            &[
                "ls-files",
                "-c",
                "-o",
                "--exclude-standard",
                "-z",
                "--",
                path,
            ],
        )?;
        Ok(!listed.trim_matches('\0').trim().is_empty())
    }

    /// Whether `git diff --quiet` (with `--cached` when `staged`) reports a
    /// difference: staged changes against HEAD, or unstaged changes against
    /// the index. Never refreshes the index (`--no-optional-locks`), whose
    /// modification time is session activity. Exit 1 is "differs"; any
    /// other failure is an error, never "clean".
    pub(crate) fn has_tracked_changes(&self, staged: bool) -> Result<bool, GitError> {
        let mut args = vec![
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "diff",
            "--quiet",
            "--ignore-submodules=none",
        ];
        if staged {
            args.push("--cached");
        }
        let mut command = git_command();
        command
            .args(&args)
            .current_dir(&self.root)
            .env("GIT_OPTIONAL_LOCKS", "0");
        let output = run_git_process_output(command, &args, git_timeout())?;
        match output.status.code() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(GitError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            }),
        }
    }

    /// The subject of every stash entry (`stash list --format=%gs`), such as
    /// `WIP on <branch>: ...` or `On <branch>: ...`.
    pub(crate) fn stash_subjects(&self) -> Result<Vec<String>, GitError> {
        Ok(run_git(&self.root, &["stash", "list", "--format=%gs"])?
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Whether the working tree holds any untracked file Git does not ignore.
    pub(crate) fn has_untracked_files(&self) -> Result<bool, GitError> {
        let listed = run_git(
            &self.root,
            &[
                "--no-optional-locks",
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
            ],
        )?;
        Ok(!listed.trim_matches('\0').is_empty())
    }

    /// Whether Git ignores `path`, relative to the repository root.
    pub fn path_is_ignored(&self, path: &str) -> bool {
        run_git(&self.root, &["check-ignore", "--quiet", "--", path]).is_ok()
    }

    /// Whether Git ignores `path`, preserving command failures separately
    /// from Git's expected exit status 1 for a path that is not ignored.
    /// Cleanup callers need the distinction: a timeout or malformed checkout
    /// must leave discovery incomplete instead of treating the path as safe.
    pub(crate) fn path_is_ignored_checked(&self, path: &str) -> Result<bool, GitError> {
        let args = ["check-ignore", "--quiet", "--", path];
        let mut command = git_command();
        command
            .args(args)
            .current_dir(&self.root)
            .env("GIT_OPTIONAL_LOCKS", "0");
        let output = run_git_process_output(command, &args, git_timeout())?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(GitError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            }),
        }
    }

    /// Validate one fully-qualified destination ref with Git's own grammar.
    pub fn validate_push_destination(&self, destination: &str) -> Result<(), GitError> {
        run_git(&self.root, &["check-ref-format", destination]).map(|_| ())
    }

    /// Observe exact remote refs without updating local tracking refs.
    ///
    /// A successful query that omits a requested ref proves that ref is
    /// absent. Any command, parse, duplicate, or unexpected-ref failure makes
    /// the complete observation unavailable; callers must not classify from a
    /// partial parse.
    pub fn remote_ref_oids(
        &self,
        remote: &str,
        destinations: &[String],
    ) -> Result<BTreeMap<String, Option<String>>, GitError> {
        self.observe_remote_ref_oids(remote, destinations, true, None)
    }

    /// As [`Self::remote_ref_oids`], and read one `advisory` ref in the same
    /// `ls-remote`: one more pattern on the call the caller makes anyway, so
    /// it costs no extra round trip.
    ///
    /// The advisory ref can never weaken the evidence for `destinations`.
    /// Their map is built exactly as [`Self::remote_ref_oids`] builds it; a
    /// line only the advisory pattern matched (Git matches patterns by path
    /// tail) is set aside rather than failing the read, and an advisory ref
    /// that is itself a destination is not asked for twice. The advisory
    /// value is `None` when it is ambiguous, `Some(None)` when the remote
    /// does not have it, and `Some(Some(oid))` otherwise.
    pub(crate) fn remote_ref_oids_with_advisory(
        &self,
        remote: &str,
        destinations: &[String],
        advisory: &str,
    ) -> Result<RemoteRefObservation, GitError> {
        if destinations
            .iter()
            .any(|destination| destination == advisory)
        {
            let refs = self.observe_remote_ref_oids(remote, destinations, true, None)?;
            let advisory = refs.get(advisory).cloned();
            return Ok(RemoteRefObservation { refs, advisory });
        }
        self.observe_remote_ref_oids_with_advisory(remote, destinations, true, None, Some(advisory))
    }

    /// Bind the endpoint actually queried to the original operation identity.
    pub(crate) fn checked_push_remote_ref_oids(
        &self,
        remote: &str,
        recorded_repository: &str,
        destinations: &[String],
    ) -> Result<BTreeMap<String, Option<String>>, GitError> {
        self.observe_remote_ref_oids(remote, destinations, true, Some(recorded_repository))
    }

    /// Observe the configured fetch endpoint through its remote alias. Push
    /// observations instead capture the verified push URL and query it in an
    /// isolated Gitdir with URL rewrites removed. Callers observing publication
    /// must first establish equal fetch/push URLs.
    pub(crate) fn remote_fetch_ref_oids(
        &self,
        remote: &str,
        destinations: &[String],
    ) -> Result<BTreeMap<String, Option<String>>, GitError> {
        self.observe_remote_ref_oids(remote, destinations, false, None)
    }

    fn observe_remote_ref_oids(
        &self,
        remote: &str,
        destinations: &[String],
        push_endpoint: bool,
        recorded_repository: Option<&str>,
    ) -> Result<BTreeMap<String, Option<String>>, GitError> {
        self.observe_remote_ref_oids_with_advisory(
            remote,
            destinations,
            push_endpoint,
            recorded_repository,
            None,
        )
        .map(|observation| observation.refs)
    }

    fn observe_remote_ref_oids_with_advisory(
        &self,
        remote: &str,
        destinations: &[String],
        push_endpoint: bool,
        recorded_repository: Option<&str>,
        advisory: Option<&str>,
    ) -> Result<RemoteRefObservation, GitError> {
        let patterns = destinations
            .iter()
            .map(String::as_str)
            .chain(advisory)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let display_args = format!("ls-remote --refs <endpoint> {}", patterns.join(" "));
        let output = if push_endpoint {
            let push_url = match recorded_repository {
                Some(repository) => crate::remote_target::inspection_push_url(
                    self, remote, repository,
                )
                .map_err(|_| GitError::Git {
                    args: "inspect recorded push endpoint".into(),
                    stderr: "push endpoint identity could not be verified".into(),
                })?,
                None => run_git(&self.root, &["remote", "get-url", "--push", remote])?,
            };
            exact_push_endpoint_output(&self.root, remote, &push_url, &patterns, &display_args)?
        } else {
            let mut command = git_command();
            command.args(["ls-remote", "--refs", remote]);
            command.args(&patterns).current_dir(&self.root);
            run_git_process_output(command, &[&display_args], git_timeout())?
        };
        if !output.status.success() {
            return Err(GitError::Git {
                args: display_args,
                stderr: "remote ref evidence command failed".into(),
            });
        }
        let output = String::from_utf8_lossy(&output.stdout);
        let mut observed = destinations
            .iter()
            .cloned()
            .map(|destination| (destination, None))
            .collect::<BTreeMap<_, _>>();
        let mut advisory_value = None;
        let mut advisory_ambiguous = false;
        for line in output.lines() {
            let Some((sha, destination)) = line.split_once('\t') else {
                return Err(GitError::Git {
                    args: display_args.clone(),
                    stderr: "remote ref evidence was not a SHA/ref pair".into(),
                });
            };
            if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(GitError::Git {
                    args: display_args.clone(),
                    stderr: "remote ref evidence contained an invalid object ID".into(),
                });
            }
            let Some(slot) = observed.get_mut(destination) else {
                match advisory_line(advisory, destinations, destination) {
                    AdvisoryLine::Exact => {
                        if advisory_value.replace(sha.to_ascii_lowercase()).is_some() {
                            advisory_ambiguous = true;
                        }
                        continue;
                    }
                    AdvisoryLine::TailOnly => continue,
                    AdvisoryLine::NotAdvisory => {}
                }
                return Err(GitError::Git {
                    args: display_args.clone(),
                    stderr: "remote ref evidence contained an unrequested destination".into(),
                });
            };
            if slot.replace(sha.to_ascii_lowercase()).is_some() {
                return Err(GitError::Git {
                    args: display_args.clone(),
                    stderr: "remote ref evidence repeated one destination".into(),
                });
            }
        }
        Ok(RemoteRefObservation {
            refs: observed,
            advisory: (advisory.is_some() && !advisory_ambiguous).then_some(advisory_value),
        })
    }

    /// Query the remote's advertised HEAD without updating any local ref.
    pub fn remote_default_branch(&self, remote: &str) -> Result<RemoteDefaultBranch, GitError> {
        let output = run_git(&self.root, &["ls-remote", "--symref", remote, "HEAD"])?;
        let ref_name = output
            .lines()
            .find_map(|line| {
                line.strip_prefix("ref: ")
                    .and_then(|rest| rest.split_once('\t'))
                    .filter(|(_, name)| *name == "HEAD")
                    .map(|(name, _)| name.to_string())
            })
            .ok_or_else(|| GitError::Git {
                args: format!("ls-remote --symref {remote} HEAD"),
                stderr: format!("remote {remote:?} did not advertise a symbolic HEAD"),
            })?;
        let sha = output
            .lines()
            .find_map(|line| {
                line.split_once('\t')
                    .filter(|(sha, name)| {
                        *name == "HEAD"
                            && sha.len() == 40
                            && sha.chars().all(|character| character.is_ascii_hexdigit())
                    })
                    .map(|(sha, _)| sha.to_string())
            })
            .ok_or_else(|| GitError::Git {
                args: format!("ls-remote --symref {remote} HEAD"),
                stderr: format!("remote {remote:?} did not advertise a HEAD commit"),
            })?;
        Ok(RemoteDefaultBranch { ref_name, sha })
    }

    /// Resolve two exact refs for one observation. Fall back to Git's
    /// individual commit resolution for odd refs, missing objects or errors.
    pub(crate) fn resolve_commit_ref_pair(
        &self,
        first: &str,
        second: &str,
    ) -> (Option<String>, Option<String>) {
        let args = [
            "for-each-ref",
            "--count=3",
            "--format=%(refname)%00%(objecttype)%00%(objectname)",
            first,
            second,
        ];
        if let Ok(output) = self.run_git_output_limited(&args, 16 * 1024)
            && output.stderr.is_empty()
            && let Ok(text) = std::str::from_utf8(&output.stdout)
            && (text.is_empty() || text.ends_with('\n'))
        {
            let mut pair = (None, None);
            let valid = text.lines().all(|line| {
                let fields: Vec<_> = line.split('\0').collect();
                let [reference, "commit", oid] = fields.as_slice() else {
                    return false;
                };
                let slot = if *reference == first {
                    &mut pair.0
                } else if *reference == second {
                    &mut pair.1
                } else {
                    return false;
                };
                if slot.is_some() || !valid_git_object_id(oid.as_bytes()) {
                    return false;
                }
                *slot = Some(oid.to_string());
                true
            });
            if valid && pair.0.is_some() {
                return pair;
            }
        }
        (self.resolve_ref(first), self.resolve_ref(second))
    }

    pub fn resolve_ref(&self, name: &str) -> Option<String> {
        run_git(
            &self.root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{name}^{{commit}}"),
            ],
        )
        .ok()
    }

    /// Read one repository-relative UTF-8 file from an exact commit without
    /// checking it out or mutating refs, indexes, or worktree state.
    /// The `.aethyme/<stem>` configuration committed at `commit`, in whichever
    /// format it was written (#1223). Two format twins committed together is
    /// [`GitError::Config`], never a silent preference.
    pub fn config_at_commit(
        &self,
        commit: &str,
        stem: &str,
    ) -> Result<Option<aethyme_config::ConfigText>, GitError> {
        aethyme_config::read_config_text_with(aethyme_config::CONFIG_DIR, stem, |relpath| {
            self.file_at_commit(commit, relpath)
        })
    }

    /// [`Self::config_at_commit`] for policy reads. A missing tree entry is
    /// distinct from a Git read failure, so callers cannot silently fall back
    /// to working-copy policy when the committed tree could not be inspected.
    /// Read configuration from a commit OID that was already resolved and
    /// verified by the caller. All format twins are checked in one tree
    /// inspection so a strict policy read does not launch one Git process per
    /// extension.
    pub(crate) fn config_at_resolved_commit_checked(
        &self,
        commit: &str,
        stem: &str,
    ) -> Result<Option<aethyme_config::ConfigText>, GitError> {
        let candidate_paths = aethyme_config::candidate_relpaths(stem);
        let mut loaded: Option<BTreeMap<String, String>> = None;
        aethyme_config::read_config_text_with(aethyme_config::CONFIG_DIR, stem, |relpath| {
            if loaded.is_none() {
                let objects =
                    self.config_tree_entries_at_commit_checked(commit, &candidate_paths)?;
                let present_paths = candidate_paths
                    .iter()
                    .filter(|path| objects.contains_key(*path))
                    .cloned()
                    .collect::<Vec<_>>();
                if present_paths.len() > 1 {
                    return Err(aethyme_config::ConfigSourceError::Ambiguous {
                        stem: stem.to_string(),
                        paths: present_paths.into_iter().map(PathBuf::from).collect(),
                    }
                    .into());
                }
                let files = if let Some(path) = present_paths.first() {
                    let object = objects.get(path).ok_or_else(|| GitError::UntrustedOutput {
                        detail: format!(
                            "git ls-tree omitted the selected configuration path {path:?}"
                        ),
                    })?;
                    BTreeMap::from([(path.clone(), self.config_blob_checked(object, path)?)])
                } else {
                    BTreeMap::new()
                };
                loaded = Some(files);
            }
            Ok(loaded
                .as_ref()
                .and_then(|files| files.get(relpath))
                .cloned())
        })
    }

    fn config_tree_entries_at_commit_checked(
        &self,
        commit: &str,
        paths: &[String],
    ) -> Result<BTreeMap<String, String>, GitError> {
        let mut owned_args = vec![
            "ls-tree".to_string(),
            "-z".to_string(),
            "--full-tree".to_string(),
            commit.to_string(),
            "--".to_string(),
        ];
        for path in paths {
            let path_value = Path::new(path);
            if path_value.is_absolute()
                || path_value.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir | std::path::Component::Prefix(_)
                    )
                })
                || path.contains(':')
                || path.contains('\\')
            {
                return Err(GitError::Git {
                    args: "ls-tree <commit> -- <path>".into(),
                    stderr:
                        "file path must be repository-relative and cannot contain pathspec magic"
                            .into(),
                });
            }
            // Config stems may contain portable glob characters such as
            // brackets. Keep each candidate literal so the tree query cannot
            // match a neighboring path by Git pathspec rules.
            owned_args.push(format!(":(literal){path}"));
        }
        let args = owned_args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = self.run_git_output_limited(&args, 64 * 1024)?;
        let records = Self::strict_config_tree_records(&output.stdout, &args)?;

        let mut objects = BTreeMap::new();
        for record in records {
            let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
                return Err(GitError::UntrustedOutput {
                    detail: format!("git {} returned a malformed tree entry", args.join(" ")),
                });
            };
            let metadata = &record[..tab];
            let recorded_path = &record[tab + 1..];
            let fields = metadata.split(|byte| *byte == b' ').collect::<Vec<_>>();
            if fields.len() != 3 {
                return Err(GitError::UntrustedOutput {
                    detail: format!("git {} returned an unexpected tree entry", args.join(" ")),
                });
            }
            let Some(path) = paths.iter().find(|path| path.as_bytes() == recorded_path) else {
                return Err(GitError::UntrustedOutput {
                    detail: format!(
                        "git {} returned an unexpected configuration path",
                        args.join(" ")
                    ),
                });
            };
            let regular_mode = fields[0] == b"100644" || fields[0] == b"100755";
            if !regular_mode || fields[1] != b"blob" {
                return Err(GitError::UntrustedOutput {
                    detail: format!("committed configuration path {path:?} is not a regular file"),
                });
            }
            let object = std::str::from_utf8(fields[2]).map_err(|_| GitError::UntrustedOutput {
                detail: "git ls-tree returned a non-UTF-8 object id".into(),
            })?;
            if objects.insert(path.clone(), object.to_string()).is_some() {
                return Err(GitError::UntrustedOutput {
                    detail: format!(
                        "git {} returned a duplicate configuration path",
                        args.join(" ")
                    ),
                });
            }
        }
        Ok(objects)
    }

    fn strict_config_tree_records<'a>(
        stdout: &'a [u8],
        args: &[&str],
    ) -> Result<Vec<&'a [u8]>, GitError> {
        if stdout.is_empty() {
            return Ok(Vec::new());
        }
        if stdout.last() != Some(&0) {
            return Err(GitError::UntrustedOutput {
                detail: format!("git {} returned an unterminated tree entry", args.join(" ")),
            });
        }

        // Consume exactly one framing NUL. Any remaining empty record indicates
        // malformed nonempty output, not an absent configuration path.
        let records = stdout[..stdout.len() - 1]
            .split(|byte| *byte == 0)
            .collect::<Vec<_>>();
        if records.iter().any(|record| record.is_empty()) {
            return Err(GitError::UntrustedOutput {
                detail: format!("git {} returned an empty tree entry", args.join(" ")),
            });
        }
        Ok(records)
    }

    pub(crate) fn graph_blob_bytes(&self, object: &str, limit: usize) -> Result<Vec<u8>, GitError> {
        if !valid_git_object_id(object.as_bytes()) {
            return Err(graph_snapshot_error("invalid exact blob object ID"));
        }
        self.run_git_output_limited(&["cat-file", "blob", object], limit)
            .map(|output| output.stdout)
    }

    fn config_blob_checked(&self, object: &str, path: &str) -> Result<String, GitError> {
        let args = ["cat-file", "blob", object];
        let output = self.run_git_output_limited(&args, 4 * 1024 * 1024)?;
        String::from_utf8(output.stdout).map_err(|_| GitError::UntrustedOutput {
            detail: format!("committed configuration {path:?} is not UTF-8"),
        })
    }

    fn run_git_output_limited(
        &self,
        args: &[&str],
        max_output_bytes: usize,
    ) -> Result<std::process::Output, GitError> {
        let mut command = git_command();
        command
            .args(args)
            .current_dir(&self.root)
            .env("GIT_OPTIONAL_LOCKS", "0");
        let deadline = Instant::now() + git_timeout();
        let output =
            run_git_process_output_limited(&mut command, args, deadline, max_output_bytes)?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(output)
    }

    /// [`Self::config_at_commit`] as TOML text: a TOML file byte-for-byte, a
    /// YAML file validated and rendered from the same value.
    pub fn config_toml_at_commit(
        &self,
        commit: &str,
        stem: &str,
    ) -> Result<Option<String>, GitError> {
        match self.config_at_commit(commit, stem)? {
            Some(text) => Ok(Some(text.toml_text()?.into_owned())),
            None => Ok(None),
        }
    }

    pub fn file_at_commit(&self, commit: &str, path: &str) -> Result<Option<String>, GitError> {
        let path_value = Path::new(path);
        if path_value.is_absolute()
            || path_value.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::Prefix(_)
                )
            })
            || path.contains(':')
        {
            return Err(GitError::Git {
                args: "show <commit>:<path>".into(),
                stderr: "file path must be repository-relative and cannot contain ':'".into(),
            });
        }
        let Some(exact) = self.resolve_ref(commit) else {
            return Ok(None);
        };
        let object = format!("{exact}:{path}");
        match run_git(&self.root, &["cat-file", "-e", &object]) {
            Ok(_) => run_git(&self.root, &["show", &object]).map(Some),
            Err(GitError::Git { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Human-readable description of `rev` using the repository's tags
    /// when available, falling back to an abbreviated commit.
    pub fn describe_ref(&self, rev: &str) -> Option<String> {
        run_git(&self.root, &["describe", "--tags", "--always", rev])
            .ok()
            .filter(|value| !value.is_empty())
    }

    /// Nearest reachable release tag for `rev`, if the repository has
    /// one. Used only for operator-facing version drift classification.
    pub fn nearest_tag(&self, rev: &str) -> Option<String> {
        run_git(&self.root, &["describe", "--tags", "--abbrev=0", rev])
            .ok()
            .filter(|value| !value.is_empty())
    }

    /// Every local branch's commit, keyed by full ref name (`refs/heads/...`),
    /// from one `git for-each-ref`.
    ///
    /// For callers that would otherwise resolve one branch per session: the
    /// cleanup plan behind `broker status` visits every session ever recorded,
    /// and one `rev-parse` each made status fork hundreds of processes on a
    /// repository with a long history. `None` means the listing failed, and the
    /// caller must fall back to resolving each ref rather than read every
    /// branch as absent.
    pub fn local_branch_tips(&self) -> Option<BTreeMap<String, String>> {
        let output = run_git(
            &self.root,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads/",
            ],
        )
        .ok()?;
        Some(
            output
                .lines()
                .filter_map(|line| line.trim().split_once(' '))
                .map(|(name, oid)| (name.to_string(), oid.to_string()))
                .collect(),
        )
    }

    /// Local branches (`refs/heads/...`) whose tip is an ancestor of
    /// `commit`, in one `for-each-ref --merged` listing. `None` when Git
    /// fails.
    pub(crate) fn local_branches_merged_into(
        &self,
        commit: &str,
    ) -> Option<std::collections::BTreeSet<String>> {
        let merged = format!("--merged={commit}");
        let output = run_git(
            &self.root,
            &[
                "for-each-ref",
                &merged,
                "--format=%(refname)",
                "refs/heads/",
            ],
        )
        .ok()?;
        Some(
            output
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect(),
        )
    }

    /// Remote-tracking refs from which `commit` is reachable.
    ///
    /// Reachability from a pushed ref is what makes a worktree disposable: the
    /// commits survive somewhere that is not this directory. It holds whatever
    /// the merge strategy did, which ancestry against the default branch does
    /// not once a squash has rewritten the SHA (#222).
    pub fn remote_refs_containing(&self, commit: &str) -> Vec<String> {
        run_git(
            &self.root,
            &[
                "for-each-ref",
                "--contains",
                commit,
                "--format=%(refname)",
                "refs/remotes/",
            ],
        )
        .map(|output| {
            output
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
    }

    /// What the remote says a branch points at right now, within a bounded wait.
    ///
    /// `None` means "do not know" -- offline, slow, unreadable -- never "absent".
    /// This is consulted on a path that authorizes deletion, so every caller
    /// must treat not-knowing as unproven. Bounding the wait matters for the
    /// same reason it did in the admission lane: `Command::output` has no
    /// deadline, and a wedged remote would hang a local cleanup (#219).
    pub fn remote_branch_head(
        &self,
        remote: &str,
        branch: &str,
        budget: std::time::Duration,
    ) -> Option<String> {
        #[cfg(test)]
        TEST_REMOTE_QUERIES.with(|queries| queries.set(queries.get() + 1));
        let mut command = git_command();
        command.arg("-C").arg(&self.root).args([
            "ls-remote",
            "--heads",
            remote,
            &format!("refs/heads/{branch}"),
        ]);
        let output = crate::bounded_output::output_within(&mut command, budget).ok()??;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().next())
            .map(str::to_string)
    }

    /// Every branch `remote` lists right now, by branch name, within a
    /// bounded wait. `None` means "do not know", as for
    /// [`Self::remote_branch_head`]; a branch missing from a listing that
    /// was read is absent on the remote.
    pub fn remote_branch_heads(
        &self,
        remote: &str,
        budget: std::time::Duration,
    ) -> Option<BTreeMap<String, String>> {
        #[cfg(test)]
        TEST_REMOTE_QUERIES.with(|queries| queries.set(queries.get() + 1));
        let mut command = git_command();
        command
            .arg("-C")
            .arg(&self.root)
            .args(["ls-remote", "--heads", remote]);
        let output = crate::bounded_output::output_within(&mut command, budget).ok()??;
        if !output.status.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let (oid, name) = line.split_once('\t')?;
                    Some((
                        name.strip_prefix("refs/heads/")?.to_string(),
                        oid.to_string(),
                    ))
                })
                .collect(),
        )
    }

    /// True when `ancestor` is reachable from `descendant`
    /// (`git merge-base --is-ancestor`).
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        self.is_ancestor_checked(ancestor, descendant)
            .unwrap_or(false)
    }

    /// The checked form distinguishes "not an ancestor" from an inspection
    /// that failed or exhausted its enclosing deadline. Interactive status
    /// must not turn the latter into a divergence verdict.
    pub fn is_ancestor_checked(&self, ancestor: &str, descendant: &str) -> Result<bool, GitError> {
        let args = ["merge-base", "--is-ancestor", ancestor, descendant];
        let mut command = git_command();
        command
            .args(args)
            .current_dir(&self.root)
            .env("GIT_OPTIONAL_LOCKS", "0");
        let output = run_git_process_output(command, &args, git_timeout())?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(GitError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            }),
        }
    }

    /// Fast-forward the checked-out branch and worktree to an exact commit.
    /// The caller is responsible for clean-worktree and ancestry preflight;
    /// `--ff-only` keeps Git as the final enforcement boundary.
    pub fn fast_forward_checkout(&self, commit: &str) -> Result<(), GitError> {
        run_git(&self.root, &["merge", "--ff-only", commit])?;
        Ok(())
    }

    /// Fast-forward the checked-out `branch` from exactly `from` to `to`.
    ///
    /// The branch ref moves by compare-and-swap, so a HEAD that moved after
    /// the caller's preflight fails here instead of being carried forward.
    /// The index and worktree then follow with a two-tree `read-tree -m -u`,
    /// which refuses on any local change or a held index lock; in that case
    /// the ref is swapped back so the checkout is left exactly as found.
    pub fn fast_forward_checkout_from(
        &self,
        branch: &str,
        from: &str,
        to: &str,
    ) -> Result<(), GitError> {
        let reference = format!("refs/heads/{branch}");
        run_git(&self.root, &["update-ref", &reference, to, from])?;
        if let Err(error) = run_git(&self.root, &["read-tree", "-m", "-u", from, to]) {
            let restored = run_git(&self.root, &["update-ref", &reference, from, to]);
            if let Err(restore) = restored {
                return Err(GitError::Git {
                    args: format!("read-tree -m -u {from} {to}"),
                    stderr: format!(
                        "{error}; restoring {reference} to {from} also failed: {restore}"
                    ),
                });
            }
            return Err(error);
        }
        Ok(())
    }

    /// Move the index and working tree from `from` to `to`, leaving every
    /// unrelated local change where it is. The first half of a primary
    /// checkout fast-forward; the branch ref moves only afterwards
    /// ([`Self::swap_branch_ref`]), so an interruption between the two leaves
    /// the tree ahead of HEAD, never HEAD ahead of the tree.
    ///
    /// A two-tree `read-tree -m -u` is Git's own branch-switch rule: a path
    /// whose content is the same in `from` and `to` keeps its index entry and
    /// working file untouched; a path the update changes is replaced only when
    /// its index entry and working file still match `from`; an untracked or
    /// ignored file the update would write over makes Git refuse. On refusal
    /// nothing is written. No stash, reset, forced checkout or submodule update.
    ///
    /// Runs under its own `budget`, not an enclosing command deadline. No
    /// repository hook runs: `read-tree` and `update-index` have none.
    pub(crate) fn update_tree_preserving_local_changes(
        &self,
        from: &str,
        to: &str,
        budget: Duration,
    ) -> Result<(), GitError> {
        let hooks_off = format!("core.hooksPath={NO_HOOKS_PATH}");
        // Refresh only the stat data of entries whose content is unchanged,
        // so a file touched without being edited is not taken for a local
        // change. It never rewrites an entry whose content differs. Best
        // effort: `read-tree` stays the arbiter when this cannot run.
        run_git_own_bound(
            &self.root,
            &["-c", &hooks_off, "update-index", "-q", "--refresh"],
            budget,
        )
        .ok();
        run_git_own_bound(
            &self.root,
            &["-c", &hooks_off, "read-tree", "-m", "-u", from, to],
            budget,
        )?;
        Ok(())
    }

    /// Compare-and-swap `refs/heads/<branch>` from exactly `from` to `to`,
    /// with a reflog entry. `core.hooksPath` points where no hook can exist,
    /// so `reference-transaction` does not run.
    pub(crate) fn swap_branch_ref(
        &self,
        branch: &str,
        from: &str,
        to: &str,
        budget: Duration,
    ) -> Result<(), GitError> {
        let hooks_off = format!("core.hooksPath={NO_HOOKS_PATH}");
        let reference = format!("refs/heads/{branch}");
        run_git_own_bound(
            &self.root,
            &[
                "-c",
                &hooks_off,
                "update-ref",
                "-m",
                "aethyme: fast-forward main checkout",
                &reference,
                to,
                from,
            ],
            budget,
        )?;
        Ok(())
    }

    /// `(status, path)` for every path that differs between two commits,
    /// renames reported as a deletion and an addition (`A`, `D`, `M`, `T`).
    pub(crate) fn changed_between_with_status(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<(u8, String)>, GitError> {
        let args = [
            "diff",
            "--name-status",
            "-z",
            "--no-renames",
            from,
            to,
            "--",
        ];
        let output = run_git_bytes(&self.root, &args)?;
        let mut fields = output.split(|byte| *byte == 0).filter(|f| !f.is_empty());
        let mut entries = Vec::new();
        while let Some(status) = fields.next() {
            let path = fields.next().ok_or_else(|| GitError::UntrustedOutput {
                detail: "git diff --name-status -z ended without a path".into(),
            })?;
            let path = std::str::from_utf8(path).map_err(|_| GitError::UntrustedOutput {
                detail: "git diff --name-status -z returned a non-UTF-8 path".into(),
            })?;
            entries.push((status.first().copied().unwrap_or(b'?'), path.to_string()));
        }
        Ok(entries)
    }

    /// Paths whose index entry differs from `commit`.
    pub(crate) fn index_paths_differing_from(
        &self,
        commit: &str,
    ) -> Result<std::collections::BTreeSet<String>, GitError> {
        Ok(parse_nul_paths(&run_git(
            &self.root,
            &[
                "diff",
                "--cached",
                "--name-only",
                "-z",
                "--no-renames",
                "--ignore-submodules=none",
                commit,
                "--",
            ],
        )?)
        .into_iter()
        .collect())
    }

    /// Tracked paths whose working file differs from its index entry.
    pub(crate) fn worktree_paths_differing_from_index(
        &self,
    ) -> Result<std::collections::BTreeSet<String>, GitError> {
        Ok(parse_nul_paths(&run_git(
            &self.root,
            &[
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                "--ignore-submodules=none",
                "--",
            ],
        )?)
        .into_iter()
        .collect())
    }

    /// Commits after `from` up to `to` along `to`'s first-parent line, oldest
    /// first, each a descendant of `from` (so a fast-forward target).
    pub(crate) fn fast_forward_steps(&self, from: &str, to: &str) -> Result<Vec<String>, GitError> {
        let range = format!("{from}..{to}");
        let listing = run_git(
            &self.root,
            &[
                "rev-list",
                "--reverse",
                "--first-parent",
                "--ancestry-path",
                &range,
            ],
        )?;
        Ok(listing
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// The blob ids `git hash-object` gives the working files at `paths`, in
    /// order (clean filters and line-ending rules applied, as `git add`
    /// would). Paths with a newline are refused.
    pub(crate) fn hash_working_files(&self, paths: &[String]) -> Result<Vec<String>, GitError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        if paths.iter().any(|path| path.contains('\n')) {
            return Err(GitError::UntrustedOutput {
                detail: "a path contains a newline".into(),
            });
        }
        let input = paths.join("\n") + "\n";
        let output = run_git_bytes_with_stdin(
            &self.root,
            &["hash-object", "--stdin-paths"],
            input.as_bytes(),
        )?;
        let ids = String::from_utf8_lossy(&output)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if ids.len() != paths.len() {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "git hash-object returned {} ids for {} paths",
                    ids.len(),
                    paths.len()
                ),
            });
        }
        Ok(ids)
    }

    /// The blob id `file` would get if it were the working file at the
    /// repository-relative `path` (that path's filters and line-ending rules
    /// applied), for checking a copy against a blob proven at `path`.
    pub(crate) fn hash_file_as(&self, path: &str, file: &Path) -> Result<String, GitError> {
        let path_arg = format!("--path={path}");
        let file_arg = file.to_string_lossy().into_owned();
        run_git(&self.root, &["hash-object", &path_arg, "--", &file_arg])
    }

    /// Every blob id `path` has had in the last `max_commits` commits that
    /// touched it in `commit`'s history: a record of the exact bytes once
    /// committed there.
    pub(crate) fn recorded_blobs(
        &self,
        commit: &str,
        path: &str,
        max_commits: usize,
    ) -> Result<std::collections::BTreeSet<String>, GitError> {
        let limit = format!("--max-count={max_commits}");
        let listing = run_git(
            &self.root,
            &[
                "log",
                "--format=",
                "--raw",
                "--no-abbrev",
                "--no-renames",
                &limit,
                commit,
                "--",
                path,
            ],
        )?;
        Ok(listing
            .lines()
            .filter(|line| line.starts_with(':'))
            .flat_map(|line| {
                line.split_whitespace()
                    .skip(2)
                    .take(2)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|id| id.bytes().any(|b| b != b'0'))
            .collect())
    }

    /// Rewrite the working files at `paths` from their index entries, and
    /// nothing else. Only for files the caller has proven replaceable; no
    /// repository hook runs.
    pub(crate) fn restore_working_files_from_index(
        &self,
        paths: &[String],
    ) -> Result<(), GitError> {
        if paths.is_empty() {
            return Ok(());
        }
        let hooks_off = format!("core.hooksPath={NO_HOOKS_PATH}");
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        run_git_bytes_with_stdin(
            &self.root,
            &[
                "-c",
                &hooks_off,
                "checkout-index",
                "--force",
                "--quiet",
                "--stdin",
                "-z",
            ],
            &input,
        )?;
        Ok(())
    }

    /// Whether any clean/smudge filter driver (`filter.<name>.smudge` or
    /// `filter.<name>.process`) is configured for this checkout.
    pub(crate) fn filter_drivers_configured(&self) -> Result<bool, GitError> {
        let args = ["config", "--get-regexp", r"^filter\..*\.(smudge|process)$"];
        let mut command = git_command();
        command
            .args(args)
            .current_dir(&self.root)
            .env("GIT_OPTIONAL_LOCKS", "0");
        let output = run_git_process_output(command, &args, git_timeout())?;
        match output.status.code() {
            Some(0) => Ok(!output.stdout.is_empty()),
            Some(1) => Ok(false),
            _ => Err(GitError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            }),
        }
    }

    /// The `paths` that a `filter` attribute applies to.
    pub(crate) fn paths_with_filter_attribute(
        &self,
        paths: &[String],
    ) -> Result<Vec<String>, GitError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        let output = run_git_bytes_with_stdin(
            &self.root,
            &["check-attr", "-z", "--stdin", "filter"],
            &input,
        )?;
        let fields = output
            .split(|byte| *byte == 0)
            .map(|field| String::from_utf8_lossy(field).into_owned())
            .collect::<Vec<_>>();
        Ok(fields
            .chunks(3)
            .filter(|triple| triple.len() == 3)
            .filter(|triple| triple[2] != "unspecified" && triple[2] != "unset")
            .map(|triple| triple[0].clone())
            .collect())
    }

    /// Fetch exactly `refs/heads/<branch>` of `remote` into
    /// `refs/remotes/<remote>/<branch>` for the background main-checkout
    /// sync, and nothing else: an explicit non-forcing refspec, no tags, no
    /// pruning, no submodules, no automatic gc, no hooks and no interactive
    /// credential prompt. Refuses names that could be read as options or
    /// refspec syntax.
    pub(crate) fn fetch_default_branch_for_sync(
        &self,
        remote: &str,
        branch: &str,
        budget: Duration,
    ) -> Result<(), GitError> {
        let unsafe_name = |name: &str| {
            name.is_empty()
                || name.starts_with('-')
                || name.starts_with('+')
                || name
                    .chars()
                    .any(|c| c == ':' || c == '*' || c.is_whitespace() || c.is_control())
        };
        if unsafe_name(remote) || unsafe_name(branch) {
            return Err(GitError::Git {
                args: "fetch".into(),
                stderr: format!("refusing to fetch remote {remote:?} branch {branch:?}"),
            });
        }
        let _span = crate::phase_telemetry::span("git_fetch");
        let hooks_off = format!("core.hooksPath={NO_HOOKS_PATH}");
        let refspec = format!("refs/heads/{branch}:refs/remotes/{remote}/{branch}");
        let args = [
            "-c",
            hooks_off.as_str(),
            "-c",
            "credential.interactive=never",
            "fetch",
            "--no-tags",
            "--no-prune",
            "--no-recurse-submodules",
            "--no-auto-gc",
            "--quiet",
            remote,
            refspec.as_str(),
        ];
        let mut command = git_command();
        command
            .args(args)
            .current_dir(&self.root)
            .env("GIT_TERMINAL_PROMPT", "0");
        run_git_command(command, &args, budget)?;
        Ok(())
    }

    /// Tracked paths whose index entry or working file differs from HEAD,
    /// renames reported on both sides and submodules with any change
    /// included. Read-only.
    pub(crate) fn locally_changed_tracked_paths(&self) -> Result<Vec<String>, GitError> {
        let mut paths = std::collections::BTreeSet::new();
        for cached in [false, true] {
            let mut args = vec![
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                "--ignore-submodules=none",
            ];
            if cached {
                args.push("--cached");
            }
            args.extend(["HEAD", "--"]);
            paths.extend(parse_nul_paths(&run_git(&self.root, &args)?));
        }
        Ok(paths.into_iter().collect())
    }

    /// Whether any index entry is marked assume-unchanged or skip-worktree:
    /// either hides a local change from the checks the caller makes.
    pub(crate) fn has_hidden_index_entries(&self) -> Result<bool, GitError> {
        let listing = run_git_bytes(&self.root, &["ls-files", "-v", "-z"])?;
        Ok(listing
            .split(|byte| *byte == 0)
            .filter_map(|entry| entry.first())
            .any(|tag| tag.is_ascii_lowercase() || *tag == b'S'))
    }

    /// The first Git operation left in progress in this checkout, or a held
    /// index lock: a merge, cherry-pick, revert, bisect or rebase that a
    /// person or agent is in the middle of. Paths resolve through
    /// `rev-parse --git-path`, so linked worktrees and relocated git
    /// directories are checked where Git itself keeps them.
    pub fn in_progress_operation(&self) -> Result<Option<&'static str>, GitError> {
        const MARKERS: [(&str, &str); 7] = [
            ("MERGE_HEAD", "a merge is in progress"),
            ("CHERRY_PICK_HEAD", "a cherry-pick is in progress"),
            ("REVERT_HEAD", "a revert is in progress"),
            ("BISECT_LOG", "a bisect is in progress"),
            ("rebase-merge", "a rebase is in progress"),
            ("rebase-apply", "a rebase or am is in progress"),
            ("index.lock", "another Git process holds the index lock"),
        ];
        let mut args = vec!["rev-parse"];
        for (marker, _) in MARKERS {
            args.extend(["--git-path", marker]);
        }
        let output = run_git(&self.root, &args)?;
        let paths: Vec<&str> = output.lines().collect();
        if paths.len() != MARKERS.len() {
            return Err(GitError::Git {
                args: "rev-parse --git-path".into(),
                stderr: format!(
                    "expected {} paths, Git printed {}",
                    MARKERS.len(),
                    paths.len()
                ),
            });
        }
        for ((_, reason), path) in MARKERS.iter().zip(paths) {
            if self.root.join(path).exists() {
                return Ok(Some(reason));
            }
        }
        Ok(None)
    }

    /// Create or fast-move a local branch ref to `commit` (no checkout).
    pub fn update_branch_ref(&self, branch: &str, commit: &str) -> Result<(), GitError> {
        run_git(
            &self.root,
            &["update-ref", &format!("refs/heads/{branch}"), commit],
        )?;
        Ok(())
    }

    /// Compare-and-swap a branch ref. Reconciliation uses the expected
    /// old value so a concurrent promotion cannot be silently overwritten.
    pub fn update_branch_ref_checked(
        &self,
        branch: &str,
        commit: &str,
        expected_old: &str,
    ) -> Result<(), GitError> {
        run_git(
            &self.root,
            &[
                "update-ref",
                &format!("refs/heads/{branch}"),
                commit,
                expected_old,
            ],
        )?;
        Ok(())
    }

    /// First parent of a commit, used to recover the exact promoted delta
    /// even though promotion details intentionally keep a compact payload.
    pub fn first_parent(&self, commit: &str) -> Result<String, GitError> {
        run_git(&self.root, &["rev-parse", &format!("{commit}^1")])
    }

    /// The newest `limit` commits in `from..to` touching any of `globs`
    /// (every commit when empty), returned oldest first.
    pub fn commits_touching(
        &self,
        from: &str,
        to: &str,
        globs: &[String],
        limit: usize,
    ) -> Result<Vec<String>, GitError> {
        let range = format!("{from}..{to}");
        let max = format!("--max-count={limit}");
        let pathspecs: Vec<String> = globs.iter().map(|glob| format!(":(glob){glob}")).collect();
        let mut args = vec!["rev-list", max.as_str(), range.as_str()];
        if !pathspecs.is_empty() {
            args.push("--");
            args.extend(pathspecs.iter().map(String::as_str));
        }
        let mut commits: Vec<String> = run_git(&self.root, &args)?
            .lines()
            .map(str::to_string)
            .collect();
        commits.reverse();
        Ok(commits)
    }

    /// The commit on `head`'s first-parent line that brought `commit` in:
    /// `commit` itself when it lies on that line, otherwise the oldest
    /// first-parent descendant of it, the merge that landed it.
    pub fn landing_commit(&self, commit: &str, head: &str) -> Result<String, GitError> {
        let commit = run_git(
            &self.root,
            &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
        )?;
        let range = format!("{commit}..{head}");
        let listed = run_git(
            &self.root,
            &["rev-list", "--first-parent", "--ancestry-path", &range],
        )?;
        let Some(oldest) = listed.lines().last() else {
            return Ok(commit);
        };
        let parents = self.commit_parents(oldest)?;
        Ok(
            if parents.first().map(String::as_str) == Some(commit.as_str()) {
                commit
            } else {
                oldest.to_string()
            },
        )
    }

    /// One commit's subject line.
    pub fn commit_subject(&self, commit: &str) -> Result<String, GitError> {
        run_git(&self.root, &["log", "-1", "--format=%s", commit])
    }

    /// Every commit reachable from `head`, `head` included: one `rev-list`
    /// that answers `is_ancestor(commit, head)` for any number of commits by
    /// membership.
    pub fn reachable_commits(&self, head: &str) -> Result<Vec<String>, GitError> {
        Ok(run_git(&self.root, &["rev-list", head])?
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Every path any non-merge commit in `from..to` changed, both sides of a
    /// rename included, sorted and deduplicated. Unlike the net diff, a path
    /// one commit changed and a later one restored is still listed.
    pub(crate) fn paths_touched_between(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, GitError> {
        let range = format!("{from}..{to}");
        let out = run_git(
            &self.root,
            &[
                "log",
                "--no-merges",
                "--no-renames",
                "--name-only",
                "--format=",
                "-z",
                &range,
            ],
        )?;
        let mut paths = out
            .split('\0')
            .map(|path| path.trim_matches('\n'))
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Commits reachable from `to` but not `from`, oldest first.
    pub fn commits_between_oldest(&self, from: &str, to: &str) -> Result<Vec<String>, GitError> {
        let range = format!("{from}..{to}");
        Ok(run_git(&self.root, &["rev-list", "--reverse", &range])?
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Commits reachable from `to` but not `from`, oldest first, with bounded
    /// process time and captured output. An incomplete listing is an error;
    /// callers must not interpret it as a complete history window.
    pub(crate) fn commits_between_oldest_bounded(
        &self,
        from: &str,
        to: &str,
        max_output_bytes: usize,
    ) -> Result<Vec<String>, GitError> {
        let range = format!("{from}..{to}");
        let args = "rev-list --reverse";
        let started = Instant::now();
        let deadline = effective_git_deadline(args, git_timeout(), started)?;
        let budget = remaining_git_budget(args, deadline)?;
        let mut command = git_command();
        command
            .args(["rev-list", "--reverse", &range])
            .current_dir(&self.root);
        let output =
            match crate::bounded_output::output_limited(&mut command, budget, max_output_bytes)
                .map_err(|source| GitError::Spawn {
                    args: args.into(),
                    source,
                })? {
                crate::bounded_output::LimitedOutput::Completed(output) => output,
                crate::bounded_output::LimitedOutput::Deadline => {
                    note_git_timeout();
                    return Err(GitError::timed_out_running(
                        args,
                        Some(&self.root),
                        started,
                        deadline_is_outer(deadline),
                        budget,
                    ));
                }
                crate::bounded_output::LimitedOutput::OutputLimit => {
                    return Err(GitError::Git {
                        args: args.into(),
                        stderr: format!(
                            "Git command output exceeded {max_output_bytes} bytes per stream"
                        ),
                    });
                }
            };
        if !output.status.success() {
            return Err(GitError::Git {
                args: args.into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        if Instant::now() >= deadline {
            note_git_timeout();
            return Err(GitError::timed_out_running(
                args,
                Some(&self.root),
                started,
                deadline_is_outer(deadline),
                deadline.saturating_duration_since(started),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Commits reachable from `head` but not `excluded`, oldest first.
    /// Unlike a `from..to` range this remains meaningful when the two
    /// histories have diverged.
    pub fn commits_excluding_oldest(
        &self,
        head: &str,
        excluded: &str,
    ) -> Result<Vec<String>, GitError> {
        Ok(run_git(
            &self.root,
            &["rev-list", "--reverse", head, "--not", excluded],
        )?
        .lines()
        .map(str::to_string)
        .collect())
    }

    /// Earliest committer time among `commits`, Unix epoch milliseconds, in
    /// one Git process however many commits are named.
    pub fn oldest_commit_time_ms(&self, commits: &[&str]) -> Option<i64> {
        if commits.is_empty() {
            return None;
        }
        let mut args = vec!["log", "--no-walk=unsorted", "--format=%ct"];
        args.extend_from_slice(commits);
        run_git(&self.root, &args)
            .ok()?
            .lines()
            .filter_map(|seconds| seconds.trim().parse::<i64>().ok())
            .min()
            .map(|seconds| seconds * 1000)
    }

    /// First-parent commits reachable from `head` but not `excluded`,
    /// oldest first. Submission provenance uses only this layer so a
    /// broker promotion merge and its submitted second parent do not
    /// appear as two patch-identity candidates.
    pub fn first_parent_commits_excluding_oldest(
        &self,
        head: &str,
        excluded: &str,
    ) -> Result<Vec<String>, GitError> {
        Ok(run_git(
            &self.root,
            &[
                "rev-list",
                "--first-parent",
                "--reverse",
                head,
                "--not",
                excluded,
            ],
        )?
        .lines()
        .map(str::to_string)
        .collect())
    }

    /// Every parent of a commit, in Git's stored order.
    pub fn commit_parents(&self, commit: &str) -> Result<Vec<String>, GitError> {
        let line = run_git(&self.root, &["rev-list", "--parents", "-n", "1", commit])?;
        Ok(line
            .split_whitespace()
            .skip(1)
            .map(str::to_string)
            .collect())
    }

    pub(crate) fn commit_parents_until_with_limit(
        &self,
        commit: &str,
        deadline: Instant,
        max_output_bytes: usize,
    ) -> Result<Vec<String>, GitError> {
        let args = [
            "--no-replace-objects",
            "rev-list",
            "--parents",
            "-n",
            "1",
            commit,
        ];
        let deadline = active_git_deadline().map_or(deadline, |active| active.min(deadline));
        ensure_bounded_git_listing_before(deadline, &args)?;
        if std::env::vars_os().any(|(key, _)| unsafe_session_commit_git_environment_key(&key)) {
            return Err(GitError::UntrustedOutput {
                detail:
                    "inherited Git routing or configuration overrides make commit ancestry unknown"
                        .into(),
            });
        }
        let mut command = git_command();
        command.args(args).current_dir(&self.root);
        let output =
            run_git_process_output_limited(&mut command, &args, deadline, max_output_bytes)?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        ensure_bounded_git_listing_before(deadline, &args)?;
        if output.stdout.last() != Some(&b'\n')
            || output.stdout[..output.stdout.len().saturating_sub(1)].contains(&b'\n')
        {
            return Err(GitError::UntrustedOutput {
                detail: "git returned an incomplete commit ancestry record".into(),
            });
        }
        let mut fields =
            output.stdout[..output.stdout.len() - 1].split(|byte| byte.is_ascii_whitespace());
        let object = fields.next().ok_or_else(|| GitError::UntrustedOutput {
            detail: "git returned an empty commit ancestry record".into(),
        })?;
        if !valid_git_object_id(object) || object != commit.as_bytes() {
            return Err(GitError::UntrustedOutput {
                detail: "git returned a malformed commit object id".into(),
            });
        }
        let mut parents = Vec::new();
        for parent in fields.filter(|field| !field.is_empty()) {
            if !valid_git_object_id(parent) {
                return Err(GitError::UntrustedOutput {
                    detail: "git returned a malformed parent object id".into(),
                });
            }
            parents.push(String::from_utf8(parent.to_vec()).map_err(|_| {
                GitError::UntrustedOutput {
                    detail: "git returned a non-UTF-8 parent object id".into(),
                }
            })?);
        }
        Ok(parents)
    }

    /// Tree object referenced by a commit.
    /// `(status, path)` for every path a commit changed, against its first
    /// parent. Status is git's name-status letter: `A`, `M`, `D`, and so on.
    pub fn commit_changed_paths(&self, commit: &str) -> Result<Vec<(String, String)>, GitError> {
        let out = run_git(
            &self.root,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-status",
                "-r",
                "-m",
                "--first-parent",
                commit,
            ],
        )?;
        Ok(out
            .lines()
            .filter_map(|line| {
                let mut parts = line.split('\t');
                let status = parts.next()?.trim().to_string();
                let path = parts.next()?.trim().to_string();
                (!status.is_empty() && !path.is_empty()).then_some((status, path))
            })
            .collect())
    }

    /// Renames git detects between two commits, as `old -> new`.
    ///
    /// Computed once over the whole diff rather than per path: rename detection
    /// needs to see both sides, so a pathspec limited to the old name would hide
    /// the new one and report a plain deletion.
    pub fn renames_between(&self, from: &str, to: &str) -> Result<Vec<(String, String)>, GitError> {
        let out = run_git(
            &self.root,
            &[
                "diff",
                "-M",
                "--diff-filter=R",
                "--name-status",
                "--no-renames-empty",
                from,
                to,
            ],
        )
        .or_else(|_| {
            // Older git does not know --no-renames-empty.
            run_git(
                &self.root,
                &["diff", "-M", "--diff-filter=R", "--name-status", from, to],
            )
        })?;
        Ok(out
            .lines()
            .filter_map(|line| {
                let mut parts = line.split('\t');
                let status = parts.next()?;
                if !status.starts_with('R') {
                    return None;
                }
                let old = parts.next()?.trim().to_string();
                let new = parts.next()?.trim().to_string();
                (!old.is_empty() && !new.is_empty()).then_some((old, new))
            })
            .collect())
    }

    /// Blob id of `path` at `rev`, or `None` when the path does not exist there.
    /// Comparing blob ids is how "this change is already present" is decided
    /// without replaying the patch.
    pub fn blob_at(&self, rev: &str, path: &str) -> Option<String> {
        self.blob_at_for_batch(rev, path).ok().flatten()
    }

    /// [`Self::blob_at`] while preserving failures that make an ownership
    /// comparison incomplete. A quiet missing-path result is the only Git
    /// failure interpreted as absence; timeouts and other failures propagate.
    fn blob_at_for_batch(&self, rev: &str, path: &str) -> Result<Option<String>, GitError> {
        let spec = format!("{rev}:{path}");
        match run_git(&self.root, &["rev-parse", "--verify", "--quiet", &spec]) {
            Ok(value) => Ok((!value.is_empty()).then_some(value)),
            Err(GitError::Git { stderr, .. }) if stderr.is_empty() => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// [`GitRepo::blob_at`] for many `(rev, path)` pairs, answered in input
    /// order and with exactly its answers.
    ///
    /// A landing search asks this question for every candidate commit, and
    /// one `rev-parse` per question made that search too slow for the routine
    /// cleanup plan behind `broker status`. `cat-file --batch-check` answers
    /// the whole list in a single spawn.
    ///
    /// The answer is the object id at the path whatever its kind, as
    /// `rev-parse` gives it: a gitlink answers its submodule commit and a
    /// directory its tree. Answering only blobs made two different submodule
    /// pointers both `None`, so a session that only moved one compared equal
    /// to its own base and read as already represented.
    ///
    /// The batch protocol is line-delimited, and NUL-delimited output (`-Z`)
    /// needs Git 2.42, above [`crate::MINIMUM_GIT_VERSION`]. A query holding a
    /// newline would be split in two, and Git strips a trailing carriage
    /// return from each line, so those rare queries are read one at a time.
    /// The full list is bounded by count, staged input bytes, output bytes and
    /// the active Git deadline; inability to complete any path read is an
    /// error, never an absent object.
    pub fn objects_at_many(
        &self,
        queries: &[(&str, &str)],
    ) -> Result<Vec<Option<String>>, GitError> {
        let args = "cat-file --batch-check";
        if queries.len() > MAX_GIT_BATCH_QUERY_COUNT {
            return Err(GitError::Git {
                args: args.into(),
                stderr: format!("Git batch exceeded {MAX_GIT_BATCH_QUERY_COUNT} queries"),
            });
        }
        let started = Instant::now();
        let deadline = effective_git_deadline(args, git_timeout(), started)?;
        let _deadline = limit_git_until(deadline);
        checked_git_batch_check_input_len(queries, MAX_GIT_BATCH_INPUT_BYTES, deadline)
            .map_err(|error| git_batch_input_error(args, started, deadline, error))?;
        let line_safe = |text: &str| !text.contains(['\n', '\r']);
        let (batched, single): (Vec<usize>, Vec<usize>) = (0..queries.len())
            .partition(|&index| line_safe(queries[index].0) && line_safe(queries[index].1));
        let mut answers = vec![None; queries.len()];
        for index in single {
            let (rev, path) = queries[index];
            answers[index] = self.blob_at_for_batch(rev, path)?;
        }
        let read = self.batch_check_objects(
            &batched
                .iter()
                .map(|&index| queries[index])
                .collect::<Vec<_>>(),
        )?;
        for (index, answer) in batched.into_iter().zip(read) {
            answers[index] = answer;
        }
        if Instant::now() >= deadline {
            note_git_timeout();
            return Err(GitError::timed_out_running(
                args,
                Some(&self.root),
                started,
                deadline_is_outer(deadline),
                deadline.saturating_duration_since(started),
            ));
        }
        Ok(answers)
    }

    /// One bounded `cat-file --batch-check` over queries that hold no line
    /// breaks.
    fn batch_check_objects(
        &self,
        queries: &[(&str, &str)],
    ) -> Result<Vec<Option<String>>, GitError> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        let args = "cat-file --batch-check=%(objectname) %(objecttype)";
        if queries.len() > MAX_GIT_BATCH_QUERY_COUNT {
            return Err(GitError::Git {
                args: args.into(),
                stderr: format!("Git batch exceeded {MAX_GIT_BATCH_QUERY_COUNT} queries"),
            });
        }
        let started = Instant::now();
        let deadline = effective_git_deadline(args, git_timeout(), started)?;
        let input = git_batch_check_input(queries, deadline)
            .map_err(|error| git_batch_input_error(args, started, deadline, error))?;
        let budget = remaining_git_budget(args, deadline)?;
        let mut command = git_command();
        command
            .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"])
            .current_dir(&self.root);
        let output = match crate::bounded_output::output_with_input_limited(
            &mut command,
            input,
            budget,
            MAX_GIT_BATCH_OUTPUT_BYTES,
        )
        .map_err(|source| GitError::Spawn {
            args: args.into(),
            source,
        })? {
            crate::bounded_output::LimitedOutput::Completed(output) => output,
            crate::bounded_output::LimitedOutput::Deadline => {
                note_git_timeout();
                return Err(GitError::timed_out_running(
                    args,
                    Some(&self.root),
                    started,
                    deadline_is_outer(deadline),
                    budget,
                ));
            }
            crate::bounded_output::LimitedOutput::OutputLimit => {
                return Err(GitError::Git {
                    args: args.into(),
                    stderr: format!(
                        "Git batch output exceeded {MAX_GIT_BATCH_OUTPUT_BYTES} bytes per stream"
                    ),
                });
            }
        };
        if !output.status.success() {
            return Err(GitError::Git {
                args: args.into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let answers = stdout
            .lines()
            .map(|line| {
                // "<oid> <kind>" for an entry, "<input> missing" (or
                // "ambiguous") when the path does not resolve. The input is
                // echoed only on those, so the last field is always the kind.
                let mut fields = line.rsplitn(2, ' ');
                let kind = fields.next().unwrap_or_default();
                let oid = fields.next().unwrap_or_default();
                matches!(kind, "blob" | "tree" | "submodule" | "commit" | "tag")
                    .then(|| oid.to_string())
            })
            .collect::<Vec<_>>();
        if answers.len() != queries.len() {
            return Err(GitError::Git {
                args: args.into(),
                stderr: format!("expected {} answers, read {}", queries.len(), answers.len()),
            });
        }
        if Instant::now() >= deadline {
            note_git_timeout();
            return Err(GitError::timed_out_running(
                args,
                Some(&self.root),
                started,
                deadline_is_outer(deadline),
                deadline.saturating_duration_since(started),
            ));
        }
        Ok(answers)
    }

    fn cherry_side_commits(
        &self,
        range: &str,
        only: &str,
    ) -> Result<Vec<(String, bool)>, GitError> {
        Ok(run_git(
            &self.root,
            &["rev-list", "--no-merges", "--parents", only, range],
        )?
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let commit = fields.next()?.to_string();
            Some((commit, fields.next().is_some()))
        })
        .collect())
    }

    /// Commits on one side of `left...right` with whether a patch-equivalent
    /// commit exists on the other side, newest first. Merges are excluded on
    /// both sides, as `--cherry-mark` does. Equivalence is decided with
    /// `git patch-id --verbatim` rather than `--cherry-mark`, whose internal patch ids
    /// ignore whitespace: a commit that differs only in whitespace is not
    /// equivalent. A root commit, or one whose diff is empty, has no patch id
    /// and is never equivalent.
    pub fn cherry_marked(
        &self,
        left: &str,
        right: &str,
        side: CherrySide,
    ) -> Result<Vec<(String, bool)>, GitError> {
        let (listed, other) = match side {
            CherrySide::Left => ("--left-only", "--right-only"),
            CherrySide::Right => ("--right-only", "--left-only"),
        };
        let range = format!("{left}...{right}");
        let listed = self.cherry_side_commits(&range, listed)?;
        // A merged or fast-forwarded branch has nothing on the asked side.
        // Listing the other side and hashing every patch on it -- every
        // commit the default branch gained since the fork -- would answer a
        // question with no commits to ask it about (#588).
        if listed.is_empty() {
            return Ok(Vec::new());
        }
        let other = self.cherry_side_commits(&range, other)?;
        // No commit can have an equivalent patch on an empty opposite side.
        // In particular, a session directly ahead of integration needs no
        // binary patch stream merely to establish this fact (#892).
        if other.is_empty() {
            return Ok(listed
                .into_iter()
                .map(|(commit, _)| (commit, false))
                .collect());
        }
        let comparable =
            self.commits_sharing_changed_paths(&listed, &other, &mut PatchIdCache::default())?;
        let patch_ids = self.first_parent_patch_ids(&comparable)?;
        let other_ids = other
            .iter()
            .filter_map(|(commit, _)| patch_ids.get(commit))
            .collect::<std::collections::HashSet<_>>();
        Ok(listed
            .into_iter()
            .map(|(commit, _)| {
                let equivalent = patch_ids
                    .get(&commit)
                    .is_some_and(|patch| other_ids.contains(patch));
                (commit, equivalent)
            })
            .collect())
    }

    /// Both side-specific results for left...right, as
    /// (left_only, right_only), each newest first.
    ///
    /// Callers that need both directions can share one patch-id stream. When
    /// either side has no commits, neither side can have a patch-equivalent
    /// commit, so the nonempty side is returned as non-equivalent without
    /// generating patches.
    #[cfg(test)]
    pub(crate) fn cherry_marked_both(
        &self,
        left: &str,
        right: &str,
    ) -> Result<PairedCherryMarks, GitError> {
        self.cherry_marked_both_cached(left, right, &mut PatchIdCache::default())
    }

    pub(crate) fn cherry_marked_both_cached(
        &self,
        left: &str,
        right: &str,
        cache: &mut PatchIdCache,
    ) -> Result<PairedCherryMarks, GitError> {
        let range = format!("{left}...{right}");
        let left = self.cherry_side_commits(&range, "--left-only")?;
        let right = self.cherry_side_commits(&range, "--right-only")?;
        if left.is_empty() || right.is_empty() {
            let left = left
                .into_iter()
                .map(|(commit, _)| (commit, false))
                .collect();
            let right = right
                .into_iter()
                .map(|(commit, _)| (commit, false))
                .collect();
            return Ok(PairedCherryMarks {
                left_only: left,
                right_only: right,
            });
        }

        let comparable = self.commits_sharing_changed_paths(&left, &right, cache)?;
        let patch_ids = self.cached_first_parent_patch_ids(&comparable, cache)?;
        let left_patch_ids = left
            .iter()
            .filter_map(|(commit, _)| patch_ids.get(commit).map(String::as_str))
            .collect::<std::collections::HashSet<_>>();
        let right_patch_ids = right
            .iter()
            .filter_map(|(commit, _)| patch_ids.get(commit).map(String::as_str))
            .collect::<std::collections::HashSet<_>>();
        let left = left
            .into_iter()
            .map(|(commit, _)| {
                let equivalent = patch_ids
                    .get(&commit)
                    .is_some_and(|patch_id| right_patch_ids.contains(patch_id.as_str()));
                (commit, equivalent)
            })
            .collect();
        let right = right
            .into_iter()
            .map(|(commit, _)| {
                let equivalent = patch_ids
                    .get(&commit)
                    .is_some_and(|patch_id| left_patch_ids.contains(patch_id.as_str()));
                (commit, equivalent)
            })
            .collect();
        Ok(PairedCherryMarks {
            left_only: left,
            right_only: right,
        })
    }

    /// The commits of two sides whose patch ids can decide patch equivalence
    /// between them: those with a parent whose changed-path set also occurs
    /// on the opposite side.
    ///
    /// A verbatim patch id covers each file header, so two commits with the
    /// same id change the same paths. A commit whose path set occurs on one
    /// side only therefore has no equivalent, and rendering its patch proves
    /// nothing. The default branch usually gained hundreds of commits since a
    /// session forked and almost none share a path set with the session's
    /// few, so this replaces a patch stream over all of that history with one
    /// path listing. Measured on 2026-11-11 over 2,426 default-branch
    /// commits: 13.8 s to hash every patch, 1.0 s to list the paths.
    ///
    /// Narrowing can only withhold an equivalence, never invent one, and a
    /// listing that is unavailable falls back to every commit.
    fn commits_sharing_changed_paths(
        &self,
        left: &[(String, bool)],
        right: &[(String, bool)],
        cache: &mut PatchIdCache,
    ) -> Result<Vec<String>, GitError> {
        let with_parent = |side: &[(String, bool)]| {
            side.iter()
                .filter(|(_, has_parent)| *has_parent)
                .map(|(commit, _)| commit.clone())
                .collect::<Vec<_>>()
        };
        let (left, right) = (with_parent(left), with_parent(right));
        let every_commit = || left.iter().chain(&right).cloned().collect::<Vec<_>>();
        if left.is_empty() || right.is_empty() {
            return Ok(Vec::new());
        }
        let Some(signatures) = self.cached_changed_path_signatures(&every_commit(), cache)? else {
            return Ok(every_commit());
        };
        let on_side = |side: &[String]| {
            side.iter()
                .filter_map(|commit| signatures.get(commit))
                .collect::<std::collections::HashSet<_>>()
        };
        let (left_signatures, right_signatures) = (on_side(&left), on_side(&right));
        Ok(left
            .iter()
            .chain(&right)
            .filter(|commit| {
                signatures.get(*commit).is_some_and(|signature| {
                    left_signatures.contains(signature) && right_signatures.contains(signature)
                })
            })
            .cloned()
            .collect())
    }

    /// Changed-path signatures for `commits`, or `None` when the listing is
    /// too large to hold or does not answer for every commit.
    fn cached_changed_path_signatures(
        &self,
        commits: &[String],
        cache: &mut PatchIdCache,
    ) -> Result<Option<BTreeMap<String, ChangedPathSignature>>, GitError> {
        let key = |commit: &String| (self.root.clone(), commit.clone());
        let missing = commits
            .iter()
            .filter(|commit| !cache.changed_paths.contains_key(&key(commit)))
            .cloned()
            .collect::<Vec<_>>();
        let Some(fresh) = self.changed_path_signatures(&missing)? else {
            return Ok(None);
        };
        let mut result = BTreeMap::new();
        for commit in commits {
            let signature = match cache.changed_paths.get(&key(commit)) {
                Some(signature) => {
                    #[cfg(test)]
                    {
                        cache.hits += 1;
                    }
                    *signature
                }
                None => {
                    let Some(signature) = fresh.get(commit) else {
                        return Ok(None);
                    };
                    if cache.changed_paths.len() < MAX_PATCH_ID_CACHE_ENTRIES {
                        cache.changed_paths.insert(key(commit), *signature);
                    }
                    *signature
                }
            };
            result.insert(commit.clone(), signature);
        }
        Ok(Some(result))
    }

    /// One `git log --name-only` over `commits`, digested per commit. `None`
    /// when the listing exceeds the batch output limit.
    fn changed_path_signatures(
        &self,
        commits: &[String],
    ) -> Result<Option<BTreeMap<String, ChangedPathSignature>>, GitError> {
        use sha2::Digest as _;
        let mut signatures = BTreeMap::new();
        if commits.is_empty() {
            return Ok(Some(signatures));
        }
        let Some(stdout) =
            self.run_git_with_stdin_within_output_limit(&changed_path_log_args(), commits)?
        else {
            return Ok(None);
        };
        for record in stdout.split(|byte| *byte == 0).skip(1) {
            let (commit, paths) = match record.iter().position(|byte| *byte == b'\n') {
                Some(end) => (&record[..end], &record[end..]),
                None => (record, &[][..]),
            };
            // Surrounding blank lines vary with a commit's position in the
            // listing; the path lines between them do not.
            signatures.insert(
                String::from_utf8_lossy(commit).into_owned(),
                sha2::Sha256::digest(paths.trim_ascii()).into(),
            );
        }
        Ok(Some(signatures))
    }

    /// Reuse only successful, complete batches; missing IDs represent empty
    /// diffs, never errors. Callers still perform their exact tree checks.
    fn cached_first_parent_patch_ids(
        &self,
        commits: &[String],
        cache: &mut PatchIdCache,
    ) -> Result<BTreeMap<String, String>, GitError> {
        // A warm cache must not bypass the caller's inspection deadline.
        let started = Instant::now();
        effective_git_deadline("cached patch-id", git_timeout(), started)?;
        let missing = commits
            .iter()
            .filter(|commit| {
                !cache
                    .entries
                    .contains_key(&(self.root.clone(), (*commit).clone()))
            })
            .cloned()
            .collect::<Vec<_>>();
        let fresh = self.first_parent_patch_ids(&missing)?;
        let mut result = fresh.clone();
        for commit in commits {
            let key = (self.root.clone(), commit.clone());
            if let Some(value) = cache.entries.get(&key) {
                #[cfg(test)]
                {
                    cache.hits += 1;
                }
                if let Some(value) = value {
                    result.insert(commit.clone(), value.clone());
                }
            } else if cache.entries.len() < MAX_PATCH_ID_CACHE_ENTRIES {
                cache.entries.insert(key, fresh.get(commit).cloned());
            }
        }
        Ok(result)
    }

    /// Commits reachable from `to` that descend from `from`, excluding
    /// `from` itself (`rev-list --ancestry-path from..to`).
    pub fn descendants_towards(&self, from: &str, to: &str) -> Result<Vec<String>, GitError> {
        Ok(run_git(
            &self.root,
            &["rev-list", "--ancestry-path", &format!("{from}..{to}")],
        )?
        .lines()
        .map(str::to_string)
        .collect())
    }

    pub fn commit_tree_id(&self, commit: &str) -> Result<String, GitError> {
        run_git(&self.root, &["rev-parse", &format!("{commit}^{{tree}}")])
    }

    /// First-parent commits reachable from `to` but not `from`, oldest
    /// first. Integration reconciliation uses this to inspect only the
    /// broker-created layer, excluding submitted heads attached as second
    /// parents of promotion merges.
    pub fn first_parent_commits_between_oldest(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, GitError> {
        let range = format!("{from}..{to}");
        Ok(run_git(
            &self.root,
            &["rev-list", "--first-parent", "--reverse", &range],
        )?
        .lines()
        .map(str::to_string)
        .collect())
    }

    /// Tree and parents of each commit, keyed by commit, from one
    /// `git log --no-walk` however many commits are named. Every requested
    /// commit must resolve; a missing one is an error, as a per-commit
    /// `rev-parse` would have been.
    pub fn commit_shapes(
        &self,
        commits: &[String],
    ) -> Result<BTreeMap<String, CommitShape>, GitError> {
        let mut shapes = BTreeMap::new();
        if commits.is_empty() {
            return Ok(shapes);
        }
        let args = [
            "log",
            "--no-walk=unsorted",
            "--stdin",
            "--no-color",
            "--format=%H %T %P",
        ];
        let stdout = self.run_git_with_stdin(&args, commits)?;
        for line in String::from_utf8_lossy(&stdout).lines() {
            let mut fields = line.split_whitespace();
            let (Some(commit), Some(tree)) = (fields.next(), fields.next()) else {
                continue;
            };
            shapes.insert(
                commit.to_string(),
                CommitShape {
                    tree: tree.to_string(),
                    parents: fields.map(str::to_string).collect(),
                },
            );
        }
        if let Some(missing) = commits.iter().find(|commit| !shapes.contains_key(*commit)) {
            return Err(GitError::Git {
                args: args.join(" "),
                stderr: format!("no commit shape reported for {missing}"),
            });
        }
        Ok(shapes)
    }

    /// Verbatim patch id (`git patch-id --verbatim`) of each commit's diff against its
    /// first parent --
    /// [`Self::patch_id_between`] for `parent..commit` -- from one `git log -p`
    /// streamed into one `git patch-id`, however many commits are named.
    /// Commits whose diff is empty have no patch id and are absent from the
    /// map. Pass only commits that have a parent: a root commit would be
    /// diffed against the empty tree.
    pub fn first_parent_patch_ids(
        &self,
        commits: &[String],
    ) -> Result<BTreeMap<String, String>, GitError> {
        let mut patch_ids = BTreeMap::new();
        if commits.is_empty() {
            return Ok(patch_ids);
        }
        let log_args = first_parent_patch_id_log_args();
        let pipeline_args = format!("{} | patch-id --verbatim", log_args.join(" "));
        let started = Instant::now();
        let deadline = effective_git_deadline(&pipeline_args, git_timeout(), started)?;
        let input = git_batch_input(commits, deadline)
            .map_err(|error| git_batch_input_error(&pipeline_args, started, deadline, error))?;
        let budget = remaining_git_budget(&pipeline_args, deadline)?;
        let mut log = git_command();
        log.args(&log_args).current_dir(&self.root);
        // The diff streams from one process into the other through a pipe,
        // so it is never held in memory however much history it covers.
        let mut patch_id = git_command();
        patch_id.args(PATCH_ID_ARGS).current_dir(&self.root);
        let output = crate::bounded_output::pipeline_with_input_limited(
            &mut log,
            input,
            &mut patch_id,
            budget,
            MAX_GIT_BATCH_OUTPUT_BYTES,
        )
        .map_err(|source| GitError::Spawn {
            args: pipeline_args.clone(),
            source,
        })?;
        let (log_status, log_stderr, output) = match output {
            crate::bounded_output::LimitedPipelineOutput::Completed {
                first_status,
                first_stderr,
                second,
            } => (first_status, first_stderr, second),
            crate::bounded_output::LimitedPipelineOutput::Deadline => {
                note_git_timeout();
                return Err(GitError::timed_out_running(
                    pipeline_args,
                    Some(&self.root),
                    started,
                    deadline_is_outer(deadline),
                    budget,
                ));
            }
            crate::bounded_output::LimitedPipelineOutput::OutputLimit => {
                return Err(GitError::Git {
                    args: pipeline_args,
                    stderr: format!(
                        "streamed Git output exceeded {MAX_GIT_BATCH_OUTPUT_BYTES} bytes per captured stream"
                    ),
                });
            }
        };
        if !log_status.success() {
            return Err(GitError::Git {
                args: log_args.join(" "),
                stderr: String::from_utf8_lossy(&log_stderr).trim().to_string(),
            });
        }
        if !output.status.success() {
            return Err(GitError::Git {
                args: "patch-id --verbatim".into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut fields = line.split_whitespace();
            if let (Some(patch), Some(commit)) = (fields.next(), fields.next()) {
                patch_ids.insert(commit.to_string(), patch.to_string());
            }
        }
        Ok(patch_ids)
    }

    /// Run git with `input` on stdin and return its stdout. The input is
    /// staged into an anonymous file so the child runner has no writer thread
    /// that could outlive the command deadline.
    fn run_git_with_stdin(&self, args: &[&str], commits: &[String]) -> Result<Vec<u8>, GitError> {
        self.run_git_with_stdin_within_output_limit(args, commits)?
            .ok_or_else(|| GitError::Git {
                args: args.join(" "),
                stderr: format!(
                    "Git batch output exceeded {MAX_GIT_BATCH_OUTPUT_BYTES} bytes per stream"
                ),
            })
    }

    /// [`Self::run_git_with_stdin`], with `None` for output past the batch
    /// limit so a caller holding a fallback can take it.
    fn run_git_with_stdin_within_output_limit(
        &self,
        args: &[&str],
        commits: &[String],
    ) -> Result<Option<Vec<u8>>, GitError> {
        let args_display = args.join(" ");
        let started = Instant::now();
        let deadline = effective_git_deadline(&args_display, git_timeout(), started)?;
        let input = git_batch_input(commits, deadline)
            .map_err(|error| git_batch_input_error(&args_display, started, deadline, error))?;
        let budget = remaining_git_budget(&args_display, deadline)?;
        let mut command = git_command();
        command
            .args(args)
            .current_dir(&self.root)
            .env("GIT_OPTIONAL_LOCKS", "0");
        let output = crate::bounded_output::output_with_input_limited(
            &mut command,
            input,
            budget,
            MAX_GIT_BATCH_OUTPUT_BYTES,
        )
        .map_err(|source| GitError::Spawn {
            args: args_display.clone(),
            source,
        })?;
        let output = match output {
            crate::bounded_output::LimitedOutput::Completed(output) => output,
            crate::bounded_output::LimitedOutput::Deadline => {
                note_git_timeout();
                return Err(GitError::timed_out_running(
                    args_display,
                    Some(&self.root),
                    started,
                    deadline_is_outer(deadline),
                    budget,
                ));
            }
            crate::bounded_output::LimitedOutput::OutputLimit => return Ok(None),
        };
        if !output.status.success() {
            return Err(GitError::Git {
                args: args_display,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(Some(output.stdout))
    }

    /// Verbatim patch id (`git patch-id --verbatim`) for the cumulative diff
    /// `from..to`, using the same pinned diff rendering as
    /// [`Self::first_parent_patch_ids`]. Empty diffs have no patch id and
    /// return `None`.
    pub fn patch_id_between(&self, from: &str, to: &str) -> Result<Option<String>, GitError> {
        let mut diff_args = vec!["diff"];
        diff_args.extend(patch_id_diff_args());
        diff_args.extend([from, to, "--"]);
        let diff_args_display = diff_args.join(" ");
        let mut command = git_command();
        command.args(&diff_args).current_dir(&self.root);
        let diff = command.output().map_err(|source| GitError::Spawn {
            args: diff_args_display.clone(),
            source,
        })?;
        if !diff.status.success() {
            return Err(GitError::Git {
                args: diff_args_display,
                stderr: String::from_utf8_lossy(&diff.stderr).trim().to_string(),
            });
        }
        if diff.stdout.is_empty() {
            return Ok(None);
        }
        let mut child = git_command()
            .args(PATCH_ID_ARGS)
            .current_dir(&self.root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|source| GitError::Spawn {
                args: "patch-id --verbatim".into(),
                source,
            })?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| GitError::Git {
                args: "patch-id --verbatim".into(),
                stderr: "failed to open patch-id stdin".into(),
            })?
            .write_all(&diff.stdout)
            .map_err(|source| GitError::Spawn {
                args: "patch-id --verbatim stdin".into(),
                source,
            })?;
        let output = child.wait_with_output().map_err(|source| GitError::Spawn {
            args: "patch-id --verbatim".into(),
            source,
        })?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: "patch-id --verbatim".into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .map(str::to_string))
    }

    /// Whether two commits have identical content for the supplied paths.
    ///
    /// Reads a path list, not an exit code, and that shape is the substance
    /// (#178). `git diff --quiet` answers in a status, and the #176 probe
    /// cannot audit a status -- it proves what a candidate writes to stdout,
    /// and an exit code is not stdout. A wrapper returning 0 where git
    /// returns 1 therefore passed that probe while telling this function that
    /// differing content was identical.
    ///
    /// That matters more here than at most call sites: this answer is the
    /// sole automatic evidence for classifying a promoted entry
    /// `SupersededUpstream`, and an unattended post-merge cleanup discards
    /// local work on the strength of it. So it is read the way `is_dirty` is
    /// read -- through the validated NUL grammar, and only once the machine's
    /// git has been believed.
    pub fn paths_equal(&self, left: &str, right: &str, paths: &[String]) -> Result<bool, GitError> {
        if paths.is_empty() {
            return Ok(true);
        }
        require_trustworthy_porcelain()?;
        // The sole automatic evidence for discarding a superseded promotion:
        // never answered on a cached probe verdict.
        ensure_fresh_git_verdict()?;
        const DIFF_ARGS: &str = "diff --literal-pathspecs --name-only --no-renames";
        const DIFF_COMMAND: &[&str] = &[
            "--literal-pathspecs",
            "diff",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=none",
            "-z",
        ];
        let started = Instant::now();
        let deadline = effective_git_deadline(DIFF_ARGS, git_timeout(), started)?;
        let _deadline = limit_git_until(deadline);

        let mut total_input_bytes = 0_usize;
        for path in paths {
            remaining_git_budget(DIFF_ARGS, deadline)?;
            let Some(path_bytes) = path.len().checked_add(1) else {
                return Err(GitError::Git {
                    args: DIFF_ARGS.into(),
                    stderr: "pathspec byte count overflowed".into(),
                });
            };
            let Some(next_total) = total_input_bytes.checked_add(path_bytes) else {
                return Err(GitError::Git {
                    args: DIFF_ARGS.into(),
                    stderr: "pathspec byte count overflowed".into(),
                });
            };
            if paths.len() > MAX_GIT_BATCH_QUERY_COUNT || next_total > MAX_GIT_BATCH_INPUT_BYTES {
                return Err(GitError::Git {
                    args: DIFF_ARGS.into(),
                    stderr: format!(
                        "pathspec input exceeded {MAX_GIT_BATCH_QUERY_COUNT} paths or {MAX_GIT_BATCH_INPUT_BYTES} bytes"
                    ),
                });
            }
            total_input_bytes = next_total;
        }

        let mut start = 0;
        while start < paths.len() {
            remaining_git_budget(DIFF_ARGS, deadline)?;
            let mut end = start;
            let mut batch_bytes = 0_usize;
            while end < paths.len() && end - start < MAX_LITERAL_PATHSPEC_BATCH_COUNT {
                let path_bytes = paths[end].len().saturating_add(1);
                if path_bytes > MAX_LITERAL_PATHSPEC_BATCH_BYTES {
                    return Err(GitError::Git {
                        args: DIFF_ARGS.into(),
                        stderr: format!(
                            "one pathspec exceeded {MAX_LITERAL_PATHSPEC_BATCH_BYTES} bytes"
                        ),
                    });
                }
                if batch_bytes.saturating_add(path_bytes) > MAX_LITERAL_PATHSPEC_BATCH_BYTES {
                    break;
                }
                batch_bytes += path_bytes;
                end += 1;
            }
            if end == start {
                return Err(GitError::Git {
                    args: DIFF_ARGS.into(),
                    stderr: "could not make progress while batching pathspecs".into(),
                });
            }

            let mut args = vec![
                "--literal-pathspecs",
                "diff",
                "--name-only",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=none",
                "-z",
                left,
                right,
                "--",
            ];
            args.extend(paths[start..end].iter().map(String::as_str));
            let out = run_git_bytes(&self.root, &args)?;
            if !parse_nul_paths_strict(&out, DIFF_COMMAND)?.is_empty() {
                return Ok(false);
            }
            start = end;
        }
        Ok(true)
    }

    /// Simulate merging `head` onto `base` without touching any worktree
    /// (`git merge-tree --write-tree`). Returns the resulting tree and
    /// the conflicted file list (empty = clean).
    pub fn merge_tree_simulate(&self, base: &str, head: &str) -> Result<MergeSimulation, GitError> {
        // Exit code 1 = conflicts (still writes a tree); >1 = real error.
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            base,
            head,
        ];
        let mut command = git_command();
        command.args(args).current_dir(&self.root);
        let output = run_git_process_output(command, &args, git_timeout())?;
        let code = output.status.code().unwrap_or(-1);
        if code != 0 && code != 1 {
            return Err(GitError::Git {
                args: "merge-tree --write-tree".into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        let tree = lines.next().unwrap_or_default().trim().to_string();
        let conflicts: Vec<String> = lines
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        Ok(MergeSimulation { tree, conflicts })
    }

    /// Replay `old_base..incoming` onto `current` using an explicit merge
    /// base. This preserves upstream follow-up fixes and never asks Git to
    /// infer history across rewritten (for example squash-merged) commits.
    pub fn merge_tree_with_base(
        &self,
        old_base: &str,
        current: &str,
        incoming: &str,
    ) -> Result<MergeSimulation, GitError> {
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "--merge-base",
            old_base,
            current,
            incoming,
        ];
        let mut command = git_command();
        command.args(args).current_dir(&self.root);
        let output = run_git_process_output(command, &args, git_timeout())?;
        let code = output.status.code().unwrap_or(-1);
        if code != 0 && code != 1 {
            return Err(GitError::Git {
                args: "merge-tree --write-tree --merge-base".into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        let tree = lines.next().unwrap_or_default().trim().to_string();
        let conflicts = lines
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        Ok(MergeSimulation { tree, conflicts })
    }

    /// Read `git config --get <key>`, or `None` when unset or empty.
    /// Missing config is an expected state here, not an error.
    pub fn config_value(&self, key: &str) -> Option<String> {
        run_git(&self.root, &["config", "--get", key])
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    /// Create a commit object for `tree` with `parents` (no checkout,
    /// no ref update). Used to materialize simulated merges.
    ///
    /// `attribution` fills the author and committer slots; the caller is
    /// responsible for appending any `Co-Authored-By` trailers to
    /// `message` (see [`crate::attribution::Attribution::trailer_block`]).
    pub fn commit_tree(
        &self,
        tree: &str,
        parents: &[&str],
        message: &str,
        attribution: &crate::attribution::Attribution,
    ) -> Result<String, GitError> {
        self.commit_tree_with_date(tree, parents, message, attribution, None)
    }

    pub(crate) fn commit_tree_with_date(
        &self,
        tree: &str,
        parents: &[&str],
        message: &str,
        attribution: &crate::attribution::Attribution,
        fixed_date: Option<&str>,
    ) -> Result<String, GitError> {
        let mut args: Vec<String> = vec!["commit-tree".into(), tree.into()];
        if fixed_date.is_some() {
            // Candidate construction must be repeatable and cannot prompt for
            // a signing key. A publishing workflow applies its own policy.
            args.push("--no-gpg-sign".into());
        }
        for parent in parents {
            args.push("-p".into());
            args.push((*parent).into());
        }
        args.push("-m".into());
        args.push(message.into());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut command = git_command();
        command
            .args(&arg_refs)
            .current_dir(&self.root)
            .env("GIT_AUTHOR_NAME", &attribution.author.name)
            .env("GIT_AUTHOR_EMAIL", &attribution.author.email)
            .env("GIT_COMMITTER_NAME", &attribution.committer.name)
            .env("GIT_COMMITTER_EMAIL", &attribution.committer.email);
        if let Some(date) = fixed_date {
            command
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date);
        }
        let output = command.output().map_err(|source| GitError::Spawn {
            args: "commit-tree".into(),
            source,
        })?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: "commit-tree".into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Read one commit's complete message without diff or decoration.
    pub fn commit_message(&self, commit: &str) -> Result<String, GitError> {
        run_git(&self.root, &["show", "-s", "--format=%B", commit])
    }

    /// Changed paths between two commits with Git's rename-aware projection.
    /// A rename is reported at its destination, preserving the lease and
    /// changed-path behavior expected by existing callers.
    pub fn changed_between(&self, from: &str, to: &str) -> Result<Vec<String>, GitError> {
        self.changed_between_with_rename_policy(from, to, "--find-renames")
    }

    /// Complete changed-path census with rename collapsing disabled. This is
    /// for content proofs that must account for both sides of a rename.
    pub(crate) fn changed_between_all_paths(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, GitError> {
        self.changed_between_with_rename_policy(from, to, "--no-renames")
    }

    /// A bounded complete path census for a local commit delta. Unlike the
    /// compatibility API above, this refuses when either the byte or path
    /// count cap is reached so a commit guard never treats a partial listing
    /// as an ownership proof.
    pub(crate) fn changed_between_until_with_limits(
        &self,
        from: &str,
        to: &str,
        deadline: Instant,
        max_output_bytes: usize,
        max_path_fields: usize,
    ) -> Result<Vec<String>, GitError> {
        require_trustworthy_porcelain()?;
        let args = [
            "--no-replace-objects",
            "diff",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=none",
            "-z",
            from,
            to,
            "--",
        ];
        let deadline = active_git_deadline().map_or(deadline, |active| active.min(deadline));
        ensure_bounded_git_listing_before(deadline, &args)?;
        if std::env::vars_os().any(|(key, _)| unsafe_session_commit_git_environment_key(&key)) {
            return Err(GitError::UntrustedOutput {
                detail: "inherited Git routing or configuration overrides make the commit path census unknown".into(),
            });
        }
        let mut command = git_command();
        command.args(args).current_dir(&self.root);
        let output =
            run_git_process_output_limited(&mut command, &args, deadline, max_output_bytes)?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let paths = parse_nul_paths_strict(&output.stdout, &args)?;
        if paths.len() > max_path_fields {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "git {} returned more than {max_path_fields} changed paths",
                    args.join(" ")
                ),
            });
        }
        ensure_bounded_git_listing_before(deadline, &args)?;
        Ok(paths)
    }

    fn changed_between_with_rename_policy(
        &self,
        from: &str,
        to: &str,
        rename_policy: &str,
    ) -> Result<Vec<String>, GitError> {
        require_trustworthy_porcelain()?;
        let args = [
            "diff",
            "--name-only",
            rename_policy,
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=none",
            "-z",
            from,
            to,
            "--",
        ];
        let out = run_git_bytes(&self.root, &args)?;
        parse_nul_paths_strict(&out, &args)
    }

    /// Changed files for gate trigger evaluation. Rename collapsing is
    /// disabled so both the removed and added path can select relevant gates.
    pub fn gate_scope_changed_between(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, GitError> {
        Ok(parse_nul_paths(&run_git(
            &self.root,
            &["diff", "--name-only", "--no-renames", "-z", from, to],
        )?))
    }

    /// Tracked paths in an exact committed tree, decoded through the shared
    /// NUL-safe path parser and returned in deterministic Git tree order.
    pub fn tracked_files_at(&self, commit: &str) -> Result<Vec<String>, GitError> {
        Ok(parse_nul_paths(&run_git(
            &self.root,
            &["ls-tree", "-r", "--name-only", "-z", commit],
        )?))
    }

    /// Files and gitlinks in an exact committed tree. Unlike `tracked_files_at`,
    /// this bounded form retains the object IDs and modes needed to check a
    /// partially removed worktree's surviving tracked payload.
    #[cfg(unix)]
    pub(crate) fn tracked_tree_files_at(
        &self,
        commit: &str,
        deadline: Instant,
    ) -> Result<Vec<GitTreeFile>, GitError> {
        let args = ["ls-tree", "-r", "--full-tree", "-z", commit];
        let mut command = git_command();
        command
            .args(args)
            .current_dir(&self.root)
            .env("GIT_OPTIONAL_LOCKS", "0");
        let output = run_git_process_output_limited(
            &mut command,
            &args,
            deadline,
            MAX_GIT_BATCH_OUTPUT_BYTES,
        )?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let invalid = || GitError::UntrustedOutput {
            detail: format!("git {} returned a malformed tracked tree", args.join(" ")),
        };
        let mut entries = Vec::new();
        for record in strict_git_listing_records(&output.stdout, &args)? {
            let tab = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(invalid)?;
            let mut fields = record[..tab].split(|byte| *byte == b' ');
            let mode = fields.next().ok_or_else(invalid)?;
            let object_type = fields.next().ok_or_else(invalid)?;
            let object_id = fields.next().ok_or_else(invalid)?;
            if fields.next().is_some()
                || !valid_tree_entry_mode_type(mode, object_type)
                || !valid_git_object_id(object_id)
            {
                return Err(invalid());
            }
            let path = std::str::from_utf8(&record[tab + 1..]).map_err(|_| invalid())?;
            if path.len() > MAX_IGNORED_PATH_BYTES {
                return Err(invalid());
            }
            validate_relative_git_path(path, &args)?;
            entries.push(GitTreeFile {
                path: path.to_owned(),
                mode: std::str::from_utf8(mode).map_err(|_| invalid())?.to_owned(),
                object_id: std::str::from_utf8(object_id)
                    .map_err(|_| invalid())?
                    .to_owned(),
            });
        }
        Ok(entries)
    }

    /// Inventory one exact tree before allocating an independent checkout.
    /// Output, entries and source bytes are bounded; incomplete inventories
    /// never authorize a copy. Gitlinks are unsupported in this local lane.
    pub(crate) fn graph_snapshot_inventory(&self, commit: &str) -> Result<(u64, u64), GitError> {
        graph_snapshot_object_format(commit)?;
        let bytes =
            run_isolated_graph_git(&self.root, &["ls-tree", "-r", "-t", "-l", "-z", commit])?;
        parse_graph_snapshot_inventory(&bytes)
    }

    /// Materialize one commit in independently owned metadata, without hooks,
    /// templates, inherited Git configuration, filters or linked registration.
    pub(crate) fn isolated_checkout_at(
        &self,
        destination: &Path,
        commit: &str,
    ) -> Result<GitRepo, GitError> {
        let invalid = |message| graph_snapshot_error(message);
        let object_format = graph_snapshot_object_format(commit)?;
        let source = self
            .root
            .to_str()
            .ok_or_else(|| invalid("non-UTF-8 source path"))?;
        let target = destination
            .to_str()
            .ok_or_else(|| invalid("non-UTF-8 target path"))?;
        if destination.exists() || std::fs::symlink_metadata(destination).is_ok() {
            return Err(invalid("graph snapshot destination must not exist"));
        }
        let object_format_arg = format!("--object-format={object_format}");
        run_isolated_graph_git(
            &self.root,
            &["init", "--quiet", "--template=", &object_format_arg, target],
        )?;
        run_isolated_graph_git(
            destination,
            &[
                "-c",
                "protocol.file.allow=always",
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-recurse-submodules",
                "--depth=1",
                "--",
                source,
                commit,
            ],
        )?;
        run_isolated_graph_git(
            destination,
            &[
                "checkout",
                "--quiet",
                "--no-recurse-submodules",
                "--detach",
                commit,
            ],
        )?;
        for path in [destination.to_path_buf(), destination.join(".git")] {
            let metadata = std::fs::symlink_metadata(&path).map_err(|source| GitError::Spawn {
                args: "inspect isolated graph checkout".into(),
                source,
            })?;
            if !metadata.file_type().is_dir() {
                return Err(invalid(
                    "isolated graph checkout must own ordinary Git metadata",
                ));
            }
        }
        let root = destination
            .canonicalize()
            .map_err(|source| GitError::Spawn {
                args: "canonicalize isolated graph checkout".into(),
                source,
            })?;
        Ok(GitRepo { root })
    }

    /// Create a detached worktree at `dest` checked out at `commit`.
    pub fn worktree_add_detached(&self, dest: &Path, commit: &str) -> Result<GitRepo, GitError> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|source| GitError::Spawn {
                args: format!("create_dir_all {}", parent.display()),
                source,
            })?;
        }
        run_git(
            &self.root,
            &[
                "worktree",
                "add",
                "--detach",
                dest.to_str().unwrap_or_default(),
                commit,
            ],
        )?;
        GitRepo::discover(dest)
    }

    /// Deterministic tree hash of the checkout's current source working state
    /// (HEAD + staged + unstaged + untracked, .gitignore respected) — the
    /// gate-result cache key. Untracked broker runtime paths are excluded so
    /// publishing a broker marker cannot invalidate its own cache key; tracked
    /// changes under those paths remain included. Uses a throwaway index so
    /// the real index is never touched; identical content in different
    /// worktrees hashes identically, which is what makes cross-agent gate
    /// dedup work.
    /// On Unix, each private-index Git child runs in an owned process group;
    /// completion also proves same-group clean-filter descendants are quiescent.
    pub fn working_tree_hash(&self) -> Result<String, GitError> {
        self.working_tree_hash_observation().result
    }

    pub(crate) fn working_tree_hash_observation(&self) -> WorkingTreeHashObservation {
        let mut writers_quiescent = true;
        let result = self.working_tree_hash_with_quiescence(&mut writers_quiescent);
        WorkingTreeHashObservation {
            result,
            writers_quiescent,
        }
    }

    /// [`Self::working_tree_hash`] timed as the span `site`, so each caller's
    /// share of tree hashing shows in the command's phases.
    pub(crate) fn working_tree_hash_at(&self, site: &'static str) -> Result<String, GitError> {
        let _span = crate::phase_telemetry::span(site);
        self.working_tree_hash()
    }

    /// The same tree as [`Self::working_tree_hash_observation`], for a
    /// checkout the broker itself just created from a commit and that only
    /// the broker has touched since (a submit verification slot).
    ///
    /// The full hash re-reads every tracked file because its private index
    /// starts from `read-tree HEAD`, with no stat data. This one starts the
    /// private index from a copy of the checkout's own index, with the copy's
    /// timestamp set to the original's, so `add -u` re-reads only entries
    /// whose stat data no longer matches or that Git's racy-clean rule
    /// (entry mtime not older than the index timestamp) makes suspect, as
    /// `git status` would. Git compares mtime, ctime, size, inode and owner;
    /// stat checking, trusting ctime and the default stat check are forced,
    /// and fsmonitor and the untracked cache are disabled. It answers only when it can prove
    /// the seeded index equals a fresh `read-tree HEAD` apart from stat data,
    /// and that no attribute or setting converts content between the files
    /// and Git (where a fresh checkout can re-hash to a different tree than
    /// the commit's): every other case, and any error, takes the full hash.
    /// Each outcome is counted (`tree_hash_seeded`,
    /// `tree_hash_seed_fallback_<reason>`).
    pub(crate) fn working_tree_hash_observation_for_fresh_checkout(
        &self,
    ) -> WorkingTreeHashObservation {
        let mut writers_quiescent = true;
        let result = match self.seeded_working_tree_hash(&mut writers_quiescent) {
            Ok(tree) => {
                crate::phase_telemetry::add("tree_hash_seeded", Duration::ZERO);
                Ok(tree)
            }
            Err(reason) => {
                crate::phase_telemetry::add(reason.counter(), Duration::ZERO);
                self.working_tree_hash_with_quiescence(&mut writers_quiescent)
            }
        };
        WorkingTreeHashObservation {
            result,
            writers_quiescent,
        }
    }

    fn seeded_working_tree_hash(
        &self,
        writers_quiescent: &mut bool,
    ) -> Result<String, SeedFallback> {
        if std::env::var_os(TREE_HASH_SEED_ENV).is_some_and(|value| value == "off") {
            return Err(SeedFallback::SwitchedOff);
        }
        let _span = crate::phase_telemetry::span("tree_hash_seeded_total");
        let index_dir = self
            .git_common_dir()
            .map_err(|_| SeedFallback::Unreadable)?
            .join("aethyme-broker/indexes");
        std::fs::create_dir_all(&index_dir).map_err(|_| SeedFallback::Unreadable)?;
        let tmp = index_dir.join(format!(
            "index-{}-{}.seed.tmp",
            std::process::id(),
            NEXT_PRIVATE_INDEX_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let lock = tmp.with_file_name(format!(
            "{}.lock",
            tmp.file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default()
        ));
        let result = self.seeded_working_tree_hash_in(&index_dir, &tmp, writers_quiescent);
        // Best effort, as for the full hash's private index: a leftover is a
        // stray file in the broker's index directory, never broker state.
        std::fs::remove_file(&tmp).ok();
        std::fs::remove_file(lock).ok();
        result
    }

    fn seeded_working_tree_hash_in(
        &self,
        index_dir: &Path,
        tmp: &Path,
        writers_quiescent: &mut bool,
    ) -> Result<String, SeedFallback> {
        let tmp_str = tmp.to_string_lossy().into_owned();
        let git = |args: &[&str], quiescent: &mut bool| -> Result<Vec<u8>, SeedFallback> {
            let mut full: Vec<&str> = SEEDED_GIT_CONFIG.to_vec();
            full.extend_from_slice(args);
            run_git_bytes_with_index_for_tree_hash(&self.root, &tmp_str, &full, quiescent)
                .map_err(|_| SeedFallback::GitFailed)
        };
        {
            let _span = crate::phase_telemetry::span("tree_hash_seed_index");
            let index = run_git(&self.root, &["rev-parse", "--git-path", "index"])
                .map_err(|_| SeedFallback::Unreadable)?;
            let index = {
                let path = PathBuf::from(index.trim());
                if path.is_absolute() {
                    path
                } else {
                    self.root.join(path)
                }
            };
            copy_index_with_its_timestamp(&index, tmp)?;
        }
        let checks = crate::phase_telemetry::span("tree_hash_seed_checks");
        // Entry flags: an assume-unchanged (lowercase tag) or skip-worktree
        // (`S`) entry is never refreshed by `add -u`.
        let listed = git(&["ls-files", "-v", "-z"], writers_quiescent)?;
        let mut paths: Vec<u8> = Vec::with_capacity(listed.len());
        let mut tracked = 0_u64;
        for record in listed
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let (Some(tag), Some(b' ')) = (record.first(), record.get(1)) else {
                return Err(SeedFallback::GitFailed);
            };
            if tag.is_ascii_lowercase() || *tag == b'S' {
                return Err(SeedFallback::EntryFlags);
            }
            paths.extend_from_slice(&record[2..]);
            paths.push(0);
            tracked += 1;
        }
        // Staged content: the seeded index must name exactly HEAD's tree.
        let index_tree = git(&["write-tree"], writers_quiescent)?;
        let head_tree = run_git(&self.root, &["rev-parse", "HEAD^{tree}"])
            .map_err(|_| SeedFallback::GitFailed)?;
        if String::from_utf8_lossy(&index_tree).trim() != head_tree.trim() {
            return Err(SeedFallback::IndexNotHead);
        }
        // Content conversion: with any of these, a file Git checked out can
        // re-hash to a different blob than the one recorded.
        let autocrlf = run_git(&self.root, &["config", "--get", "core.autocrlf"]).ok();
        if autocrlf
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty() && value != "false")
        {
            return Err(SeedFallback::ContentConversion);
        }
        if tracked > 0 && attributes_convert_content(&self.root, &tmp_str, &paths)? {
            return Err(SeedFallback::ContentConversion);
        }
        drop(checks);
        {
            let _span = crate::phase_telemetry::span("tree_hash_tracked_rehash");
            git(&["add", "-u", "--", "."], writers_quiescent)?;
        }
        let untracked = {
            let _span = crate::phase_telemetry::span("tree_hash_untracked_walk");
            git(
                &["ls-files", "--others", "--exclude-standard", "-z"],
                writers_quiescent,
            )?
        };
        let pathspecs =
            untracked_hash_pathspecs(&untracked).map_err(|_| SeedFallback::GitFailed)?;
        record_tree_hash_files(tracked, count_nul_records(&pathspecs));
        if !pathspecs.is_empty() {
            let _span = crate::phase_telemetry::span("tree_hash_untracked_add");
            let mut pathspec_file =
                tempfile::NamedTempFile::new_in(index_dir).map_err(|_| SeedFallback::Unreadable)?;
            pathspec_file
                .write_all(&pathspecs)
                .map_err(|_| SeedFallback::Unreadable)?;
            let pathspec_arg = format!("--pathspec-from-file={}", pathspec_file.path().display());
            git(
                &[
                    "--literal-pathspecs",
                    "add",
                    "-A",
                    &pathspec_arg,
                    "--pathspec-file-nul",
                ],
                writers_quiescent,
            )?;
        }
        let _span = crate::phase_telemetry::span("tree_hash_write_tree");
        let tree = git(&["write-tree"], writers_quiescent)?;
        let tree = String::from_utf8_lossy(&tree).trim().to_string();
        if tree.is_empty() {
            return Err(SeedFallback::GitFailed);
        }
        Ok(tree)
    }

    fn working_tree_hash_with_quiescence(
        &self,
        writers_quiescent: &mut bool,
    ) -> Result<String, GitError> {
        let index_dir = self.git_common_dir()?.join("aethyme-broker/indexes");
        std::fs::create_dir_all(&index_dir).map_err(|source| GitError::Spawn {
            args: format!("create_dir_all {}", index_dir.display()),
            source,
        })?;
        let tmp = index_dir.join(format!(
            "index-{}-{}.tmp",
            std::process::id(),
            NEXT_PRIVATE_INDEX_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp_str = tmp.to_string_lossy().into_owned();
        let lock = tmp.with_file_name(format!(
            "{}.lock",
            tmp.file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default()
        ));
        let result = (|| {
            {
                let _span = crate::phase_telemetry::span("tree_hash_index_build");
                run_git_with_index_for_tree_hash(
                    &self.root,
                    &tmp_str,
                    &["read-tree", "HEAD"],
                    writers_quiescent,
                )?;
            }
            let tracked = index_entry_count(&tmp).unwrap_or(0);
            // Capture tracked edits and deletions first, including anything
            // intentionally tracked beneath a runtime path. Enumerate only
            // Git-visible untracked files for the second pass, then filter
            // broker runtime paths in memory. Negative pathspecs after `.`
            // can make Git report an ignored parent directory such as
            // `.aethyme` itself, even when every ignored runtime child is
            // excluded.
            {
                let _span = crate::phase_telemetry::span("tree_hash_tracked_rehash");
                run_git_with_index_for_tree_hash(
                    &self.root,
                    &tmp_str,
                    &["add", "-u", "--", "."],
                    writers_quiescent,
                )?;
            }
            let untracked = {
                let _span = crate::phase_telemetry::span("tree_hash_untracked_walk");
                run_git_bytes_with_index_for_tree_hash(
                    &self.root,
                    &tmp_str,
                    &["ls-files", "--others", "--exclude-standard", "-z"],
                    writers_quiescent,
                )?
            };
            let pathspecs = untracked_hash_pathspecs(&untracked)?;
            record_tree_hash_files(tracked, count_nul_records(&pathspecs));
            if !pathspecs.is_empty() {
                let _span = crate::phase_telemetry::span("tree_hash_untracked_add");
                let mut pathspec_file =
                    tempfile::NamedTempFile::new_in(&index_dir).map_err(|source| {
                        GitError::Spawn {
                            args: format!("create pathspec file in {}", index_dir.display()),
                            source,
                        }
                    })?;
                pathspec_file
                    .write_all(&pathspecs)
                    .map_err(|source| GitError::Spawn {
                        args: format!("write {}", pathspec_file.path().display()),
                        source,
                    })?;
                let pathspec_arg =
                    format!("--pathspec-from-file={}", pathspec_file.path().display());
                run_git_with_index_for_tree_hash(
                    &self.root,
                    &tmp_str,
                    &[
                        "--literal-pathspecs",
                        "add",
                        "-A",
                        &pathspec_arg,
                        "--pathspec-file-nul",
                    ],
                    writers_quiescent,
                )?;
            }
            let _span = crate::phase_telemetry::span("tree_hash_write_tree");
            run_git_with_index_for_tree_hash(
                &self.root,
                &tmp_str,
                &["write-tree"],
                writers_quiescent,
            )
        })();
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(lock);
        result
    }

    /// The committed change `from..to` as a zero-context unified diff, with
    /// rename detection off so a moved file reads as a removal plus an
    /// addition. Colour and external diff drivers are disabled explicitly: a
    /// decorated diff cannot be parsed into line ranges.
    pub fn unified_diff_zero(&self, from: &str, to: &str) -> Result<String, GitError> {
        run_git(
            &self.root,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-renames",
                "-U0",
                from,
                to,
                "--",
            ],
        )
    }

    /// Tracked-file changes (committed + staged + unstaged) against
    /// `base`, plus untracked files: the diff surface implicit leases are
    /// derived from. NUL-separated output because shim-decorated
    /// line output was observed minting leases named `""` and
    /// `"--- Changes ---"` (2026-07-17 operational data).
    pub fn changed_files(&self, base: &str) -> Result<Vec<String>, GitError> {
        let mut files =
            parse_nul_paths(&run_git(&self.root, &["diff", "--name-only", "-z", base])?);
        let untracked = run_git(
            &self.root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?;
        files.extend(parse_nul_paths(&untracked));
        files.sort();
        files.dedup();
        Ok(files)
    }

    /// Every file the checkout's working state consists of: tracked paths
    /// plus untracked, non-ignored ones, relative to the root. It is the
    /// same surface `working_tree_hash` hashes, so it names exactly the
    /// sources a gate verdict claims to be about. A tracked path deleted
    /// in the worktree is still listed; callers must tolerate its absence.
    pub fn working_state_files(&self) -> Result<Vec<String>, GitError> {
        let listed = run_git(
            &self.root,
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ],
        )?;
        let mut files = parse_nul_paths(&listed);
        files.sort();
        files.dedup();
        Ok(files)
    }

    /// Paths staged for the next commit (`git diff --cached --name-only`)
    /// — the selection surface of the derived pre-commit hook. Inherits
    /// GIT_INDEX_FILE when git set one, so partial commits see exactly
    /// what will be committed.
    pub fn staged_files(&self) -> Result<Vec<String>, GitError> {
        Ok(run_git(&self.root, &["diff", "--cached", "--name-only"])?
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Files changed by the HEAD commit — what the post-commit conflict
    /// radar compares against other sessions' leases. `--root` makes the
    /// initial commit report its files too.
    pub fn head_changed_files(&self) -> Result<Vec<String>, GitError> {
        Ok(parse_path_lines(&run_git(
            &self.root,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "--root",
                "HEAD",
            ],
        )?))
    }

    /// One-line summaries (short-sha + subject) of `from..to`, newest
    /// first, capped at `limit` — the submit preflight's "exactly what
    /// will be submitted" listing.
    pub fn commit_summaries(
        &self,
        from: &str,
        to: &str,
        limit: usize,
    ) -> Result<Vec<String>, GitError> {
        let range = format!("{from}..{to}");
        let n = format!("--max-count={limit}");
        let out = run_git(
            &self.root,
            &["log", "--oneline", "--no-decorate", &n, &range],
        )?;
        Ok(out.lines().map(str::to_string).collect())
    }

    /// Paths with uncommitted or untracked changes — the submit preflight
    /// warns about these because only committed work integrates.
    ///
    /// Refuses outright when no trustworthy `git` was found: this answer
    /// decides whether a worktree may be removed and whether a push may
    /// proceed, and "clean" from a wrapper that swallowed a line is the one
    /// wrong answer that costs work rather than disk. Submodule contents are
    /// included explicitly so user ignore settings cannot make them appear clean.
    pub fn dirty_paths(&self) -> Result<Vec<String>, GitError> {
        require_trustworthy_porcelain()?;
        Ok(parse_porcelain_entries_z(&run_git_bytes(
            &self.root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        )?)?
        .into_iter()
        .map(|(_, path)| path)
        .collect())
    }

    /// Gitlink paths in both the committed HEAD tree and every index stage.
    /// The archive stores superproject commits and patches, not child Git
    /// object stores, so cleanup must not treat a missing index entry as proof
    /// that no submodule content belongs to this checkout.
    pub(crate) fn tracked_submodule_paths(&self) -> Result<Vec<String>, GitError> {
        require_trustworthy_porcelain()?;
        const HEAD_ARGS: &[&str] = &["ls-tree", "-r", "--full-tree", "-z", "HEAD"];
        const INDEX_ARGS: &[&str] = &["ls-files", "--stage", "--full-name", "-z"];
        let deadline = Instant::now() + Duration::from_secs(15);
        let run = |args: &[&str]| -> Result<Vec<u8>, GitError> {
            let mut command = git_command();
            command
                .args(args)
                .current_dir(&self.root)
                .env("GIT_OPTIONAL_LOCKS", "0");
            let output = run_git_process_output_limited(
                &mut command,
                args,
                deadline,
                MAX_GIT_BATCH_OUTPUT_BYTES,
            )?;
            if !output.status.success() {
                return Err(GitError::Git {
                    args: args.join(" "),
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                });
            }
            Ok(output.stdout)
        };
        let mut paths = parse_tree_gitlinks(&run(HEAD_ARGS)?, HEAD_ARGS)?;
        paths.extend(parse_index_gitlinks(&run(INDEX_ARGS)?, INDEX_ARGS)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Whether this checkout or its repository-wide Git directory retains a
    /// submodule metadata root. Such metadata may hold child objects even if
    /// the corresponding gitlink was removed from HEAD and the index.
    pub(crate) fn has_submodule_git_metadata(&self) -> Result<bool, GitError> {
        let worktree_git_dir =
            worktree_git_dir(&self.root).ok_or_else(|| GitError::UntrustedOutput {
                detail:
                    "cannot establish the checkout Git directory for submodule metadata inspection"
                        .into(),
            })?;
        let mut git_dirs = vec![worktree_git_dir, self.git_common_dir()?];
        git_dirs.sort();
        git_dirs.dedup();
        for git_dir in git_dirs {
            let modules = git_dir.join("modules");
            match std::fs::symlink_metadata(&modules) {
                Ok(_) => return Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(GitError::Spawn {
                        args: format!("inspect {}", modules.display()),
                        source,
                    });
                }
            }
        }
        Ok(false)
    }

    /// Dirty paths and their untracked subset from one trustworthy status snapshot.
    pub(crate) fn dirty_and_untracked_paths(&self) -> Result<(Vec<String>, Vec<String>), GitError> {
        require_trustworthy_porcelain()?;
        let entries = parse_porcelain_entries_z(&run_git_bytes(
            &self.root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        )?)?;
        let mut dirty = Vec::with_capacity(entries.len());
        let mut untracked = Vec::new();
        for (status, path) in entries {
            if status == [b'?', b'?'] {
                untracked.push(path.clone());
            }
            dirty.push(path);
        }
        Ok((dirty, untracked))
    }

    pub(crate) fn has_only_unstaged_tracked_deletions_until_with_limits(
        &self,
        policy: &crate::AutoCleanupPolicy,
        deadline: Instant,
        max_output_bytes: usize,
        max_path_fields: usize,
    ) -> Result<bool, GitError> {
        let entries = self.cleanup_status_entries_until_with_limits(
            deadline,
            max_output_bytes,
            max_path_fields,
            true,
        )?;
        Ok(only_unstaged_tracked_deletions(&entries, |path| {
            crate::auto_cleanup::classify_ignored(&self.root, path, policy).is_ok()
        }))
    }

    /// Untracked paths from porcelain status. Used for the adoption-time
    /// foreign-file snapshot; keeping the status grammar here prevents
    /// output decoration from becoming "foreign" broker state.
    /// A commit holding this checkout's tracked working content for `paths`
    /// on top of HEAD, or `None` when none of them differs from HEAD.
    ///
    /// Nothing is stashed and no ref moves. The content is staged into a
    /// private copy of the index, because every index-writing command --
    /// `git stash create` included, even under `GIT_OPTIONAL_LOCKS=0` --
    /// refreshes the checkout's real index, and the broker reads that
    /// file's mtime as evidence the session's agent is working. Classifying
    /// another session's edits must not make an idle session look active.
    /// Whether this checkout is part-way through a merge, rebase,
    /// cherry-pick or revert, or holds unmerged index entries. Building a
    /// tree from such an index fails, so callers check this first.
    pub fn operation_in_progress(&self) -> Result<Option<InProgressOperation>, GitError> {
        let operation = worktree_operation_in_progress(&self.root);
        let unresolved_conflicts = !run_git(&self.root, &["ls-files", "--unmerged"])?
            .trim()
            .is_empty();
        Ok(
            (operation.is_some() || unresolved_conflicts).then_some(InProgressOperation {
                operation,
                unresolved_conflicts,
            }),
        )
    }

    pub fn working_state_commit(&self, paths: &[String]) -> Result<Option<String>, GitError> {
        let dirty = self.dirty_tracked_among(paths)?;
        if dirty.is_empty() {
            return Ok(None);
        }
        let real_index = run_git(&self.root, &["rev-parse", "--git-path", "index"])?;
        let real_index = self.root.join(real_index.trim());
        let scratch = tempfile::NamedTempFile::new().map_err(|source| GitError::Spawn {
            args: "working state index".into(),
            source,
        })?;
        std::fs::copy(&real_index, scratch.path()).map_err(|source| GitError::Spawn {
            args: "working state index".into(),
            source,
        })?;
        let index = scratch.path().to_string_lossy().into_owned();
        let mut add = vec!["add", "-u", "--"];
        add.extend(dirty.iter().map(String::as_str));
        run_git_with_index(&self.root, &index, &add)?;
        let tree = run_git_with_index(&self.root, &index, &["write-tree"])?;
        let output = git_command()
            .args([
                "commit-tree",
                tree.trim(),
                "-p",
                "HEAD",
                "-m",
                "aethyme: working state for overlap classification",
            ])
            .env("GIT_AUTHOR_NAME", "aethyme")
            .env("GIT_AUTHOR_EMAIL", "aethyme@localhost")
            .env("GIT_COMMITTER_NAME", "aethyme")
            .env("GIT_COMMITTER_EMAIL", "aethyme@localhost")
            .current_dir(&self.root)
            .output()
            .map_err(|source| GitError::Spawn {
                args: "commit-tree".into(),
                source,
            })?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: "commit-tree".into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    }

    /// Untracked, non-ignored paths via `ls-files`, which never writes the
    /// index (unlike `status`, which may refresh it).
    pub fn untracked_paths_readonly(&self) -> Result<Vec<String>, GitError> {
        Ok(parse_nul_paths(&run_git(
            &self.root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?))
    }

    /// Ignored roots via `ls-files`, with ignored directories collapsed. The
    /// listing is a cleanup proof: unsupported path names fail closed instead
    /// of being dropped by the display-oriented path parser. It uses the
    /// bounded listing runner and refuses incomplete output.
    pub fn ignored_paths_readonly(&self) -> Result<Vec<String>, GitError> {
        let deadline = Instant::now() + Duration::from_secs(15);
        Ok(self
            .ignored_paths_readonly_until(deadline)?
            .into_iter()
            .map(|entry| {
                if entry.directory_marker {
                    format!("{}/", entry.relative)
                } else {
                    entry.relative
                }
            })
            .collect())
    }

    /// Tracked paths whose working content differs from HEAD, limited to
    /// `paths`. Untracked files are not included.
    pub fn dirty_tracked_among(&self, paths: &[String]) -> Result<Vec<String>, GitError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec!["diff", "--name-only", "-z", "HEAD", "--"];
        args.extend(paths.iter().map(String::as_str));
        Ok(parse_nul_paths(&run_git(&self.root, &args)?))
    }

    /// Uncommitted work as `git status` shows it by default: each changed
    /// tracked path, and each untracked directory as a single entry.
    ///
    /// For reporting only. [`GitRepo::dirty_paths`] lists every untracked
    /// file because it decides whether a worktree may be removed; a count
    /// meant for a person must not turn one untracked build folder into
    /// thousands of "uncommitted files" that bury the edits worth seeing.
    pub fn uncommitted_summary(&self) -> Result<UncommittedSummary, GitError> {
        require_trustworthy_porcelain()?;
        let mut summary = UncommittedSummary::default();
        for (xy, path) in parse_porcelain_entries_z(&run_git_bytes(
            &self.root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=normal",
                "--ignore-submodules=none",
            ],
        )?)? {
            match xy {
                [b'?', b'?'] => summary.untracked.push(path),
                [b'!', b'!'] => {}
                _ => summary.modified.push(path),
            }
        }
        Ok(summary)
    }

    pub fn untracked_paths(&self) -> Result<Vec<String>, GitError> {
        Ok(parse_porcelain_entries_z(&run_git_bytes(
            &self.root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        )?)?
        .into_iter()
        .filter(|(xy, _path)| *xy == [b'?', b'?'])
        .map(|(_xy, path)| path)
        .collect())
    }

    pub(crate) fn dirty_and_untracked_paths_until_with_limits(
        &self,
        deadline: Instant,
        max_output_bytes: usize,
        max_path_fields: usize,
    ) -> Result<(Vec<String>, Vec<String>), GitError> {
        let entries = self.cleanup_status_entries_until_with_limits(
            deadline,
            max_output_bytes,
            max_path_fields,
            false,
        )?;
        let mut dirty = Vec::with_capacity(entries.len());
        let mut untracked = Vec::new();
        for (status, path) in entries {
            if status == [b'?', b'?'] {
                untracked.push(path.clone());
            }
            dirty.push(path);
        }
        Ok((dirty, untracked))
    }

    fn cleanup_status_entries_until_with_limits(
        &self,
        deadline: Instant,
        max_output_bytes: usize,
        max_path_fields: usize,
        include_matching_ignored: bool,
    ) -> Result<Vec<([u8; 2], String)>, GitError> {
        let mut args = vec![
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ];
        if include_matching_ignored {
            args.push("--ignored=matching");
        }
        let deadline = active_git_deadline().map_or(deadline, |active| active.min(deadline));
        ensure_bounded_git_listing_before(deadline, &args)?;
        if std::env::vars_os().any(|(key, _)| unsafe_graph_git_environment_key(&key)) {
            return Err(GitError::UntrustedOutput {
                detail: "inherited Git routing overrides make cleanup status unknown".into(),
            });
        }
        require_trustworthy_porcelain()?;
        ensure_bounded_git_listing_before(deadline, &args)?;
        let mut command = ignored_paths_git_command(&self.root, &args);
        let output =
            run_git_process_output_limited(&mut command, &args, deadline, max_output_bytes)?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        let entries = parse_porcelain_entries_z_with_path_limit(&output.stdout, max_path_fields)?;
        ensure_bounded_git_listing_before(deadline, &args)?;
        Ok(entries)
    }

    pub(crate) fn untracked_paths_until_with_limits(
        &self,
        deadline: Instant,
        max_output_bytes: usize,
        max_path_fields: usize,
    ) -> Result<Vec<String>, GitError> {
        self.dirty_and_untracked_paths_until_with_limits(
            deadline,
            max_output_bytes,
            max_path_fields,
        )
        .map(|(_, untracked)| untracked)
    }

    pub(crate) fn is_dirty_until_with_limits(
        &self,
        deadline: Instant,
        max_output_bytes: usize,
        max_path_fields: usize,
    ) -> Result<bool, GitError> {
        self.dirty_and_untracked_paths_until_with_limits(
            deadline,
            max_output_bytes,
            max_path_fields,
        )
        .map(|(dirty, _)| !dirty.is_empty())
    }

    /// A binary-safe patch of this checkout's staged (`cached`) or unstaged
    /// changes, in the form `git apply` reads back.
    ///
    /// The prefixes, rename detection and diff drivers are pinned rather than
    /// inherited: a user's `diff.noprefix`, `diff.external` or textconv filter
    /// changes what `git diff` prints, and a recovery patch has to apply on a
    /// machine that does not share that configuration. Submodule visibility is
    /// pinned too, so a local ignore setting cannot hide a gitlink pointer change.
    /// Each capture is capped at 64 MiB; larger patches fail closed without
    /// returning truncated output.
    pub fn recovery_patch(&self, cached: bool) -> Result<Vec<u8>, GitError> {
        self.recovery_patch_with_output_limit(cached, MAX_RECOVERY_PATCH_OUTPUT_BYTES)
    }

    fn recovery_patch_with_output_limit(
        &self,
        cached: bool,
        max_output_bytes: usize,
    ) -> Result<Vec<u8>, GitError> {
        let mut args = vec![
            "-c",
            "diff.noprefix=false",
            "-c",
            "diff.mnemonicPrefix=false",
            "diff",
        ];
        args.extend(PINNED_DIFF_FORMAT_ARGS);
        args.extend([
            "--binary",
            "--full-index",
            "--no-renames",
            "--ignore-submodules=none",
        ]);
        if cached {
            args.push("--cached");
        }
        let mut command = git_command();
        command
            .args(&args)
            .current_dir(&self.root)
            .env("GIT_OPTIONAL_LOCKS", "0");
        let output = run_git_process_output_limited(
            &mut command,
            &args,
            Instant::now()
                .checked_add(git_timeout())
                .unwrap_or_else(Instant::now),
            max_output_bytes,
        )?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(output.stdout)
    }

    /// Commits reachable from `head` that none of `excluded` holds, newest
    /// first. These are what a checkout's removal would take with it.
    pub fn commits_not_on_any(
        &self,
        head: &str,
        excluded: &[String],
    ) -> Result<Vec<String>, GitError> {
        let mut args = vec!["rev-list", head, "--not"];
        args.extend(excluded.iter().map(String::as_str));
        Ok(run_git(&self.root, &args)?
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Write a bundle holding `HEAD` minus everything `excluded` holds, so it
    /// restores into any clone that has those commits.
    pub fn create_recovery_bundle(&self, path: &Path, excluded: &[String]) -> Result<(), GitError> {
        let path = path.to_string_lossy();
        let mut args = vec!["bundle", "create", path.as_ref(), "HEAD", "--not"];
        args.extend(excluded.iter().map(String::as_str));
        run_git(&self.root, &args).map(|_| ())
    }

    /// Read a bundle back -- its format, and that every prerequisite commit
    /// exists in this repository -- and return the commits it carries as tips.
    pub fn verify_bundle(&self, path: &Path) -> Result<Vec<String>, GitError> {
        let path = path.to_string_lossy();
        run_git(&self.root, &["bundle", "verify", "--quiet", path.as_ref()])?;
        Ok(
            run_git(&self.root, &["bundle", "list-heads", path.as_ref()])?
                .lines()
                .filter_map(|line| line.split_whitespace().next())
                .map(str::to_string)
                .collect(),
        )
    }

    /// Prove that `staged` and then `unstaged` apply on top of `head`, using a
    /// private index so neither this checkout's index nor its files move.
    ///
    /// The order mirrors how the patches were taken: `git diff --cached` is
    /// HEAD to index, and `git diff` is index to working tree.
    pub fn check_recovery_patches(
        &self,
        head: &str,
        staged: Option<&Path>,
        unstaged: Option<&Path>,
        scratch_index: &Path,
    ) -> Result<(), GitError> {
        let index = scratch_index.to_string_lossy();
        run_git_with_index(&self.root, &index, &["read-tree", head])?;
        if let Some(staged) = staged {
            run_git_with_index(
                &self.root,
                &index,
                &["apply", "--cached", &staged.to_string_lossy()],
            )?;
        }
        if let Some(unstaged) = unstaged {
            run_git_with_index(
                &self.root,
                &index,
                &["apply", "--cached", "--check", &unstaged.to_string_lossy()],
            )?;
        }
        Ok(())
    }

    /// Tracked paths with staged or unstaged changes. Kept separate from
    /// untracked paths for operations that can safely tolerate unrelated
    /// local files while still refusing modifications to committed state.
    pub fn tracked_dirty_paths(&self) -> Result<Vec<String>, GitError> {
        Ok(parse_porcelain_entries_z(&run_git_bytes(
            &self.root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        )?)?
        .into_iter()
        .filter(|(xy, _path)| *xy != [b'?', b'?'])
        .map(|(_xy, path)| path)
        .collect())
    }

    /// Whether one repository-relative path is present in the committed/staged
    /// index. An untracked gate definition cannot exist in a spawned worktree.
    pub fn is_tracked(&self, path: &str) -> Result<bool, GitError> {
        Ok(!run_git(&self.root, &["ls-files", "--cached", "-z", "--", path])?.is_empty())
    }

    /// Fetch a local commit object into this worktree's repository. Used
    /// by broker repair to apply the same no-network path written into
    /// `.aethyme/broker-action-required.md`.
    pub fn fetch_local_commit(&self, commit: &str) -> Result<(), GitError> {
        run_git(&self.root, &["fetch", ".", commit])?;
        Ok(())
    }

    /// Rebase this checkout onto `base`. If git stops for conflicts, the
    /// caller receives the stderr and the worktree is intentionally left
    /// in the paused rebase state for manual resolution.
    pub fn rebase_onto(&self, base: &str) -> Result<(), GitError> {
        run_git(&self.root, &["rebase", base])?;
        Ok(())
    }

    /// Merge `commit` into the checked-out branch with `message`, never
    /// opening an editor. A conflict leaves the merge in progress for the
    /// caller to abort.
    pub fn merge_commit_no_edit(&self, commit: &str, message: &str) -> Result<(), GitError> {
        run_git(&self.root, &["merge", "--no-edit", "-m", message, commit])?;
        Ok(())
    }

    /// Fetch exactly `refs/heads/<branch>` from `remote` into
    /// `refs/remotes/<remote>/<branch>`, within `budget`. A read that moves
    /// one remote-tracking ref; no tags, no other refs.
    pub fn fetch_branch_into_tracking_ref(
        &self,
        remote: &str,
        branch: &str,
        budget: Duration,
    ) -> Result<(), GitError> {
        let _span = crate::phase_telemetry::span("git_fetch");
        let refspec = format!("refs/heads/{branch}:refs/remotes/{remote}/{branch}");
        let args = ["fetch", "--no-tags", "--quiet", remote, refspec.as_str()];
        let mut command = git_command();
        command.args(args).current_dir(&self.root);
        run_git_command(command, &args, budget)?;
        Ok(())
    }

    /// Abandon a paused rebase, restoring the branch it started from.
    pub fn abort_rebase(&self) -> Result<(), GitError> {
        run_git(&self.root, &["rebase", "--abort"])?;
        Ok(())
    }

    /// Abandon a paused merge, restoring the pre-merge state.
    pub fn abort_merge(&self) -> Result<(), GitError> {
        run_git(&self.root, &["merge", "--abort"])?;
        Ok(())
    }

    /// Seconds since `reference` last moved, from its reflog, falling back
    /// to the age of `FETCH_HEAD`. `None` when neither is recorded.
    pub fn ref_age_seconds(&self, reference: &str) -> Option<u64> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        let from_reflog = run_git(
            &self.root,
            &[
                "reflog",
                "show",
                "-1",
                "--date=unix",
                "--format=%gd",
                reference,
            ],
        )
        .ok()
        .and_then(|line| {
            let start = line.rfind('{')? + 1;
            let end = line.rfind('}')?;
            line.get(start..end)?.parse::<u64>().ok()
        });
        let moved = from_reflog.or_else(|| {
            let path = run_git(&self.root, &["rev-parse", "--git-path", "FETCH_HEAD"]).ok()?;
            let path = std::path::PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                self.root.join(path)
            };
            std::fs::metadata(path)
                .ok()?
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|since| since.as_secs())
        })?;
        Some(now.saturating_sub(moved))
    }

    /// Replay exactly `upstream..HEAD` onto `base`. Unlike a plain
    /// `git rebase <base>`, this never lets Git infer an older merge-base
    /// and accidentally include commits that predate the broker session.
    pub fn rebase_onto_range(&self, base: &str, upstream: &str) -> Result<(), GitError> {
        run_git(&self.root, &["rebase", "--onto", base, upstream])?;
        Ok(())
    }

    /// True when the checkout has uncommitted changes or untracked files —
    /// the guard `broker cleanup` consults before removing a worktree.
    ///
    /// Deliberately expressed as "are there any dirty paths" rather than "is
    /// the output non-empty". The raw-emptiness spelling was the last
    /// porcelain reader in this module that did not go through the validated
    /// grammar, and it is what made six bytes of wrapper decoration read as a
    /// dirty worktree -- every worktree on the machine, permanently
    /// ineligible for cleanup (#176).
    pub fn is_dirty(&self) -> Result<bool, GitError> {
        Ok(!self.dirty_paths()?.is_empty())
    }

    /// Commits reachable from HEAD that no remote-tracking ref contains.
    ///
    /// Zero means every commit here exists somewhere else, so the checkout can
    /// be removed without losing history. Non-zero is the count of commits that
    /// would go with it.
    pub fn commits_not_on_any_remote(&self) -> Result<usize, GitError> {
        let count = run_git(
            &self.root,
            &["rev-list", "--count", "HEAD", "--not", "--remotes"],
        )?;
        parse_commit_count(&count)
    }

    /// Unix seconds of HEAD's commit time, for reporting how long a checkout
    /// has sat untouched.
    pub fn head_committed_at(&self) -> Result<i64, GitError> {
        let text = run_git(&self.root, &["log", "-1", "--format=%ct", "HEAD"])?;
        Ok(text.trim().parse().unwrap_or(0))
    }

    /// Commits on HEAD that are not reachable from `base` — cleanup
    /// refuses to remove a worktree whose work was never merged.
    pub fn unmerged_commit_count(&self, base: &str) -> Result<u64, GitError> {
        let count = run_git(
            &self.root,
            &["rev-list", "--count", &format!("{base}..HEAD")],
        )?;
        parse_commit_count(&count)
    }

    /// Commits reachable from `head` that no remote-tracking ref holds, newest
    /// first. Read-only and as fresh as the last fetch or push: it answers
    /// "what exists only on this machine", not "what the remote has now".
    pub fn commits_not_on_remotes(&self, head: &str) -> Result<Vec<LoggedCommit>, GitError> {
        self.logged_commits(&[head, "--not", "--remotes"])
    }

    /// [`GitRepo::commits_not_on_remotes`] without merges and without any
    /// commit `excluded` reaches: the commits a session made itself rather
    /// than inherited from its base.
    pub fn own_commits_not_on_remotes(
        &self,
        head: &str,
        excluded: &[&str],
    ) -> Result<Vec<LoggedCommit>, GitError> {
        let mut range = vec!["--no-merges", head, "--not", "--remotes"];
        range.extend_from_slice(excluded);
        self.logged_commits(&range)
    }

    /// Commits reachable from `head` but not from `base`, newest first.
    pub fn commits_between(&self, base: &str, head: &str) -> Result<Vec<LoggedCommit>, GitError> {
        self.logged_commits(&[&format!("{base}..{head}")])
    }

    fn logged_commits(&self, range: &[&str]) -> Result<Vec<LoggedCommit>, GitError> {
        let mut args = vec!["log", "--format=%H%x1f%ct%x1f%s"];
        args.extend_from_slice(range);
        Ok(run_git(&self.root, &args)?
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\u{1f}');
                let sha = fields.next()?.to_string();
                let seconds = fields.next()?.parse::<i64>().ok()?;
                Some(LoggedCommit {
                    sha,
                    committed_at_ms: seconds.saturating_mul(1000),
                    subject: fields.next().unwrap_or_default().to_string(),
                })
            })
            .collect())
    }

    /// Commits reachable from `to` but not from `from`. Used by status
    /// surfaces that compare named refs instead of this checkout's HEAD.
    pub fn commit_count_between(&self, from: &str, to: &str) -> Result<u64, GitError> {
        let count = run_git(
            &self.root,
            &["rev-list", "--count", &format!("{from}..{to}")],
        )?;
        parse_commit_count(&count)
    }

    /// Commits unique to each side of a pair of revisions, respectively.
    pub(crate) fn symmetric_commit_counts(
        &self,
        left: &str,
        right: &str,
    ) -> Result<(u64, u64), GitError> {
        let counts = run_git(
            &self.root,
            &[
                "rev-list",
                "--left-right",
                "--count",
                &format!("{left}...{right}"),
            ],
        )?;
        parse_symmetric_commit_counts(&counts)
    }

    /// Non-merge commits on `head` since it forked from `target` whose patch
    /// `target` does not already carry. Zero means every change was applied
    /// to `target` under another SHA -- a rebase or cherry-pick delivery that
    /// ancestry cannot see.
    pub fn patch_unique_commit_count(&self, target: &str, head: &str) -> Result<u64, GitError> {
        let count = run_git(
            &self.root,
            &[
                "rev-list",
                "--count",
                "--right-only",
                "--cherry-pick",
                "--no-merges",
                &format!("{target}...{head}"),
            ],
        )?;
        parse_commit_count(&count)
    }

    /// Create a linked worktree at `dest` on new branch `branch` starting
    /// from `base`, returning a handle on it.
    pub fn worktree_add(&self, dest: &Path, branch: &str, base: &str) -> Result<GitRepo, GitError> {
        self.worktree_add_full(dest, branch, base)
    }

    // Respect an operator's effective Git setting. Only supply a bounded
    // command-local default when configuration is absent and the host has
    // enough CPUs; never write repository or global configuration.
    fn checkout_worker_default(&self, available_cpus: usize) -> Option<&'static str> {
        if available_cpus < 4 {
            return None;
        }
        checkout_worker_default_from_config(
            run_git(&self.root, &["config", "--get", "checkout.workers"]),
            available_cpus,
        )
    }

    fn worktree_add_full(
        &self,
        dest: &Path,
        branch: &str,
        base: &str,
    ) -> Result<GitRepo, GitError> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|source| GitError::Spawn {
                args: format!("create_dir_all {}", parent.display()),
                source,
            })?;
        }
        let available_cpus = std::thread::available_parallelism().map_or(1, usize::from);
        let mut args = Vec::new();
        if let Some(default) = self.checkout_worker_default(available_cpus) {
            args.extend(["-c", default]);
        }
        args.extend([
            "worktree",
            "add",
            "-b",
            branch,
            dest.to_str().unwrap_or_default(),
            base,
        ]);
        run_git(&self.root, &args)?;
        GitRepo::discover(dest)
    }

    /// Create a linked worktree, optionally materializing only the requested
    /// cone-mode directories. Sparse setup is intentionally scoped to a fresh
    /// worktree: callers must not use this for reuse or adoption.
    pub(crate) fn worktree_add_with_sparse_paths(
        &self,
        dest: &Path,
        branch: &str,
        base: &str,
        sparse_paths: &[String],
    ) -> Result<(GitRepo, Option<String>), GitError> {
        self.worktree_add_with_sparse_paths_using(
            dest,
            branch,
            base,
            sparse_paths,
            |worktree, paths| {
                let mut args = vec![
                    "sparse-checkout".to_string(),
                    "set".to_string(),
                    "--cone".to_string(),
                    "--no-sparse-index".to_string(),
                    "--".to_string(),
                ];
                args.extend(paths.iter().cloned());
                let args = args.iter().map(String::as_str).collect::<Vec<_>>();
                run_git(worktree, &args).map(|_| ())
            },
        )
    }

    fn worktree_add_with_sparse_paths_using(
        &self,
        dest: &Path,
        branch: &str,
        base: &str,
        sparse_paths: &[String],
        apply_sparse: impl FnOnce(&Path, &[String]) -> Result<(), GitError>,
    ) -> Result<(GitRepo, Option<String>), GitError> {
        if sparse_paths.is_empty() {
            return self
                .worktree_add_full(dest, branch, base)
                .map(|repo| (repo, None));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|source| GitError::Spawn {
                args: format!("create_dir_all {}", parent.display()),
                source,
            })?;
        }
        run_git(
            &self.root,
            &[
                "worktree",
                "add",
                "--no-checkout",
                "-b",
                branch,
                dest.to_str().unwrap_or_default(),
                base,
            ],
        )?;

        // `--no-checkout` must leave an empty index and no user content before
        // the sparse command is allowed to change its rules. In particular,
        // Git may remove ignored files when the cone narrows.
        self.verify_fresh_sparse_worktree_with_empty_index(dest, branch, base)
            .map_err(|error| {
                Self::sparse_recovery_retained_error(
                    dest,
                    branch,
                    base,
                    format!(
                        "refusing sparse setup because the fresh worktree is not pristine: {error}"
                    ),
                )
            })?;

        let sparse_result = apply_sparse(dest, sparse_paths)
            .and_then(|()| self.materialize_fresh_sparse_worktree(dest, branch, base));
        match sparse_result {
            Ok(worktree) => Ok((worktree, None)),
            Err(sparse_error) => {
                let reason = sparse_error.to_string();
                match self.restore_full_checkout_after_sparse_failure(dest, branch, base) {
                    Ok(repo) => Ok((repo, Some(reason))),
                    Err(recovery_error) => Err(GitError::Git {
                        args: "sparse-checkout recovery".into(),
                        stderr: format!(
                            "sparse setup failed: {sparse_error}; full-checkout recovery failed: {recovery_error}"
                        ),
                    }),
                }
            }
        }
    }

    /// Check that a path is a tracked directory in a specific commit. Only a
    /// tree object is accepted, so symlinks and submodules cannot be mistaken
    /// for directories when validating sparse configuration.
    pub(crate) fn commit_has_directory(
        &self,
        commit: &str,
        relative_path: &str,
    ) -> Result<bool, GitError> {
        let object = format!("{commit}:{relative_path}");
        match run_git(&self.root, &["cat-file", "-t", &object]) {
            Ok(object_type) => Ok(object_type == "tree"),
            // A missing path, symlink blob or gitlink is not a sparse root.
            // Treat Git's object lookup refusal as a safe full-checkout
            // fallback; process and timeout errors remain visible to callers.
            Err(GitError::Git { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn verify_fresh_sparse_recovery_identity(
        &self,
        dest: &Path,
        branch: &str,
        base: &str,
    ) -> Result<GitRepo, GitError> {
        let expected_base = self.resolve_commit_checked(base)?;
        let expected_branch = format!("refs/heads/{branch}");
        let destination = dest.canonicalize().map_err(|source| GitError::Spawn {
            args: format!("canonicalize sparse recovery path {}", dest.display()),
            source,
        })?;
        let inventory = self.worktree_inventory()?;
        let matching = inventory
            .iter()
            .filter(|entry| {
                entry.path == dest
                    || entry.path.canonicalize().ok().as_deref() == Some(destination.as_path())
            })
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "expected one registered worktree at {}, found {}",
                    dest.display(),
                    matching.len()
                ),
            });
        }
        let entry = matching[0];
        if entry.prunable
            || entry.bare
            || entry.detached
            || entry.locked
            || entry.branch.as_deref() != Some(expected_branch.as_str())
            || entry.head.as_deref() != Some(expected_base.as_str())
        {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "worktree at {} no longer has branch {expected_branch} at {expected_base}",
                    dest.display()
                ),
            });
        }
        let branch_tip = run_git(&self.root, &["rev-parse", "--verify", &expected_branch])?;
        if branch_tip != expected_base {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "branch {expected_branch} moved from {expected_base} to {branch_tip}"
                ),
            });
        }

        let worktree = GitRepo::discover(dest)?;
        let discovered_root = worktree
            .root()
            .canonicalize()
            .map_err(|source| GitError::Spawn {
                args: format!(
                    "canonicalize discovered worktree {}",
                    worktree.root().display()
                ),
                source,
            })?;
        if discovered_root != destination {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "Git discovered {} for sparse recovery path {}",
                    discovered_root.display(),
                    dest.display()
                ),
            });
        }
        let expected_common =
            self.git_common_dir()?
                .canonicalize()
                .map_err(|source| GitError::Spawn {
                    args: "canonicalize repository git common directory".into(),
                    source,
                })?;
        let actual_common = worktree
            .git_common_dir()?
            .canonicalize()
            .map_err(|source| GitError::Spawn {
                args: "canonicalize sparse worktree git common directory".into(),
                source,
            })?;
        if actual_common != expected_common
            || worktree.current_branch()? != branch
            || worktree.head_commit()? != expected_base
        {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "discovered checkout at {} is not the expected branch {expected_branch} at {expected_base}",
                    dest.display()
                ),
            });
        }

        Ok(worktree)
    }

    fn verify_sparse_worktree_has_no_untracked_or_ignored(
        worktree: &GitRepo,
        dest: &Path,
    ) -> Result<(), GitError> {
        let untracked_paths = worktree.untracked_paths_readonly()?;
        if !untracked_paths.is_empty() {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "worktree at {} has untracked files ({})",
                    dest.display(),
                    untracked_paths.len()
                ),
            });
        }
        let ignored_paths = worktree.ignored_paths_readonly()?;
        if !ignored_paths.is_empty() {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "worktree at {} has ignored files that recovery could overwrite ({})",
                    dest.display(),
                    ignored_paths.len()
                ),
            });
        }
        Ok(())
    }

    fn verify_fresh_sparse_recovery_worktree(
        &self,
        dest: &Path,
        branch: &str,
        base: &str,
    ) -> Result<GitRepo, GitError> {
        let worktree = self.verify_fresh_sparse_recovery_identity(dest, branch, base)?;
        let dirty_paths = worktree.dirty_paths()?;
        if !dirty_paths.is_empty() {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "worktree at {} has tracked or untracked changes ({})",
                    dest.display(),
                    dirty_paths.len()
                ),
            });
        }
        Self::verify_sparse_worktree_has_no_untracked_or_ignored(&worktree, dest)?;
        Ok(worktree)
    }

    /// Prove that a `worktree add --no-checkout` result is still the exact
    /// fresh branch at `base`, with no index entries or user-created files.
    /// This is the precondition for sparse rules that can remove ignored paths.
    fn verify_fresh_sparse_worktree_with_empty_index(
        &self,
        dest: &Path,
        branch: &str,
        base: &str,
    ) -> Result<GitRepo, GitError> {
        let worktree = self.verify_fresh_sparse_recovery_identity(dest, branch, base)?;
        let index_entries = run_git_bytes(worktree.root(), &["ls-files", "--stage", "-z"])?;
        if !index_entries.is_empty() {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "fresh --no-checkout worktree at {} has a non-empty index before materialization",
                    dest.display()
                ),
            });
        }
        Self::verify_sparse_worktree_has_no_untracked_or_ignored(&worktree, dest)?;
        Ok(worktree)
    }

    fn verify_sparse_recovery_index_matches_base(
        &self,
        worktree: &GitRepo,
        base: &str,
    ) -> Result<(), GitError> {
        let expected_tree = run_git(
            &self.root,
            &["rev-parse", "--verify", &format!("{base}^{{tree}}")],
        )?;
        let index_tree = run_git(worktree.root(), &["write-tree"])?;
        if index_tree != expected_tree {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "worktree at {} has an index tree different from expected base {base}",
                    worktree.root().display()
                ),
            });
        }
        Ok(())
    }

    /// Populate the sparse index from the exact branch head without asking
    /// Git to update the worktree. Sparse paths are selected with Git's own
    /// matcher, then `checkout-index` copies only missing included paths with
    /// no force option so a racing file cannot be overwritten.
    fn materialize_fresh_sparse_worktree(
        &self,
        dest: &Path,
        branch: &str,
        base: &str,
    ) -> Result<GitRepo, GitError> {
        let worktree = self.verify_fresh_sparse_worktree_with_empty_index(dest, branch, base)?;
        let expected_base = self.resolve_commit_checked(base)?;
        let tracked_paths = run_git_bytes(
            worktree.root(),
            &["ls-tree", "-r", "--name-only", "-z", &expected_base],
        )?;
        let tracked_paths = nul_terminated_paths(&tracked_paths, "ls-tree HEAD")?;
        let included_paths = run_git_bytes_with_stdin(
            worktree.root(),
            &["sparse-checkout", "check-rules", "-z"],
            &nul_path_list(&tracked_paths),
        )?;
        let included_paths = nul_terminated_paths(&included_paths, "sparse-checkout check-rules")?;
        let tracked = tracked_paths.iter().copied().collect::<BTreeSet<_>>();
        let included = included_paths.iter().copied().collect::<BTreeSet<_>>();
        if included.iter().any(|path| !tracked.contains(path)) {
            return Err(GitError::UntrustedOutput {
                detail: "sparse-checkout matcher returned a path outside the target tree".into(),
            });
        }
        let mut excluded_bytes = Vec::new();
        for path in &tracked_paths {
            if !included.contains(path) {
                excluded_bytes.extend_from_slice(path);
                excluded_bytes.push(0);
            }
        }

        // A plain read-tree only changes the index. In particular, it does
        // not perform the ignored-file overwrites that read-tree -u permits.
        self.verify_fresh_sparse_worktree_with_empty_index(dest, branch, base)?;
        run_git(worktree.root(), &["read-tree", &expected_base])?;
        if !excluded_bytes.is_empty() {
            run_git_bytes_with_stdin(
                worktree.root(),
                &["update-index", "--skip-worktree", "-z", "--stdin"],
                &excluded_bytes,
            )?;
        }
        self.verify_fresh_sparse_recovery_identity(dest, branch, base)?;
        self.verify_sparse_recovery_index_matches_base(&worktree, &expected_base)?;
        let missing_included = missing_worktree_path_list(worktree.root(), &included_paths)?;
        if !missing_included.is_empty() {
            run_git_bytes_with_stdin(
                worktree.root(),
                &["checkout-index", "-z", "--stdin"],
                &missing_included,
            )?;
        }
        self.verify_fresh_sparse_recovery_worktree(dest, branch, base)
    }

    fn verify_full_sparse_recovery_worktree(
        &self,
        dest: &Path,
        branch: &str,
        base: &str,
    ) -> Result<GitRepo, GitError> {
        let worktree = self.verify_fresh_sparse_recovery_worktree(dest, branch, base)?;
        let entries = run_git_bytes(worktree.root(), &["ls-files", "-t", "-z"])?;
        if entries
            .split(|byte| *byte == 0)
            .any(|entry| entry.starts_with(b"S "))
        {
            return Err(GitError::UntrustedOutput {
                detail: format!(
                    "worktree at {} still has sparse skip-worktree entries after full recovery",
                    dest.display()
                ),
            });
        }
        Ok(worktree)
    }

    fn sparse_recovery_retained_error(
        dest: &Path,
        branch: &str,
        base: &str,
        reason: impl std::fmt::Display,
    ) -> GitError {
        GitError::Git {
            args: "sparse-checkout recovery".into(),
            stderr: format!(
                "{reason}; partial worktree retained at {} on branch {branch} (expected base {base}); inspect it before cleanup",
                dest.display()
            ),
        }
    }

    fn restore_full_checkout_after_sparse_failure(
        &self,
        dest: &Path,
        branch: &str,
        base: &str,
    ) -> Result<GitRepo, GitError> {
        // The fresh path is not yet handed to a caller, but hooks or another
        // process can still write there. Prove its identity and contents before
        // disabling sparsity, and retain it if the proof or recovery fails.
        // Never use reset --hard or force removal on this recovery path.
        let worktree = self
            .verify_sparse_recovery_worktree_state(dest, branch, base)
            .map_err(|error| {
                Self::sparse_recovery_retained_error(
                    dest,
                    branch,
                    base,
                    format!("refusing recovery because checkout identity or cleanliness is unproven: {error}"),
                )
            })?;
        let stage_entries = run_git_bytes(worktree.root(), &["ls-files", "--stage", "-z"])
            .map_err(|error| {
                Self::sparse_recovery_retained_error(
                    dest,
                    branch,
                    base,
                    format!("could not inspect the recovery index: {error}"),
                )
            })?;
        if stage_entries.is_empty() {
            // This is the broker-created --no-checkout state. Fill the index
            // without touching files, then copy only paths that are still
            // absent. Any content that arrived since the empty-index proof is
            // left in place and causes verification to retain the worktree.
            let expected_base = self.resolve_commit_checked(base).map_err(|error| {
                Self::sparse_recovery_retained_error(
                    dest,
                    branch,
                    base,
                    format!("could not resolve the expected recovery base: {error}"),
                )
            })?;
            run_git(worktree.root(), &["read-tree", &expected_base]).map_err(|error| {
                Self::sparse_recovery_retained_error(
                    dest,
                    branch,
                    base,
                    format!("could not populate the recovery index: {error}"),
                )
            })?;
            self.verify_fresh_sparse_recovery_identity(dest, branch, base)
                .and_then(|worktree| {
                    self.verify_sparse_recovery_index_matches_base(&worktree, &expected_base)
                })
                .map_err(|error| {
                    Self::sparse_recovery_retained_error(
                        dest,
                        branch,
                        base,
                        format!("recovery identity changed while preparing the index: {error}"),
                    )
                })?;
            let paths = run_git_bytes(worktree.root(), &["ls-files", "-z"]).map_err(|error| {
                Self::sparse_recovery_retained_error(
                    dest,
                    branch,
                    base,
                    format!("could not list full-checkout paths: {error}"),
                )
            })?;
            let paths = nul_terminated_paths(&paths, "ls-files").map_err(|error| {
                Self::sparse_recovery_retained_error(
                    dest,
                    branch,
                    base,
                    format!("could not parse full-checkout paths: {error}"),
                )
            })?;
            let missing = missing_worktree_path_list(worktree.root(), &paths).map_err(|error| {
                Self::sparse_recovery_retained_error(
                    dest,
                    branch,
                    base,
                    format!("could not prove which full-checkout paths are absent: {error}"),
                )
            })?;
            if !missing.is_empty() {
                run_git_bytes_with_stdin(
                    worktree.root(),
                    &["checkout-index", "-z", "--stdin"],
                    &missing,
                )
                .map_err(|error| {
                    Self::sparse_recovery_retained_error(
                        dest,
                        branch,
                        base,
                        format!("could not safely materialize the full checkout: {error}"),
                    )
                })?;
            }
        } else {
            let tagged_entries = run_git_bytes(worktree.root(), &["ls-files", "-t", "-z"])
                .map_err(|error| {
                    Self::sparse_recovery_retained_error(
                        dest,
                        branch,
                        base,
                        format!("could not inspect sparse index entries: {error}"),
                    )
                })?;
            let skipped_paths = tagged_index_paths(&tagged_entries, b'S').map_err(|error| {
                Self::sparse_recovery_retained_error(
                    dest,
                    branch,
                    base,
                    format!("could not parse sparse index entries: {error}"),
                )
            })?;
            let missing_skipped = missing_worktree_path_list(worktree.root(), &skipped_paths)
                .map_err(|error| {
                    Self::sparse_recovery_retained_error(
                        dest,
                        branch,
                        base,
                        format!("could not prove which sparse paths are absent: {error}"),
                    )
                })?;
            if !skipped_paths.is_empty() {
                run_git_bytes_with_stdin(
                    worktree.root(),
                    &["update-index", "--no-skip-worktree", "-z", "--stdin"],
                    &nul_path_list(&skipped_paths),
                )
                .map_err(|error| {
                    Self::sparse_recovery_retained_error(
                        dest,
                        branch,
                        base,
                        format!("could not clear sparse index entries: {error}"),
                    )
                })?;
            }
            if !missing_skipped.is_empty() {
                run_git_bytes_with_stdin(
                    worktree.root(),
                    &["checkout-index", "-z", "--stdin"],
                    &missing_skipped,
                )
                .map_err(|error| {
                    Self::sparse_recovery_retained_error(
                        dest,
                        branch,
                        base,
                        format!("could not safely materialize omitted paths: {error}"),
                    )
                })?;
            }
        }
        self.disable_sparse_checkout_config_after_materialization(&worktree)
            .map_err(|error| {
                Self::sparse_recovery_retained_error(
                    dest,
                    branch,
                    base,
                    format!("could not disable sparse-checkout configuration: {error}"),
                )
            })?;
        self.verify_full_sparse_recovery_worktree(dest, branch, base)
            .map_err(|error| {
                Self::sparse_recovery_retained_error(
                    dest,
                    branch,
                    base,
                    format!("full-checkout verification failed: {error}"),
                )
            })
    }

    fn verify_sparse_recovery_worktree_state(
        &self,
        dest: &Path,
        branch: &str,
        base: &str,
    ) -> Result<GitRepo, GitError> {
        let worktree = self.verify_fresh_sparse_recovery_identity(dest, branch, base)?;
        let index_entries = run_git_bytes(worktree.root(), &["ls-files", "--stage", "-z"])?;
        if index_entries.is_empty() {
            Self::verify_sparse_worktree_has_no_untracked_or_ignored(&worktree, dest)?;
            return Ok(worktree);
        }

        self.verify_sparse_recovery_index_matches_base(&worktree, base)?;
        self.verify_fresh_sparse_recovery_worktree(dest, branch, base)
    }

    fn disable_sparse_checkout_config_after_materialization(
        &self,
        worktree: &GitRepo,
    ) -> Result<(), GitError> {
        let sparse_checkout = git_config_bool_or_unset(worktree.root(), "core.sparseCheckout")?;
        if sparse_checkout != Some(true) {
            return Ok(());
        }
        if git_config_bool_or_unset(worktree.root(), "extensions.worktreeConfig")? != Some(true) {
            return Err(GitError::UntrustedOutput {
                detail: "refusing to change shared sparse-checkout config without worktree-specific config".into(),
            });
        }
        run_git(
            worktree.root(),
            &["config", "--worktree", "core.sparseCheckout", "false"],
        )?;
        Ok(())
    }

    /// Remove a linked worktree. `force` discards uncommitted changes —
    /// callers must have applied the dirty-check policy first.
    ///
    /// `git worktree remove` deregisters the worktree and *then* deletes the
    /// directory. When the delete aborts partway — a gitignored file appearing
    /// mid-walk is enough (#165) — the administrative half is already torn down
    /// while the files remain, and every later attempt answers
    /// `is not a working tree`. The directory becomes unrecoverable through
    /// this lane, which is how sessions ended up needing a manual `rm -rf`.
    ///
    /// So that one state is finished here instead of being handed back: delete
    /// the tree with retries, then `prune` to reconcile the registration.
    ///
    /// Recovery is gated on the worktree no longer being a git worktree at all,
    /// which is deliberately narrow. A dirty checkout that `git` refused to
    /// remove is still perfectly discoverable, so it takes the untouched error
    /// path and `force`'s meaning is preserved. The gate does mean an orphan is
    /// deleted without a dirty check -- but no dirty check exists for it: the
    /// gitdir that would answer the question is what went missing, and the
    /// alternative is a directory nothing can ever remove.
    /// Lock a worktree so `git worktree prune` -- including the automatic one
    /// `git gc` runs -- keeps its registration while its directory is
    /// unreachable, as it is when it lives on a drive that is unplugged.
    pub fn worktree_lock(&self, worktree: &Path) -> Result<(), GitError> {
        let path = worktree.to_str().unwrap_or_default();
        run_git(
            &self.root,
            &[
                "worktree",
                "lock",
                "--reason",
                BROKER_WORKTREE_LOCK_REASON,
                path,
            ],
        )
        .map(|_| ())
    }

    /// The lock reason of the worktree registered at `worktree`, if locked.
    /// Reads that one registration's `locked` file (what `git worktree list`
    /// reports) when the checkout's `.git` names a registration of this
    /// repository; only otherwise lists every worktree, which on a large
    /// repository costs seconds per removal.
    fn worktree_lock_reason(&self, worktree: &Path) -> Option<String> {
        match self.registration_lock_reason(worktree) {
            Ok(reason) => reason,
            Err(()) => self.listed_worktree_lock_reason(worktree),
        }
    }

    /// `Ok(lock reason)` from the registration the checkout's `.git` file
    /// names, when that registration is this repository's and names the
    /// checkout back; `Err(())` when that cannot be established.
    pub(crate) fn registration_lock_reason(&self, worktree: &Path) -> Result<Option<String>, ()> {
        let worktree = worktree.canonicalize().map_err(|_| ())?;
        let pointer = worktree.join(".git");
        let text = fs::read_to_string(&pointer).map_err(|_| ())?;
        let admin = text
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))
            .map(|value| PathBuf::from(value.trim()))
            .ok_or(())?;
        let admin = if admin.is_absolute() {
            admin
        } else {
            worktree.join(admin)
        };
        let admin = admin.canonicalize().map_err(|_| ())?;
        let registrations = self
            .git_common_dir()
            .map_err(|_| ())?
            .join("worktrees")
            .canonicalize()
            .map_err(|_| ())?;
        if admin.parent() != Some(registrations.as_path()) {
            return Err(());
        }
        let named = fs::read_to_string(admin.join("gitdir")).map_err(|_| ())?;
        if Path::new(named.trim()).canonicalize().map_err(|_| ())? != pointer {
            return Err(());
        }
        match fs::read_to_string(admin.join("locked")) {
            Ok(reason) => Ok(Some(reason.trim_end_matches(['\n', '\r']).to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        }
    }

    /// The lock reason of the registration naming `worktree`, found from the
    /// repository side (each `<common>/worktrees/<name>/gitdir`) when the
    /// checkout's own `.git` cannot name it, and read fresh from that
    /// registration's `locked` file: a lock placed a moment ago counts. No
    /// listing is involved, cached or not.
    fn listed_worktree_lock_reason(&self, worktree: &Path) -> Option<String> {
        let wanted = worktree
            .canonicalize()
            .unwrap_or_else(|_| worktree.to_path_buf())
            .join(".git");
        let registrations = self.git_common_dir().ok()?.join("worktrees");
        for entry in fs::read_dir(&registrations).ok()?.filter_map(Result::ok) {
            let admin = entry.path();
            let Ok(named) = fs::read_to_string(admin.join("gitdir")) else {
                continue;
            };
            let named = PathBuf::from(named.trim());
            let same = named == wanted
                || named
                    .canonicalize()
                    .is_ok_and(|resolved| resolved == wanted);
            if !same {
                continue;
            }
            return match fs::read_to_string(admin.join("locked")) {
                Ok(reason) => Some(reason.trim_end_matches(['\n', '\r']).to_string()),
                Err(_) => None,
            };
        }
        None
    }

    pub fn worktree_remove(&self, worktree: &Path, force: bool) -> Result<(), GitError> {
        let _span = crate::phase_telemetry::span("worktree_remove");
        // Checked here as well as in `worktree_remove_registered`: a refusal
        // there must not fall through to the orphan removal below.
        ensure_fresh_git_verdict()?;
        self.refuse_if_start_in_progress(worktree)?;
        let Err(error) = self.worktree_remove_registered(worktree, force) else {
            return Ok(());
        };
        if !crate::broker::is_orphaned_worktree_directory(worktree) {
            // Either nothing was stranded, or the checkout is still a working
            // tree and the refusal was a real one -- a dirty worktree without
            // `force`, most often. Reporting it unchanged keeps that visible.
            return Err(error);
        }
        let outcome = crate::removal::remove_tree(worktree);
        if !outcome.removed {
            return Err(error);
        }
        // The registration may or may not still exist depending on how far the
        // original command got; prune reconciles both cases and is a no-op when
        // there is nothing stale.
        let _ = run_git(&self.root, &["worktree", "prune"]);
        Ok(())
    }

    /// Remove a worktree without recursively deleting its directory if Git refuses.
    /// Rollback callers use this after proving the worktree identity and cleanliness;
    /// any new or ambiguous contents must remain available for recovery.
    pub(crate) fn worktree_remove_without_orphan_fallback(
        &self,
        worktree: &Path,
    ) -> Result<(), GitError> {
        self.worktree_remove_registered(worktree, false)
    }

    /// Remove only through Git, preserving every refusal and registration.
    /// Force requests Git's force behavior; it never authorizes a recursive
    /// filesystem deletion when Git cannot establish a successful removal.
    pub(crate) fn worktree_remove_registered(
        &self,
        worktree: &Path,
        force: bool,
    ) -> Result<(), GitError> {
        // Whatever judged this checkout removable ran on this process's Git;
        // a deletion never rests on a cached verdict about that Git.
        ensure_fresh_git_verdict()?;
        self.refuse_if_start_in_progress(worktree)?;
        // Environments are destroyed before their worktree goes: their
        // destroy hooks usually live in it (#1211). A worktree whose
        // environments could not be destroyed is kept, with the reason.
        let environments = crate::phase_telemetry::span("worktree_remove_environments");
        if let Err(reason) = crate::environments::lifecycle::destroy_for_worktree(worktree) {
            return Err(GitError::Git {
                args: format!("worktree remove {}", worktree.display()),
                stderr: reason,
            });
        }
        drop(environments);
        // A locked worktree refuses removal. Release only the lock the broker
        // itself placed on a removable root; a lock someone else set is their
        // protection, and the removal must keep failing on it.
        if self.worktree_lock_reason(worktree).as_deref() == Some(BROKER_WORKTREE_LOCK_REASON) {
            // The removal below cannot succeed on a locked worktree, so a
            // failed unlock is the error to report, not a detail to drop.
            run_git(
                &self.root,
                &["worktree", "unlock", worktree.to_str().unwrap_or_default()],
            )?;
        }
        let path = worktree.to_str().unwrap_or_default();
        let args: Vec<&str> = if force {
            vec!["worktree", "remove", "--force", path]
        } else {
            vec!["worktree", "remove", path]
        };
        let _span = crate::phase_telemetry::span("git_worktree_remove");
        run_git(&self.root, &args)?;
        Ok(())
    }

    /// Every removal of a checkout passes here: a path a running `broker
    /// start` has reserved (`crate::start_reservation`) is refused with the
    /// recorded reason `start_in_progress`, whichever lane asked.
    fn refuse_if_start_in_progress(&self, worktree: &Path) -> Result<(), GitError> {
        let Ok(main_root) = self.main_root() else {
            return Ok(());
        };
        crate::start_reservation::refuse_if_start_in_progress(&main_root, worktree).map_err(
            |reason| GitError::Git {
                args: format!("worktree remove {}", worktree.display()),
                stderr: reason,
            },
        )
    }

    /// Every local branch as `(name, tip)`, sorted by name (`refs/heads/`
    /// stripped).
    pub(crate) fn for_each_local_branch(&self) -> Result<Vec<(String, String)>, GitError> {
        let listing = run_git(
            &self.root,
            &[
                "for-each-ref",
                "--sort=refname",
                "--format=%(objectname) %(refname)",
                "refs/heads/",
            ],
        )?;
        Ok(listing
            .lines()
            .filter_map(|line| line.split_once(' '))
            .filter_map(|(tip, reference)| {
                Some((
                    reference.strip_prefix("refs/heads/")?.to_string(),
                    tip.to_string(),
                ))
            })
            .collect())
    }

    /// Delete an exact local branch only when it still names the expected
    /// object. Used to roll back a just-created session worktree if its
    /// atomic database registration loses a planned-lease race.
    pub fn delete_branch_ref_checked(&self, branch: &str, expected: &str) -> Result<(), GitError> {
        ensure_fresh_git_verdict()?;
        run_git(
            &self.root,
            &[
                "update-ref",
                "-d",
                &format!("refs/heads/{branch}"),
                expected,
            ],
        )?;
        Ok(())
    }

    /// Paths of Git-registered linked worktrees, including registrations Git
    /// marks prunable, but excluding the main checkout.
    pub fn worktree_paths(&self) -> Result<Vec<PathBuf>, GitError> {
        let inventory = self.worktree_inventory()?;
        let main = self.main_root()?;
        Ok(inventory
            .into_iter()
            .map(|entry| entry.path)
            .filter(|path| path.canonicalize().map(|p| p != main).unwrap_or(true))
            .collect())
    }

    /// All worktree registrations Git knows about, including the main checkout
    /// and entries Git marks prunable. A failed or malformed listing is an
    /// error so cleanup callers cannot mistake unknown state for no worktrees.
    pub fn worktree_inventory(&self) -> Result<Vec<GitWorktreeInfo>, GitError> {
        let started = Instant::now();
        let listing = run_git_bytes(&self.root, &["worktree", "list", "--porcelain", "-z"])?;
        let entries = parse_worktree_porcelain_z(&listing)?;
        crate::phase_telemetry::add("git_worktree_list", started.elapsed());
        Ok(entries)
    }

    /// [`Self::worktree_inventory`], listed once per run while a
    /// [`WorktreeInventoryRunScope`] is active on this thread. Only for
    /// lookups whose caller verifies what it acts on from the filesystem
    /// (a checkout's own `.git` and its admin directory), or that look up a
    /// checkout whose directory exists (Git never registers a second
    /// worktree at an existing path); a check that a branch is checked out
    /// nowhere, or that a missing checkout is unregistered, lists afresh.
    pub(crate) fn worktree_inventory_for_run(&self) -> Result<Vec<GitWorktreeInfo>, GitError> {
        let cached = WORKTREE_INVENTORY_RUN_CACHE.with(|cache| {
            cache
                .borrow()
                .as_ref()
                .map(|cache| cache.get(&self.root).cloned())
        });
        if let Some(Some(entries)) = cached {
            crate::phase_telemetry::add("git_worktree_list_reused", Duration::ZERO);
            return Ok(entries);
        }
        let entries = self.worktree_inventory()?;
        WORKTREE_INVENTORY_RUN_CACHE.with(|cache| {
            if let Some(cache) = cache.borrow_mut().as_mut() {
                cache.insert(self.root.clone(), entries.clone());
            }
        });
        Ok(entries)
    }

    /// Return a complete, bounded listing of Git-ignored paths in this
    /// discovered working-tree checkout. Any timeout, malformed path, output
    /// cap, or failed directory verification is an error rather than a partial
    /// listing.
    ///
    /// This method only identifies collapsed directory roots. A caller that
    /// later reclaims artifacts must perform its own fresh ownership, liveness,
    /// and resource checks under the corresponding admission protocol.
    pub(crate) fn ignored_paths_readonly_until(
        &self,
        deadline: Instant,
    ) -> Result<Vec<GitIgnoredPathEntry>, GitError> {
        const ARGS: &[&str] = &[
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ];

        let deadline = active_git_deadline().map_or(deadline, |active| active.min(deadline));
        ensure_bounded_git_listing_before(deadline, ARGS)?;
        if std::env::vars_os().any(|(key, _)| unsafe_graph_git_environment_key(&key)) {
            return Err(GitError::UntrustedOutput {
                detail: "inherited Git routing overrides make ignored paths unknown".into(),
            });
        }
        require_trustworthy_porcelain()?;
        ensure_bounded_git_listing_before(deadline, ARGS)?;
        let mut command = ignored_paths_git_command(&self.root, ARGS);
        let output = run_git_process_output_limited(
            &mut command,
            ARGS,
            deadline,
            MAX_IGNORED_PATHS_OUTPUT_BYTES,
        )?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: ARGS.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        let raw_paths =
            parse_ignored_paths_with_cap(&output.stdout, ARGS, MAX_IGNORED_PATHS_ENTRIES)?;
        ensure_bounded_git_listing_before(deadline, ARGS)?;

        let mut entries = Vec::with_capacity(raw_paths.len());
        for mut raw in raw_paths {
            ensure_bounded_git_listing_before(deadline, ARGS)?;

            let directory_marker = raw.ends_with('/');
            if directory_marker {
                raw.pop();
            }
            validate_relative_git_path(&raw, ARGS)?;
            if raw.len() > MAX_IGNORED_PATH_BYTES {
                return Err(GitError::UntrustedOutput {
                    detail: format!(
                        "git {} returned an ignored path longer than {MAX_IGNORED_PATH_BYTES} bytes",
                        ARGS.join(" ")
                    ),
                });
            }

            let kind = if is_broker_protected_ignored_path(&raw) {
                GitIgnoredPathKind::Protected
            } else if directory_marker
                && ignored_directory_is_real(&self.root, &raw, ARGS, deadline)?
            {
                GitIgnoredPathKind::DirectoryRoot
            } else {
                GitIgnoredPathKind::Other
            };
            entries.push(GitIgnoredPathEntry {
                relative: raw,
                directory_marker,
                kind,
            });
        }

        ensure_bounded_git_listing_before(deadline, ARGS)?;
        Ok(entries)
    }
}

/// Preserve the checkout's normal exclude configuration while preventing
/// inherited Git directory/index selectors from redirecting this read.
fn ignored_paths_git_command(root: &Path, args: &[&str]) -> Command {
    let mut command = git_command();
    for (key, _) in std::env::vars_os() {
        if unsafe_graph_git_environment_key(&key) {
            command.env_remove(key);
        }
    }
    command
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0");
    command
}

/// A read-only cleanliness listing run in `root` with the resolved Git
/// program, the inherited variables that redirect repository discovery
/// removed and optional locks off: the command the ignored-path listing
/// uses, for the cleanup proof's own listings.
pub(crate) fn readonly_listing_command(root: &Path, args: &[&str]) -> Command {
    ignored_paths_git_command(root, args)
}

/// Whether ignored paths may be read from a Git listing at all: no
/// inherited routing override and a Git whose porcelain is not decorated.
pub(crate) fn ignored_listing_preflight() -> Result<(), String> {
    if std::env::vars_os().any(|(key, _)| unsafe_graph_git_environment_key(&key)) {
        return Err("inherited Git routing overrides make ignored paths unknown".into());
    }
    require_trustworthy_porcelain().map_err(|error| error.to_string())
}

fn ensure_bounded_git_listing_before(deadline: Instant, args: &[&str]) -> Result<(), GitError> {
    if Instant::now() >= deadline {
        note_git_timeout();
        Err(GitError::timed_out_before_start(
            args.join(" "),
            None,
            Duration::ZERO,
        ))
    } else {
        Ok(())
    }
}

fn parse_ignored_paths_with_cap(
    output: &[u8],
    args: &[&str],
    max_entries: usize,
) -> Result<Vec<String>, GitError> {
    let terminators = output
        .iter()
        .filter(|byte| **byte == 0)
        .take(max_entries.saturating_add(1))
        .count();
    if terminators > max_entries {
        return Err(GitError::UntrustedOutput {
            detail: format!(
                "git {} returned more than {max_entries} ignored paths",
                args.join(" ")
            ),
        });
    }
    parse_nul_paths_strict(output, args)
}

/// Run a prepared Git command with an absolute deadline and per-stream output
/// cap. Nonzero status and stderr are returned to the caller; deadline or cap
/// failures never expose partial stdout.
pub(crate) fn run_git_process_output_limited(
    command: &mut Command,
    args: &[&str],
    deadline: Instant,
    max_output_bytes: usize,
) -> Result<std::process::Output, GitError> {
    let effective_deadline = active_git_deadline().map_or(deadline, |active| active.min(deadline));
    let remaining = effective_deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        note_git_timeout();
        return Err(GitError::timed_out_before_start(
            args.join(" "),
            command.get_current_dir(),
            remaining,
        ));
    }
    let budget = remaining.min(git_timeout());
    let started = Instant::now();
    match crate::bounded_output::output_limited(command, budget, max_output_bytes).map_err(
        |source| GitError::Spawn {
            args: args.join(" "),
            source,
        },
    )? {
        crate::bounded_output::LimitedOutput::Completed(output) => {
            if Instant::now() >= effective_deadline {
                note_git_timeout();
                Err(GitError::timed_out_running(
                    args.join(" "),
                    command.get_current_dir(),
                    started,
                    deadline_is_outer(effective_deadline),
                    budget,
                ))
            } else {
                Ok(output)
            }
        }
        crate::bounded_output::LimitedOutput::Deadline => {
            note_git_timeout();
            Err(GitError::timed_out_running(
                args.join(" "),
                command.get_current_dir(),
                started,
                deadline_is_outer(effective_deadline),
                budget,
            ))
        }
        crate::bounded_output::LimitedOutput::OutputLimit => Err(GitError::UntrustedOutput {
            detail: format!(
                "git {} exceeded the {max_output_bytes}-byte per-stream output limit",
                args.join(" "),
            ),
        }),
    }
}

fn validate_relative_git_path(path: &str, args: &[&str]) -> Result<(), GitError> {
    let invalid = || GitError::UntrustedOutput {
        detail: format!(
            "git {} returned an invalid relative ignored path",
            args.join(" ")
        ),
    };
    if path.is_empty()
        || path.starts_with('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || Path::new(path)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(invalid());
    }
    Ok(())
}

fn is_broker_protected_ignored_path(relative: &str) -> bool {
    if relative
        .split('/')
        .any(|component| component == ".git" || component == ".aethyme")
    {
        return true;
    }

    crate::runtime_paths::broker_runtime_ignore_patterns().any(|pattern| {
        let pattern = pattern.trim_end_matches('*').trim_end_matches('/');
        relative == pattern
            || relative
                .strip_prefix(pattern)
                .is_some_and(|suffix| suffix.starts_with('/'))
            || pattern
                .strip_prefix(relative)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

/// Check only a Git-collapsed directory root. `symlink_metadata` is used for
/// every component so an ignored symlink or a symlinked parent is never
/// promoted to a directory candidate.
fn ignored_directory_is_real(
    root: &Path,
    relative: &str,
    args: &[&str],
    deadline: Instant,
) -> Result<bool, GitError> {
    let components = relative.split('/').collect::<Vec<_>>();
    let mut path = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        ensure_bounded_git_listing_before(deadline, args)?;
        path.push(component);
        let metadata = fs::symlink_metadata(&path).map_err(|source| GitError::UntrustedOutput {
            detail: format!(
                "could not verify an ignored directory from git {}: {source}",
                args.join(" ")
            ),
        })?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            return Ok(false);
        }
        if index + 1 < components.len() && !file_type.is_dir() {
            return Err(GitError::UntrustedOutput {
                detail: format!("ignored directory changed during git {}", args.join(" ")),
            });
        }
        if index + 1 == components.len() {
            return Ok(file_type.is_dir());
        }
    }
    Ok(false)
}

#[cfg(test)]
mod ignored_path_listing_tests {
    use super::{
        GitError, GitIgnoredPathKind, GitRepo, is_broker_protected_ignored_path,
        parse_ignored_paths_with_cap, run_git_process_output_limited,
    };
    use std::process::Command;
    use std::time::Instant;

    fn git(root: &std::path::Path, args: &[&str]) {
        super::run_isolated_graph_git(root, args)
            .unwrap_or_else(|error| panic!("git {}: {error}", args.join(" ")));
    }

    fn repository() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "--template=", "-b", "main"]);
        git(root.path(), &["config", "user.name", "Ignored path test"]);
        git(
            root.path(),
            &["config", "user.email", "ignored-path@example.invalid"],
        );
        git(root.path(), &["commit", "--allow-empty", "-qm", "initial"]);
        root
    }

    fn touch(root: &std::path::Path, relative: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"ignored test data").unwrap();
    }

    #[test]
    fn complete_listing_types_collapsed_directories_and_preserves_files() {
        let root = repository();
        std::fs::write(root.path().join(".gitignore"), "*\n").unwrap();
        git(root.path(), &["add", "-f", ".gitignore"]);
        git(root.path(), &["commit", "-qm", "ignore outputs"]);
        touch(root.path(), "opaque/nested/file.txt");
        touch(root.path(), "standalone");
        touch(root.path(), "newline\nname");
        touch(root.path(), ".aethyme/run/worker.json");

        let repo = GitRepo::discover(root.path()).unwrap();
        let entries = repo
            .ignored_paths_readonly_until(Instant::now() + std::time::Duration::from_secs(5))
            .unwrap();
        let find = |path: &str| entries.iter().find(|entry| entry.relative == path).unwrap();

        assert_eq!(find("opaque").kind, GitIgnoredPathKind::DirectoryRoot);
        assert!(find("opaque").directory_marker);
        assert_eq!(find("standalone").kind, GitIgnoredPathKind::Other);
        assert!(!find("standalone").directory_marker);
        assert_eq!(find("newline\nname").kind, GitIgnoredPathKind::Other);
        assert_eq!(find(".aethyme").kind, GitIgnoredPathKind::Protected);
        assert!(find(".aethyme").directory_marker);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_git_filename_makes_the_complete_listing_unknown() {
        let root = repository();
        std::fs::write(root.path().join(".gitignore"), "bad-*\n").unwrap();
        git(root.path(), &["add", ".gitignore"]);
        git(root.path(), &["commit", "-qm", "ignore malformed filename"]);
        let Some(_path) =
            aethyme_testkit::filesystem::write_non_utf8_file(root.path(), b"bad-\xff", b"ignored")
                .unwrap()
        else {
            return;
        };

        let repo = GitRepo::discover(root.path()).unwrap();
        assert!(matches!(
            repo.ignored_paths_readonly_until(Instant::now() + std::time::Duration::from_secs(5)),
            Err(GitError::UntrustedOutput { .. })
        ));
    }

    #[test]
    fn directory_with_a_tracked_child_is_not_a_collapsed_candidate() {
        let root = repository();
        std::fs::write(root.path().join(".gitignore"), "mixed/*\n").unwrap();
        git(root.path(), &["add", ".gitignore"]);
        git(root.path(), &["commit", "-qm", "ignore mixed output"]);
        std::fs::create_dir_all(root.path().join("mixed")).unwrap();
        touch(root.path(), "mixed/tracked.txt");
        touch(root.path(), "mixed/ignored.txt");
        git(root.path(), &["add", "-f", "mixed/tracked.txt"]);
        git(root.path(), &["commit", "-qm", "tracked child"]);

        let repo = GitRepo::discover(root.path()).unwrap();
        let entries = repo
            .ignored_paths_readonly_until(Instant::now() + std::time::Duration::from_secs(5))
            .unwrap();
        assert!(!entries.iter().any(|entry| {
            entry.relative == "mixed" && entry.kind == GitIgnoredPathKind::DirectoryRoot
        }));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_listing_is_never_a_directory_candidate() {
        let root = repository();
        std::fs::write(root.path().join(".gitignore"), "linked\n").unwrap();
        std::os::unix::fs::symlink(
            root.path().join("missing-target"),
            root.path().join("linked"),
        )
        .unwrap();
        let repo = GitRepo::discover(root.path()).unwrap();
        let entries = repo
            .ignored_paths_readonly_until(Instant::now() + std::time::Duration::from_secs(5))
            .unwrap();
        let linked = entries
            .iter()
            .find(|entry| entry.relative == "linked")
            .unwrap();
        assert_eq!(linked.kind, GitIgnoredPathKind::Other);
        assert!(!linked.directory_marker);
    }

    #[test]
    fn broker_roots_and_their_runtime_ancestors_are_protected() {
        for path in [
            ".git",
            ".git/worktrees/session",
            ".aethyme",
            ".aethyme/run/worker.json",
            ".aethyme/locks/cleanup.lock",
        ] {
            assert!(is_broker_protected_ignored_path(path), "{path}");
        }
        assert!(!is_broker_protected_ignored_path("cache/output"));
    }

    #[test]
    fn ignored_path_count_cap_is_checked_before_decoding_paths() {
        let mut output = Vec::new();
        for _ in 0..=super::MAX_IGNORED_PATHS_ENTRIES {
            output.extend_from_slice(b"x\0");
        }
        output.extend_from_slice(b"invalid-\xff\0");

        let error = parse_ignored_paths_with_cap(
            &output,
            &["ls-files", "--ignored", "-z"],
            super::MAX_IGNORED_PATHS_ENTRIES,
        )
        .unwrap_err();
        assert!(
            matches!(error, GitError::UntrustedOutput { ref detail } if detail.contains("more than")),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn expired_listing_deadline_fails_before_spawning_and_output_cap_is_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("should-not-run");
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "touch \"$1\"", "test"]).arg(&marker);
        assert!(matches!(
            run_git_process_output_limited(&mut command, &["test deadline"], Instant::now(), 8,),
            Err(GitError::TimedOut { .. })
        ));
        assert!(!marker.exists());

        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 1234567890"]);
        assert!(matches!(
            run_git_process_output_limited(
                &mut command,
                &["test output cap"],
                Instant::now() + std::time::Duration::from_secs(2),
                4,
            ),
            Err(GitError::UntrustedOutput { .. })
        ));
    }
}

fn parse_worktree_porcelain_z(output: &[u8]) -> Result<Vec<GitWorktreeInfo>, GitError> {
    let mut entries = Vec::new();
    let mut record = Vec::new();
    for field in output.split(|byte| *byte == 0) {
        if field.is_empty() {
            if !record.is_empty() {
                entries.push(parse_worktree_record(&record)?);
                record.clear();
            }
        } else {
            record.push(field);
        }
    }
    if !record.is_empty() {
        entries.push(parse_worktree_record(&record)?);
    }
    if entries.is_empty() {
        return Err(GitError::WorktreeListParse {
            reason: "Git returned no worktree records".into(),
        });
    }
    Ok(entries)
}

fn parse_worktree_record(fields: &[&[u8]]) -> Result<GitWorktreeInfo, GitError> {
    let mut path = None;
    let mut head = None;
    let mut branch = None;
    let mut detached = false;
    let mut bare = false;
    let mut locked = false;
    let mut lock_reason = None;
    let mut prunable = false;
    let mut prunable_reason = None;

    for field in fields {
        if let Some(value) = field.strip_prefix(b"worktree ") {
            if path.is_some() || value.is_empty() {
                return Err(GitError::WorktreeListParse {
                    reason: "record has a duplicate or empty worktree path".into(),
                });
            }
            path = Some(worktree_path_from_bytes(value));
        } else if let Some(value) = field.strip_prefix(b"HEAD ") {
            head = Some(String::from_utf8_lossy(value).into_owned());
        } else if let Some(value) = field.strip_prefix(b"branch ") {
            branch = Some(String::from_utf8_lossy(value).into_owned());
        } else if *field == b"detached" {
            detached = true;
        } else if *field == b"bare" {
            bare = true;
        } else if *field == b"locked" {
            locked = true;
        } else if let Some(value) = field.strip_prefix(b"locked ") {
            locked = true;
            lock_reason = Some(String::from_utf8_lossy(value).into_owned());
        } else if *field == b"prunable" {
            prunable = true;
        } else if let Some(value) = field.strip_prefix(b"prunable ") {
            prunable = true;
            prunable_reason = Some(String::from_utf8_lossy(value).into_owned());
        }
        // Unknown fields are reserved for forward-compatible Git extensions.
    }

    let path = path.ok_or_else(|| GitError::WorktreeListParse {
        reason: "record is missing its worktree path".into(),
    })?;
    Ok(GitWorktreeInfo {
        path,
        head,
        branch,
        detached,
        bare,
        locked,
        lock_reason,
        prunable,
        prunable_reason,
    })
}

fn worktree_path_from_bytes(path: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(path.to_vec()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(path).into_owned())
    }
}

// A missing or malformed observation is unknown, never evidence of zero.
// Do not include provider stdout in the diagnostic: it may contain unrelated
// wrapper output, control characters or sensitive host information.
fn parse_commit_count<T: std::str::FromStr>(text: &str) -> Result<T, GitError> {
    let text = text.trim();
    let invalid = || GitError::UntrustedOutput {
        detail: "git rev-list returned an invalid commit count".to_string(),
    };
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    text.parse().map_err(|_| invalid())
}

fn parse_symmetric_commit_counts(text: &str) -> Result<(u64, u64), GitError> {
    let invalid = || GitError::UntrustedOutput {
        detail: "git rev-list returned invalid symmetric commit counts".to_string(),
    };
    let (left, right) = text.trim().split_once('\t').ok_or_else(invalid)?;
    Ok((parse_commit_count(left)?, parse_commit_count(right)?))
}

#[cfg(test)]
mod commit_count_tests {
    use super::{GitError, GitRepo, parse_commit_count, parse_symmetric_commit_counts, run_git};

    #[test]
    fn unknown_count_output_never_becomes_zero() {
        for invalid in [
            "",
            " ",
            "-1",
            "+1",
            "zero",
            "0\nextra",
            "0 1",
            "18446744073709551616",
        ] {
            assert!(
                matches!(
                    parse_commit_count::<u64>(invalid),
                    Err(GitError::UntrustedOutput { .. })
                ),
                "accepted {invalid:?}"
            );
        }
        assert_eq!(parse_commit_count::<u64>("0\n").unwrap(), 0);
        assert_eq!(parse_commit_count::<u64>("12").unwrap(), 12);
        assert_eq!(
            parse_commit_count::<u64>("18446744073709551615").unwrap(),
            u64::MAX
        );
        assert!(parse_commit_count::<u8>("256").is_err());
    }

    #[test]
    fn symmetric_counts_reject_invalid_output() {
        assert_eq!(parse_symmetric_commit_counts("2\t3\n").unwrap(), (2, 3));
        for invalid in [
            "",
            "1",
            "1 2",
            "1\t2\t3",
            "-1\t2",
            "1\t2\nextra",
            "18446744073709551616\t1",
        ] {
            assert!(
                parse_symmetric_commit_counts(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn real_git_count_readers_preserve_zero_and_nonzero() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        run_git(root, &["init", "-q", "-b", "main"]).unwrap();
        run_git(
            root,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                "base",
            ],
        )
        .unwrap();
        let base = run_git(root, &["rev-parse", "HEAD"]).unwrap();
        run_git(root, &["update-ref", "refs/remotes/origin/main", &base]).unwrap();
        let repo = GitRepo::at_known_root(root);
        assert_eq!(repo.commits_not_on_any_remote().unwrap(), 0);
        assert_eq!(repo.unmerged_commit_count(&base).unwrap(), 0);
        assert_eq!(repo.commit_count_between(&base, "HEAD").unwrap(), 0);
        assert_eq!(repo.patch_unique_commit_count(&base, "HEAD").unwrap(), 0);
        std::fs::write(root.join("change.txt"), "new content\n").unwrap();
        run_git(root, &["add", "change.txt"]).unwrap();
        run_git(
            root,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "change",
            ],
        )
        .unwrap();
        assert_eq!(repo.commits_not_on_any_remote().unwrap(), 1);
        assert_eq!(repo.unmerged_commit_count(&base).unwrap(), 1);
        assert_eq!(repo.commit_count_between(&base, "HEAD").unwrap(), 1);
        assert_eq!(repo.symmetric_commit_counts(&base, "HEAD").unwrap(), (0, 1));
        assert_eq!(repo.symmetric_commit_counts("HEAD", &base).unwrap(), (1, 0));
        assert_eq!(repo.symmetric_commit_counts(&base, &base).unwrap(), (0, 0));
        assert_eq!(repo.patch_unique_commit_count(&base, "HEAD").unwrap(), 1);
        run_git(root, &["switch", "-q", "--detach", &base]).unwrap();
        run_git(
            root,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                "other",
            ],
        )
        .unwrap();
        assert_eq!(
            repo.symmetric_commit_counts("HEAD", "main").unwrap(),
            (1, 1)
        );
    }
}

#[cfg(test)]
mod worktree_porcelain_tests {
    use super::{GitError, parse_worktree_porcelain_z};
    use std::path::PathBuf;

    #[test]
    fn parses_paths_and_registration_states_from_nul_records() {
        let output = concat!(
            "worktree /repo/main\0HEAD aaa\0branch refs/heads/main\0\0",
            "worktree /repo/path with spaces\tand\nnewline\0HEAD bbb\0detached\0",
            "locked keep for review\0prunable missing gitdir\0\0",
            "worktree /repo/bare\0bare\0\0",
        );
        let entries = parse_worktree_porcelain_z(output.as_bytes()).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, PathBuf::from("/repo/main"));
        assert_eq!(entries[0].branch.as_deref(), Some("refs/heads/main"));
        assert!(!entries[0].detached);
        assert_eq!(
            entries[1].path,
            PathBuf::from("/repo/path with spaces\tand\nnewline")
        );
        assert!(entries[1].detached);
        assert!(entries[1].locked);
        assert_eq!(entries[1].lock_reason.as_deref(), Some("keep for review"));
        assert!(entries[1].prunable);
        assert_eq!(
            entries[1].prunable_reason.as_deref(),
            Some("missing gitdir")
        );
        assert!(entries[2].bare);
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_worktree_paths() {
        use std::os::unix::ffi::OsStrExt;

        let entries = parse_worktree_porcelain_z(b"worktree /repo/bad-\xff\0bare\0\0").unwrap();
        assert_eq!(entries[0].path.as_os_str().as_bytes(), b"/repo/bad-\xff");
    }

    #[test]
    fn malformed_and_empty_inventory_fail_closed() {
        assert!(matches!(
            parse_worktree_porcelain_z(b"HEAD abc\0\0"),
            Err(GitError::WorktreeListParse { .. })
        ));
        assert!(matches!(
            parse_worktree_porcelain_z(b""),
            Err(GitError::WorktreeListParse { .. })
        ));
    }
}

#[cfg(test)]
mod path_line_tests {
    use super::parse_path_lines;

    #[test]
    fn real_paths_survive_including_spaces_and_quoted_forms() {
        let out = "src/auth.py\npath with spaces.md\n\"caf\\303\\251.rs\"\nsrc/-leading-dash.rs\n";
        assert_eq!(
            parse_path_lines(out),
            vec![
                "src/auth.py",
                "path with spaces.md",
                "\"caf\\303\\251.rs\"",
                "src/-leading-dash.rs"
            ]
        );
    }

    #[test]
    fn shim_section_headers_are_not_paths() {
        // The observed decoration: a section header prepended to a dirty
        // diff listing. Parsed naively it became an implicit lease every
        // dirty session held, warning all session pairs against each other.
        let out = "--- Changes ---\nsrc/auth.py\n";
        assert_eq!(parse_path_lines(out), vec!["src/auth.py"]);
    }

    #[test]
    fn diff_rendering_artifacts_and_status_decorations_are_discarded() {
        let out =
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n---\nok \u{2713}\n\u{2713}\n\nx\n";
        assert_eq!(parse_path_lines(out), vec!["x"]);
    }
}

#[cfg(test)]
mod porcelain_tests {
    use super::{parse_porcelain_entries_z, parse_porcelain_entries_z_with_path_limit};

    #[test]
    fn nul_status_preserves_literal_whitespace_quotes_and_arrows() {
        let out = b" M leading space \0?? quote\"name\0A  tab\tand\nnewline -> name\0";
        let entries = parse_porcelain_entries_z(out).unwrap();
        assert_eq!(
            entries,
            vec![
                (*b" M", "leading space ".into()),
                (*b"??", "quote\"name".into()),
                (*b"A ", "tab\tand\nnewline -> name".into()),
            ]
        );
        assert!(parse_porcelain_entries_z(b"").unwrap().is_empty());
    }

    #[test]
    fn nul_status_rejects_malformed_decorated_and_non_utf8_output() {
        for out in [
            b"ok \xe2\x9c\x93\n".as_slice(),
            b"?? no terminator",
            b"?? \0",
            b"?? bad-\xff\0",
            b"  fake\0",
            b"?? good\0trailing\0",
        ] {
            assert!(parse_porcelain_entries_z(out).is_err(), "{out:?}");
        }
    }

    #[test]
    fn nul_renames_keep_destination_and_consume_exactly_one_source() {
        assert_eq!(
            parse_porcelain_entries_z(b"R  new -> name\0old\nname\0?? later\0").unwrap(),
            vec![(*b"R ", "new -> name".into()), (*b"??", "later".into())]
        );
        assert!(parse_porcelain_entries_z(b"R  new\0").is_err());
        assert!(parse_porcelain_entries_z(b"R  new\0\0").is_err());
    }

    #[test]
    fn bounded_nul_status_counts_both_rename_path_fields() {
        let out = b"R  new -> name\0old\nname\0?? later\0";
        assert!(parse_porcelain_entries_z_with_path_limit(out, 2).is_err());
        assert_eq!(
            parse_porcelain_entries_z_with_path_limit(out, 3).unwrap(),
            vec![(*b"R ", "new -> name".into()), (*b"??", "later".into())]
        );
    }
}

#[cfg(test)]
mod bounded_cleanup_status_tests {
    use super::{GitError, GitRepo};
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_status_preserves_unix_literal_names_and_refuses_partial_results() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(root.path().join("tracked.txt"), "base\n").unwrap();
        git(root.path(), &["add", "tracked.txt"]);
        git(root.path(), &["commit", "-qm", "initial"]);
        std::fs::write(root.path().join("line\nbreak"), "one").unwrap();
        std::fs::write(root.path().join("quote\"and->arrow"), "two").unwrap();
        let repo = GitRepo::discover(root.path()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);

        let (dirty, untracked) = repo
            .dirty_and_untracked_paths_until_with_limits(deadline, 4096, 8)
            .unwrap();
        assert_eq!(dirty.len(), 2);
        assert_eq!(untracked.len(), 2);
        assert!(untracked.contains(&"line\nbreak".to_owned()));
        assert!(untracked.contains(&"quote\"and->arrow".to_owned()));

        let byte_error = repo
            .dirty_and_untracked_paths_until_with_limits(deadline, 1, 8)
            .unwrap_err();
        assert!(matches!(byte_error, GitError::UntrustedOutput { .. }));

        let path_error = repo
            .dirty_and_untracked_paths_until_with_limits(deadline, 4096, 1)
            .unwrap_err();
        assert!(matches!(path_error, GitError::UntrustedOutput { .. }));
    }

    #[test]
    fn bounded_status_refuses_partial_results_on_all_platforms() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(root.path().join("tracked.txt"), "base\n").unwrap();
        git(root.path(), &["add", "tracked.txt"]);
        git(root.path(), &["commit", "-qm", "initial"]);
        std::fs::write(root.path().join("tracked.txt"), "changed\n").unwrap();
        std::fs::write(root.path().join("untracked.txt"), "untracked").unwrap();

        let repo = GitRepo::discover(root.path()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let byte_error = repo
            .dirty_and_untracked_paths_until_with_limits(deadline, 1, 8)
            .unwrap_err();
        assert!(matches!(byte_error, GitError::UntrustedOutput { .. }));
        let path_error = repo
            .dirty_and_untracked_paths_until_with_limits(deadline, 4096, 1)
            .unwrap_err();
        assert!(matches!(path_error, GitError::UntrustedOutput { .. }));
    }

    #[test]
    fn bounded_finish_status_keeps_matching_ignored_paths_in_its_proof() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(
            root.path().join(".gitignore"),
            "/opaque-cache/\n/private.dat\n",
        )
        .unwrap();
        std::fs::write(root.path().join("tracked.txt"), "base\n").unwrap();
        git(root.path(), &["add", "tracked.txt", ".gitignore"]);
        git(root.path(), &["commit", "-qm", "initial"]);
        std::fs::create_dir_all(root.path().join("opaque-cache")).unwrap();
        std::fs::write(root.path().join("opaque-cache/output"), "discardable").unwrap();
        std::fs::remove_file(root.path().join("tracked.txt")).unwrap();

        let repo = GitRepo::discover(root.path()).unwrap();
        let policy = crate::AutoCleanupPolicy {
            regenerable: vec!["opaque-cache".into()],
            ..Default::default()
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        assert!(
            repo.has_only_unstaged_tracked_deletions_until_with_limits(&policy, deadline, 4096, 8)
                .unwrap()
        );

        let path_error = repo
            .has_only_unstaged_tracked_deletions_until_with_limits(&policy, deadline, 4096, 1)
            .unwrap_err();
        assert!(matches!(path_error, GitError::UntrustedOutput { .. }));

        std::fs::write(root.path().join("private.dat"), "preserve").unwrap();
        assert!(
            !repo
                .has_only_unstaged_tracked_deletions_until_with_limits(&policy, deadline, 4096, 8)
                .unwrap()
        );
    }
}

#[cfg(test)]
mod gitlink_census_tests {
    use super::{GitError, GitRepo, parse_index_gitlinks, parse_tree_gitlinks};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn init_repo(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.name", "test"]);
        git(root, &["config", "user.email", "test@example.invalid"]);
    }

    #[test]
    fn checked_default_observation_matches_the_fetched_tip_and_refuses_broken_defaults() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path();
        init_repo(root);
        git(root, &["commit", "--allow-empty", "-qm", "initial"]);
        let repo = GitRepo::discover(root).unwrap();
        let first = repo.head_commit().unwrap();
        git(root, &["remote", "add", "origin", "."]);
        git(root, &["config", "branch.main.remote", "origin"]);
        git(root, &["config", "branch.main.merge", "refs/heads/main"]);
        git(root, &["update-ref", "refs/remotes/origin/main", &first]);
        assert_eq!(
            repo.upstream_default_checked().unwrap(),
            repo.upstream_default()
        );

        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        git(root, &["commit", "--allow-empty", "-qm", "updated"]);
        let second = repo.head_commit().unwrap();
        git(root, &["update-ref", "refs/remotes/origin/main", &second]);
        assert_eq!(
            repo.upstream_default_checked().unwrap(),
            Some(("origin/main".to_string(), second))
        );

        git(root, &["update-ref", "-d", "refs/remotes/origin/main"]);
        assert!(repo.upstream_default_checked().is_err());
        git(
            root,
            &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
        );
        git(root, &["update-ref", "refs/remotes/origin/HEAD", &first]);
        assert!(repo.upstream_default_checked().is_err());
    }

    fn submodule_fixture() -> (tempfile::TempDir, GitRepo, PathBuf) {
        let owner = tempfile::tempdir().unwrap();
        let source = owner.path().join("submodule-source");
        let repository = owner.path().join("superproject");
        init_repo(&source);
        std::fs::write(source.join("tracked.txt"), "initial\n").unwrap();
        git(&source, &["add", "tracked.txt"]);
        git(&source, &["commit", "-qm", "submodule initial"]);

        init_repo(&repository);
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "README.md"]);
        git(&repository, &["commit", "-qm", "superproject initial"]);
        let source_path = source.to_str().unwrap();
        git(
            &repository,
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
            &repository,
            &["config", "-f", ".gitmodules", "submodule.sub.ignore", "all"],
        );
        git(&repository, &["add", "-A"]);
        git(&repository, &["commit", "-qm", "add submodule"]);
        git(&repository, &["config", "diff.ignoreSubmodules", "all"]);
        git(&repository, &["config", "submodule.sub.ignore", "all"]);

        let checkout = GitRepo::discover(&repository).unwrap();
        (owner, checkout, repository.join("sub"))
    }

    #[test]
    fn gitlink_census_parsers_are_strict_and_preserve_newline_paths_and_conflict_stages() {
        let oid = "a".repeat(40);
        let mut tree = format!("160000 commit {oid}\tsub\nmodule\0").into_bytes();
        tree.extend_from_slice(format!("100644 blob {oid}\tregular.txt\0").as_bytes());
        assert_eq!(
            parse_tree_gitlinks(&tree, &["ls-tree"]).unwrap(),
            vec!["sub\nmodule".to_owned()]
        );

        let index = format!(
            "160000 {oid} 1\tconflicted-submodule\0\
             160000 {oid} 2\tconflicted-submodule\0\
             160000 {oid} 3\tconflicted-submodule\0\
             100644 {oid} 0\tregular.txt\0"
        );
        assert_eq!(
            parse_index_gitlinks(index.as_bytes(), &["ls-files"]).unwrap(),
            vec![
                "conflicted-submodule".to_owned(),
                "conflicted-submodule".to_owned(),
                "conflicted-submodule".to_owned()
            ]
        );
    }

    #[test]
    fn malformed_gitlink_census_output_fails_closed() {
        let oid = "a".repeat(40);
        let mut invalid_tree_path = format!("160000 commit {oid}\tbad-").into_bytes();
        invalid_tree_path.extend_from_slice(&[0xff, 0]);
        for out in [
            format!("160000 commit {oid}\tsub").into_bytes(),
            format!("160000 blob {oid}\tsub\0").into_bytes(),
            format!("100644 tree {oid}\tregular.txt\0").into_bytes(),
            format!("020000 blob {oid}\tunknown-mode\0").into_bytes(),
            invalid_tree_path,
        ] {
            assert!(parse_tree_gitlinks(&out, &["ls-tree"]).is_err(), "{out:?}");
        }
        let mut invalid_index_path = format!("160000 {oid} 1\tbad-").into_bytes();
        invalid_index_path.extend_from_slice(&[0xff, 0]);
        for out in [
            format!("160000 {oid} 4\tsub\0").into_bytes(),
            format!("040000 {oid} 0\tdirectory\0").into_bytes(),
            format!("160000 {oid} 2\tsub").into_bytes(),
            invalid_index_path,
        ] {
            assert!(
                parse_index_gitlinks(&out, &["ls-files"]).is_err(),
                "{out:?}"
            );
        }
        assert!(parse_tree_gitlinks(b"", &["ls-tree"]).unwrap().is_empty());
        assert!(parse_index_gitlinks(b"", &["ls-files"]).unwrap().is_empty());
    }

    #[test]
    fn gitlink_census_covers_head_and_index_and_detects_retained_module_metadata() {
        let (_owner, checkout, submodule) = submodule_fixture();
        assert_eq!(checkout.tracked_submodule_paths().unwrap(), vec!["sub"]);
        assert!(checkout.has_submodule_git_metadata().unwrap());

        let oid = git(&submodule, &["rev-parse", "HEAD"]);
        let cacheinfo = format!("160000,{oid},staged-submodule");
        git(
            checkout.root(),
            &["update-index", "--add", "--cacheinfo", &cacheinfo],
        );
        assert_eq!(
            checkout.tracked_submodule_paths().unwrap(),
            vec!["staged-submodule", "sub"]
        );
        git(
            checkout.root(),
            &["update-index", "--force-remove", "staged-submodule"],
        );

        git(checkout.root(), &["rm", "-f", "--", "sub"]);
        git(checkout.root(), &["commit", "-qm", "remove submodule"]);
        assert!(checkout.tracked_submodule_paths().unwrap().is_empty());
        assert!(checkout.has_submodule_git_metadata().unwrap());
    }

    #[test]
    fn git_status_and_recovery_patch_include_gitlink_changes_under_ignore_all_config() {
        let (_owner, checkout, submodule) = submodule_fixture();
        let initial_oid = git(&submodule, &["rev-parse", "HEAD"]);
        std::fs::write(submodule.join("tracked.txt"), "second commit\n").unwrap();
        git(&submodule, &["add", "tracked.txt"]);
        git(&submodule, &["commit", "-qm", "second submodule commit"]);
        let changed_oid = git(&submodule, &["rev-parse", "HEAD"]);
        assert!(
            checkout
                .dirty_paths()
                .unwrap()
                .iter()
                .any(|path| path == "sub")
        );
        let pointer_patch = String::from_utf8(checkout.recovery_patch(false).unwrap()).unwrap();
        assert!(
            pointer_patch.contains(&format!("Subproject commit {initial_oid}")),
            "{pointer_patch}"
        );
        assert!(
            pointer_patch.contains(&format!("Subproject commit {changed_oid}")),
            "{pointer_patch}"
        );

        git(&submodule, &["checkout", "--detach", &initial_oid]);
        std::fs::write(submodule.join("tracked.txt"), "modified\n").unwrap();
        assert!(
            checkout
                .dirty_paths()
                .unwrap()
                .iter()
                .any(|path| path == "sub")
        );
        git(&submodule, &["checkout", "--", "tracked.txt"]);
        std::fs::write(submodule.join("untracked.txt"), "untracked\n").unwrap();
        assert!(
            checkout
                .dirty_paths()
                .unwrap()
                .iter()
                .any(|path| path == "sub")
        );
    }

    #[test]
    fn staged_gitlink_patch_overrides_ignore_all_config() {
        let owner = tempfile::tempdir().unwrap();
        let source = owner.path().join("object-source");
        let repository = owner.path().join("superproject");
        init_repo(&source);
        std::fs::write(source.join("commit.txt"), "object source\n").unwrap();
        git(&source, &["add", "commit.txt"]);
        git(&source, &["commit", "-qm", "object source"]);
        let initial_oid = git(&source, &["rev-parse", "HEAD"]);
        std::fs::write(source.join("commit.txt"), "second object source\n").unwrap();
        git(&source, &["add", "commit.txt"]);
        git(&source, &["commit", "-qm", "second object source"]);
        let changed_oid = git(&source, &["rev-parse", "HEAD"]);

        init_repo(&repository);
        std::fs::write(repository.join("README.md"), "fixture\n").unwrap();
        git(&repository, &["add", "README.md"]);
        git(&repository, &["commit", "-qm", "superproject initial"]);
        let cacheinfo = format!("160000,{initial_oid},sub");
        git(
            &repository,
            &["update-index", "--add", "--cacheinfo", &cacheinfo],
        );
        git(&repository, &["commit", "-qm", "initial gitlink"]);
        git(&repository, &["config", "diff.ignoreSubmodules", "all"]);
        let changed_cacheinfo = format!("160000,{changed_oid},sub");
        git(
            &repository,
            &["update-index", "--cacheinfo", &changed_cacheinfo],
        );

        let checkout = GitRepo::discover(&repository).unwrap();
        let patch = String::from_utf8(checkout.recovery_patch(true).unwrap()).unwrap();
        assert!(
            patch.contains(&format!("Subproject commit {initial_oid}")),
            "{patch}"
        );
        assert!(
            patch.contains(&format!("Subproject commit {changed_oid}")),
            "{patch}"
        );
    }

    #[test]
    fn recovery_patch_output_limit_accepts_boundary_and_refuses_truncation() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        init_repo(&repository);
        std::fs::write(repository.join("tracked.txt"), "initial\n").unwrap();
        git(&repository, &["add", "tracked.txt"]);
        git(&repository, &["commit", "-qm", "initial"]);
        std::fs::write(repository.join("tracked.txt"), "changed\n").unwrap();

        let checkout = GitRepo::discover(&repository).unwrap();
        let patch = checkout.recovery_patch(false).unwrap();
        assert!(!patch.is_empty());
        assert_eq!(
            checkout
                .recovery_patch_with_output_limit(false, patch.len())
                .unwrap(),
            patch
        );

        let error = checkout
            .recovery_patch_with_output_limit(false, patch.len() - 1)
            .unwrap_err();
        assert!(
            matches!(&error, GitError::UntrustedOutput { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("output limit"), "{error}");
    }
}

#[cfg(test)]
mod nul_path_tests {
    use super::parse_nul_paths;

    #[test]
    fn nul_separated_paths_round_trip() {
        let out = "src/a.rs\0dir with space/b.rs\0c.rs\0";
        assert_eq!(
            parse_nul_paths(out),
            vec!["src/a.rs", "dir with space/b.rs", "c.rs"]
        );
    }

    #[test]
    fn shim_decorations_never_become_paths() {
        // Observed 2026-07-17: a PATH shim wrapped `diff --name-only`
        // output in a "--- Changes ---" header and blank lines, which the
        // old line-based parse recorded as leases. In -z output such
        // decorations carry newlines; every chunk containing one is
        // rejected rather than risking a phantom path.
        let out = "--- Changes ---\nsrc/a.rs\0src/b.rs\0src/c.rs\n\u{1f4ca} 3 files\0";
        assert_eq!(parse_nul_paths(out), vec!["src/b.rs"]);
    }

    #[test]
    fn empty_output_yields_no_paths() {
        assert!(parse_nul_paths("").is_empty());
        assert!(parse_nul_paths("\0\0").is_empty());
    }
}

#[cfg(all(test, unix))]
mod ignored_paths_readonly_tests {
    use super::{GitError, GitRepo};
    use std::process::Command;

    fn ignored_repo() -> (tempfile::TempDir, GitRepo) {
        let root = tempfile::tempdir().expect("temporary repository");
        let output = Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .output()
            .expect("git init");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::write(root.path().join(".gitignore"), "ignored-*\n").expect("write ignore rules");
        let repo = GitRepo::discover(root.path()).expect("discover repository");
        (root, repo)
    }

    #[test]
    fn ignored_listing_collapses_directories_and_keeps_standalone_files() {
        let (root, repo) = ignored_repo();
        std::fs::write(root.path().join("ignored-file"), "data").unwrap();
        std::fs::create_dir(root.path().join("ignored-dir")).unwrap();
        std::fs::write(root.path().join("ignored-dir/nested"), "data").unwrap();

        let mut paths = repo.ignored_paths_readonly().unwrap();
        paths.sort();
        assert_eq!(paths, vec!["ignored-dir/", "ignored-file"]);
    }

    #[test]
    fn newline_in_ignored_standalone_path_is_preserved() {
        let (root, repo) = ignored_repo();
        std::fs::write(root.path().join("ignored-line\nbreak"), "data").unwrap();

        assert_eq!(
            repo.ignored_paths_readonly().unwrap(),
            vec!["ignored-line\nbreak"]
        );
    }

    #[test]
    fn non_utf8_ignored_standalone_path_fails_closed() {
        let (root, repo) = ignored_repo();
        let Some(_path) =
            aethyme_testkit::filesystem::write_non_utf8_file(root.path(), b"ignored-\xff", b"data")
                .unwrap()
        else {
            return;
        };

        let error = repo.ignored_paths_readonly().unwrap_err();
        assert!(matches!(error, GitError::UntrustedOutput { .. }), "{error}");
    }
}

#[cfg(test)]
mod strict_nul_path_tests {
    use super::{GitError, parse_nul_paths_strict};

    #[test]
    fn strict_nul_paths_preserve_newlines_and_reject_malformed_or_non_utf8() {
        let args = ["diff", "--name-only", "-z"];
        assert_eq!(
            parse_nul_paths_strict(b"tracked\nname\0plain.txt\0", &args).unwrap(),
            vec!["tracked\nname", "plain.txt"]
        );
        assert!(matches!(
            parse_nul_paths_strict(b"invalid-\xffname\0", &args),
            Err(GitError::UntrustedOutput { .. })
        ));
        for malformed in [&b"unterminated"[..], &b"path\0\0"[..]] {
            assert!(matches!(
                parse_nul_paths_strict(malformed, &args),
                Err(GitError::UntrustedOutput { .. })
            ));
        }
        assert!(parse_nul_paths_strict(b"", &args).unwrap().is_empty());
    }
}

/// The probe that decides whether this machine's `git` may be believed (#176).
///
/// Every test here wraps a git the probe has already accepted, so the
/// assertions are about the wrapper rather than about the machine. On a
/// machine where every candidate is wrapped there is nothing honest to wrap,
/// and the tests say so instead of failing.
#[cfg(test)]
mod git_program_tests {
    use super::*;

    fn trustworthy_git() -> Option<PathBuf> {
        match git_output_trust() {
            GitOutputTrust::Undecorated { resolved, .. } => Some(resolved.clone()),
            _ => None,
        }
    }

    /// Write a `git`-named wrapper that forwards to `real`, optionally
    /// printing the decoration observed in the wild.
    fn wrapper(dir: &Path, real: &Path, decorate: bool) -> PathBuf {
        let path = dir.join("git");
        let tail = if decorate {
            // Exactly what the observed wrapper emits: six bytes and no
            // trailing newline, so `trim_end` on the captured output is no
            // defence against it.
            "printf 'ok \\342\\234\\223'\n"
        } else {
            ""
        };
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n\"{}\" \"$@\"\nstatus=$?\n{tail}exit $status\n",
                real.display()
            ),
        )
        .expect("write wrapper");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod wrapper");
        }
        path
    }

    /// `porcelain_probe` on a wrapper this test just wrote. On Linux a sibling
    /// test that forks while the wrapper's write handle is still open holds
    /// that descriptor until its child execs, and executing the wrapper in
    /// that window fails with ETXTBSY ("Text file busy"). That is a property
    /// of the test harness, not of the probe, so retry only that error.
    fn probe(path: &Path) -> PorcelainProbe {
        let mut attempts = 0;
        loop {
            let result = porcelain_probe(path);
            let busy = matches!(
                &result,
                PorcelainProbe::Unusable { reason } if reason.contains("Text file busy")
            );
            attempts += 1;
            if !busy || attempts >= 50 {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// A wrapper that prints only git's own bytes and lies in its exit code:
    /// `git diff --quiet` always succeeds. This is #178's wrapper reduced to
    /// the one behaviour that matters, and it is exactly what a probe reading
    /// stdout cannot see.
    fn quiet_rewriting_wrapper(dir: &Path, real: &Path) -> PathBuf {
        let path = dir.join("git");
        std::fs::write(
            &path,
            format!(
                r#"#!/bin/sh
"{}" "$@"
status=$?
for arg in "$@"; do
    if [ "$arg" = "--quiet" ]; then exit 0; fi
done
exit $status
"#,
                real.display()
            ),
        )
        .expect("write wrapper");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod wrapper");
        }
        path
    }

    /// A wrapper is not itself the problem — plenty of machines route git
    /// through one legitimately. Rejecting on shape rather than on behaviour
    /// would make the refusal fire where nothing is wrong.
    #[test]
    fn a_wrapper_that_forwards_faithfully_is_accepted() {
        let Some(real) = trustworthy_git() else {
            eprintln!("every git on PATH is wrapped; nothing honest to wrap");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = wrapper(dir.path(), &real, false);
        assert_eq!(probe(&path), PorcelainProbe::Undecorated);
    }

    /// The #176 wrapper: six bytes on a clean repository, which made every
    /// worktree on the machine read dirty and permanently ineligible for
    /// cleanup.
    #[test]
    fn a_wrapper_that_decorates_porcelain_is_caught() {
        let Some(real) = trustworthy_git() else {
            eprintln!("every git on PATH is wrapped; nothing honest to wrap");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = wrapper(dir.path(), &real, true);
        assert_eq!(
            probe(&path),
            PorcelainProbe::Decorated { bytes: 6 },
            "`ok \u{2713}\\n` is six bytes, and none of them are a porcelain entry"
        );
    }

    /// The gap #178 found: every byte this wrapper prints is git's own, so
    /// the emptiness check accepts it, and `diff --quiet` still always
    /// succeeds. `paths_equal` read that status as proof that differing
    /// content was identical -- the sole automatic evidence an unattended
    /// cleanup discards local work on.
    #[test]
    fn a_wrapper_that_rewrites_an_exit_code_is_caught_despite_printing_nothing() {
        let Some(real) = trustworthy_git() else {
            eprintln!("every git on PATH is wrapped; nothing honest to wrap");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = quiet_rewriting_wrapper(dir.path(), &real);
        match probe(&path) {
            PorcelainProbe::StatusRewritten { detail } => assert!(
                detail.contains("where git exits 1"),
                "the rejection should name the direction that costs work: {detail}"
            ),
            other => panic!("a rewritten exit code should be refused, got {other:?}"),
        }
    }

    /// The complement, and the reason the probe checks both directions: a
    /// wrapper forcing `--quiet` to *fail* would make every comparison read
    /// "differs", which is merely conservative. Refusing it anyway keeps the
    /// probe a statement about honesty rather than about which way the lie
    /// happens to lean.
    #[test]
    fn the_probe_asks_about_an_unmodified_file_too() {
        let Some(real) = trustworthy_git() else {
            eprintln!("every git on PATH is wrapped; nothing honest to wrap");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("git");
        std::fs::write(
            &path,
            format!(
                r#"#!/bin/sh
"{}" "$@"
status=$?
for arg in "$@"; do
    if [ "$arg" = "--quiet" ]; then exit 1; fi
done
exit $status
"#,
                real.display()
            ),
        )
        .expect("write wrapper");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod wrapper");
        }
        match probe(&path) {
            PorcelainProbe::StatusRewritten { detail } => assert!(
                detail.contains("where git exits 0"),
                "the unmodified case should be the one that fired: {detail}"
            ),
            other => panic!("a rewritten exit code should be refused, got {other:?}"),
        }
    }

    /// Write a `git`-named shim whose body is `script`, with `{real}`
    /// replaced by the honest binary. The two tests below differ only in a
    /// few lines of shell, so the file-writing half is not worth repeating.
    fn shim(dir: &Path, real: &Path, script: &str) -> PathBuf {
        let path = dir.join("git");
        std::fs::write(&path, script.replace("{real}", &real.display().to_string()))
            .expect("write shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod shim");
        }
        path
    }

    /// The gap #179's review found, and the narrowest wrapper that exploits
    /// it: honest `status`, honest exit codes, honest everything the probe
    /// asked about before this commit -- and stdout discarded for
    /// `diff --name-only` alone. That is the one command `paths_equal`
    /// actually reads, and empty output is what it takes for proof that two
    /// commits agree. Filed `Undecorated`, this binary makes every comparison
    /// answer "identical", which is the sole automatic evidence for
    /// classifying a promotion `SupersededUpstream` and discarding local work.
    #[test]
    fn a_wrapper_that_empties_name_only_is_caught_though_every_exit_code_is_honest() {
        let Some(real) = trustworthy_git() else {
            eprintln!("every git on PATH is wrapped; nothing honest to wrap");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = shim(
            dir.path(),
            &real,
            r#"#!/bin/sh
for arg in "$@"; do
    if [ "$arg" = "--name-only" ]; then
        "{real}" "$@" >/dev/null
        exit $?
    fi
done
exec "{real}" "$@"
"#,
        );
        match probe(&path) {
            PorcelainProbe::StatusRewritten { detail } => assert!(
                detail.contains("look identical"),
                "the rejection should name the consequence, not just the bytes: {detail}"
            ),
            other => panic!("an emptied --name-only should be refused, got {other:?}"),
        }
    }

    /// Printing on `--quiet` is decoration even though the exit code is
    /// right, and it reaches further than a wrong answer: before this commit
    /// the probe ran `diff --quiet` with `status()`, handing the child the
    /// broker's own stdout, so these bytes landed ahead of the JSON of
    /// whichever `aethyme ... --json` ran first. `Decorated` and not
    /// `Unusable` -- the binary works, it lies.
    #[test]
    fn a_wrapper_that_prints_while_being_quiet_is_decoration_not_breakage() {
        let Some(real) = trustworthy_git() else {
            eprintln!("every git on PATH is wrapped; nothing honest to wrap");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = shim(
            dir.path(),
            &real,
            r#"#!/bin/sh
"{real}" "$@"
status=$?
for arg in "$@"; do
    if [ "$arg" = "--quiet" ]; then printf 'ok'; fi
done
exit $status
"#,
        );
        assert_eq!(
            probe(&path),
            PorcelainProbe::Decorated { bytes: 2 },
            "a --quiet that writes two bytes to the broker's stdout is not quiet"
        );
    }

    /// A candidate that cannot run proves nothing about wrappers, and only
    /// `Decorated` makes the dirtiness predicates refuse. Conflating the two
    /// would turn a stale PATH entry into a broker that will not judge any
    /// checkout on the machine.
    #[test]
    fn a_candidate_that_cannot_run_is_unusable_rather_than_decorated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("not-a-real-git");
        assert!(matches!(
            porcelain_probe(&missing),
            PorcelainProbe::Unusable { .. }
        ));
    }
}

/// `paths_equal` is the predicate #178 was about: the only automatic evidence
/// for calling a promoted entry superseded, read by an unattended cleanup.
#[cfg(test)]
mod paths_equal_tests {
    use super::*;

    fn run(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.test")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.test")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Two commits, one path changed between them, one untouched. The
    /// untouched path must read equal and the changed one must not -- the
    /// answer this function got wrong on a wrapped machine, in the direction
    /// that discards work.
    #[test]
    fn a_changed_path_is_not_equal_and_an_untouched_one_is() {
        if !matches!(git_output_trust(), GitOutputTrust::Undecorated { .. }) {
            eprintln!("no honest git on PATH; the predicate refuses by design");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("changed.txt"), "one\n").expect("write");
        std::fs::write(root.join("stable.txt"), "same\n").expect("write");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "first"]);
        std::fs::write(root.join("changed.txt"), "two\n").expect("write");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "second"]);

        let repo = GitRepo::discover(root).expect("discover");
        assert!(
            !repo
                .paths_equal("HEAD~1", "HEAD", &["changed.txt".to_string()])
                .expect("compare"),
            "a path with different content is not equal"
        );
        assert!(
            repo.paths_equal("HEAD~1", "HEAD", &["stable.txt".to_string()])
                .expect("compare"),
            "a path neither commit touched is equal"
        );
    }

    /// No paths is not evidence of anything, and the caller asking about none
    /// is asking a vacuous question. Answering `true` keeps the classifier's
    /// "every changed path matches upstream" reading correct for an entry that
    /// changed nothing; it is never reached with a real candidate's file list.
    #[test]
    fn an_empty_path_list_is_equal_without_running_git() {
        let repo = GitRepo {
            root: PathBuf::from("/nonexistent-on-purpose"),
        };
        assert!(repo.paths_equal("a", "b", &[]).expect("compare"));
    }
}

/// The PATH the broker hands a subprocess, as opposed to the git it runs
/// itself. Separate from `paths_equal_tests` because the subject is different:
/// those cover what this crate does with git output, these cover what a gate
/// resolves when it goes looking for tools on its own.
#[cfg(test)]
mod subprocess_path_tests {
    use super::*;

    /// #177: the probe finds an honest git, but a gate resolves its own tools
    /// through PATH and meets the wrapper anyway.
    ///
    /// The directory is dropped, not the single binary -- a shim directory
    /// that rewrites `git` is the same directory that rewrites `cargo`, and
    /// PATH has no finer unit than a directory to remove.
    #[test]
    fn a_dishonest_git_s_directory_is_dropped_from_a_subprocess_path() {
        let mut remove = std::collections::BTreeSet::new();
        remove.insert(PathBuf::from("/opt/shims"));
        let inherited = std::env::join_paths(["/opt/shims", "/usr/local/bin", "/usr/bin"]).unwrap();

        let sanitized = path_without(&inherited, &remove).expect("PATH stays joinable");

        let kept: Vec<PathBuf> = std::env::split_paths(&sanitized).collect();
        assert_eq!(
            kept,
            vec![PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")],
            "the shim directory must be gone and the order of the rest preserved"
        );
    }

    /// The control for the test above: filtering must be driven by the probe's
    /// verdict, never by the name of a directory. A build that starts removing
    /// plausible-looking entries would break machines where nothing is wrong.
    #[test]
    fn nothing_is_dropped_when_the_probe_condemned_nothing() {
        let remove = std::collections::BTreeSet::new();
        let inherited = std::env::join_paths(["/opt/shims", "/usr/local/bin", "/usr/bin"]).unwrap();

        let sanitized = path_without(&inherited, &remove).unwrap();

        assert_eq!(
            sanitized, inherited,
            "an unproven PATH is returned untouched"
        );
    }

    /// A stale PATH entry that could not be executed is not evidence of a
    /// wrapper, and its directory may hold the only copy of something the gate
    /// needs. Only `dishonest` candidates may cost a directory.
    #[test]
    fn an_unusable_candidate_is_reported_but_never_removed() {
        let bypassed = vec![
            BypassedGit {
                path: PathBuf::from("/stale/bin/git"),
                reason: "/stale/bin/git: no such file or directory".into(),
                dishonest: false,
            },
            BypassedGit {
                path: PathBuf::from("/opt/shims/git"),
                reason: "/opt/shims/git emitted 6 bytes of porcelain".into(),
                dishonest: true,
            },
        ];

        let remove: std::collections::BTreeSet<PathBuf> = bypassed
            .iter()
            .filter(|entry| entry.dishonest)
            .filter_map(|entry| entry.path.parent().map(Path::to_path_buf))
            .collect();

        assert_eq!(
            remove,
            std::collections::BTreeSet::from([PathBuf::from("/opt/shims")])
        );
        assert!(
            join_reasons(&bypassed).contains("/stale/bin/git"),
            "the operator still hears about it, it just does not cost a directory"
        );
    }

    /// The note is what makes a past verdict attributable, so it may never be
    /// empty -- including in the case where nothing was changed, which is
    /// precisely the case that would otherwise be indistinguishable from a
    /// gate that ran before this existed.
    #[test]
    fn the_subprocess_note_always_names_the_trust_state() {
        let note = subprocess_path_note();
        assert!(note.starts_with(SUBPROCESS_PATH_NOTE_PREFIX), "{note}");
        assert!(note.ends_with('\n'), "{note}");
    }
}

/// Combined ownership queries must preserve checkout context.
#[cfg(test)]
mod worktree_common_dir_tests {
    use super::*;

    #[test]
    fn combined_common_dir_matches_discovery_for_main_nested_and_linked_worktrees() {
        let fixture = tempfile::tempdir().unwrap();
        let main = fixture.path().join("main ' checkout");
        let linked = fixture.path().join("linked ' checkout");
        std::fs::create_dir(&main).unwrap();
        run_git(&main, &["init", "-q", "-b", "main"]).unwrap();
        run_git(
            &main,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--allow-empty",
                "-qm",
                "initial",
            ],
        )
        .unwrap();
        run_git(
            &main,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "worktree",
                "add",
                "-q",
                "-b",
                "agent/linked",
                linked.to_str().unwrap(),
            ],
        )
        .unwrap();
        let nested = main.join("nested");
        let linked_nested = linked.join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::create_dir(&linked_nested).unwrap();
        let expected = GitRepo::discover(&main).unwrap().git_common_dir().unwrap();
        for path in [&main, &nested, &linked, &linked_nested] {
            assert_eq!(GitRepo::worktree_common_dir(path).unwrap(), expected);
            assert_eq!(
                GitRepo::worktree_common_dir(path).unwrap(),
                GitRepo::discover(path).unwrap().git_common_dir().unwrap(),
            );
        }
    }

    #[test]
    fn combined_common_dir_refuses_bare_repositories_and_non_repositories() {
        let fixture = tempfile::tempdir().unwrap();
        assert!(GitRepo::worktree_common_dir(fixture.path()).is_err());
        run_git(fixture.path(), &["init", "--bare", "-q"]).unwrap();
        // A common directory alone cannot establish a checkout: bare Git
        // repositories answer that query successfully too.
        assert!(run_git(fixture.path(), &["rev-parse", "--git-common-dir"]).is_ok());
        assert!(GitRepo::discover(fixture.path()).is_err());
        assert!(GitRepo::worktree_common_dir(fixture.path()).is_err());
    }

    #[test]
    fn combined_common_dir_uses_one_git_query() {
        const ROOT: &str = "AETHYME_COMMON_DIR_QUERY_TEST_ROOT";
        const TEST: &str = "git::worktree_common_dir_tests::combined_common_dir_uses_one_git_query";
        if let Some(root) = std::env::var_os(ROOT) {
            let root = PathBuf::from(root);
            assert_eq!(
                GitRepo::worktree_common_dir(&root).unwrap(),
                root.join(".git").canonicalize().unwrap(),
            );
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("checkout");
        let home = fixture.path().join("home");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&home).unwrap();
        run_git(&root, &["init", "-q", "-b", "main"]).unwrap();
        let global_config = fixture.path().join("empty-git-config");
        std::fs::write(&global_config, "").unwrap();
        let trace = fixture.path().join("trace.jsonl");
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", TEST, "--nocapture"]);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                child.env_remove(key);
            }
        }
        let output = child
            .env(ROOT, &root)
            .env("HOME", &home)
            .env("GIT_CONFIG_GLOBAL", &global_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TRACE2_EVENT", &trace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let events = std::fs::read_to_string(trace).unwrap();
        let queries: Vec<Vec<String>> = events
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|event| event["event"] == "start")
            .filter_map(|event| {
                let args: Vec<String> = event["argv"]
                    .as_array()?
                    .iter()
                    .map(|arg| arg.as_str().unwrap().to_owned())
                    .collect();
                (args.get(1).map(String::as_str) == Some("rev-parse")).then_some(args)
            })
            .collect();
        assert_eq!(
            queries.len(),
            1,
            "ownership lookup must use one Git query: {queries:?}"
        );
        assert_eq!(
            &queries[0][1..],
            &["rev-parse", "--is-inside-work-tree", "--git-common-dir"]
        );
    }

    #[test]
    fn a_repeated_ownership_lookup_is_answered_without_git_until_the_checkout_changes() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        run_git(&root, &["init", "-q", "-b", "main"]).unwrap();
        let root = root.canonicalize().unwrap();
        let common = root.join(".git");
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        let counts = || {
            let (_, counts) = recorder.totals();
            (
                counts
                    .get("worktree_common_dir_probe")
                    .copied()
                    .unwrap_or(0),
                counts
                    .get("worktree_common_dir_remembered")
                    .copied()
                    .unwrap_or(0),
            )
        };

        assert_eq!(GitRepo::worktree_common_dir(&root).unwrap(), common);
        assert_eq!(GitRepo::worktree_common_dir(&root).unwrap(), common);
        assert_eq!(
            counts(),
            (2, 0),
            "the memory is off unless a command enables it"
        );

        repository_identity_cache::ENABLED_ON_THIS_THREAD.with(|enabled| enabled.set(true));
        assert_eq!(GitRepo::worktree_common_dir(&root).unwrap(), common);
        assert_eq!(GitRepo::worktree_common_dir(&root).unwrap(), common);
        assert_eq!(counts(), (3, 1), "the second lookup needs no Git");

        let expired = with_git_deadline(Duration::ZERO, || GitRepo::worktree_common_dir(&root));
        assert!(
            matches!(expired, Err(GitError::TimedOut { .. })),
            "an expired deadline is not answered from memory: {expired:?}"
        );

        // A checkout replaced by something else must be asked about again.
        std::fs::remove_dir_all(&common).unwrap();
        std::fs::write(&common, "gitdir: /nonexistent\n").unwrap();
        assert!(GitRepo::worktree_common_dir(&root).is_err());
        repository_identity_cache::ENABLED_ON_THIS_THREAD.with(|enabled| enabled.set(false));
    }

    #[test]
    fn a_linked_worktree_is_answered_from_its_registration_unless_it_is_damaged() {
        let fixture = tempfile::tempdir().unwrap();
        let main = fixture.path().join("main");
        let linked = fixture.path().join("linked");
        std::fs::create_dir(&main).unwrap();
        run_git(&main, &["init", "-q", "-b", "main"]).unwrap();
        run_git(
            &main,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--allow-empty",
                "-qm",
                "initial",
            ],
        )
        .unwrap();
        run_git(
            &main,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "worktree",
                "add",
                "-q",
                "-b",
                "agent/linked",
                linked.to_str().unwrap(),
            ],
        )
        .unwrap();
        let expected = GitRepo::discover(&main).unwrap().git_common_dir().unwrap();
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        let count = |name: &str| recorder.totals().1.get(name).copied().unwrap_or(0);

        assert_eq!(GitRepo::worktree_common_dir(&linked).unwrap(), expected);
        assert_eq!(count("worktree_common_dir_registered"), 1);
        assert_eq!(count("worktree_common_dir_probe"), 0, "no Git was needed");

        // The main checkout and a path inside the worktree have no
        // registration of their own, so Git answers.
        assert_eq!(GitRepo::worktree_common_dir(&main).unwrap(), expected);
        let nested = linked.join("nested");
        std::fs::create_dir(&nested).unwrap();
        assert_eq!(GitRepo::worktree_common_dir(&nested).unwrap(), expected);
        assert_eq!(count("worktree_common_dir_probe"), 2);

        // A registration that no longer points back at this checkout is not
        // believed: the moved copy is put to Git.
        let moved = fixture.path().join("moved");
        std::fs::rename(&linked, &moved).unwrap();
        let registered = count("worktree_common_dir_registered");
        let _ = GitRepo::worktree_common_dir(&moved);
        assert_eq!(count("worktree_common_dir_registered"), registered);
        assert_eq!(count("worktree_common_dir_probe"), 3);

        // Nor is one whose Git directory has been removed.
        std::fs::rename(&moved, &linked).unwrap();
        let admin = expected.join("worktrees").join("linked");
        std::fs::remove_dir_all(&admin).unwrap();
        assert!(GitRepo::worktree_common_dir(&linked).is_err());
        assert_eq!(count("worktree_common_dir_registered"), registered);
    }

    #[test]
    fn a_remembered_registration_is_not_reread_until_its_git_file_changes() {
        let fixture = tempfile::tempdir().unwrap();
        let main = fixture.path().join("main");
        let linked = fixture.path().join("linked");
        let moved = fixture.path().join("moved");
        std::fs::create_dir(&main).unwrap();
        run_git(&main, &["init", "-q", "-b", "main"]).unwrap();
        run_git(
            &main,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--allow-empty",
                "-qm",
                "initial",
            ],
        )
        .unwrap();
        run_git(
            &main,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "worktree",
                "add",
                "-q",
                "-b",
                "agent/linked",
                linked.to_str().unwrap(),
            ],
        )
        .unwrap();
        let expected = GitRepo::discover(&main).unwrap().git_common_dir().unwrap();
        let recorder = crate::phase_telemetry::Recorder::default();
        let _installed = crate::phase_telemetry::install(recorder.clone());
        let count = |name: &str| recorder.totals().1.get(name).copied().unwrap_or(0);
        repository_identity_cache::ENABLED_ON_THIS_THREAD.with(|enabled| enabled.set(true));

        assert_eq!(GitRepo::worktree_common_dir(&linked).unwrap(), expected);
        assert_eq!(GitRepo::worktree_common_dir(&linked).unwrap(), expected);
        assert_eq!(count("worktree_common_dir_registered"), 1);
        assert_eq!(
            count("worktree_common_dir_remembered"),
            1,
            "the second lookup reuses the registration's answer"
        );
        assert_eq!(count("worktree_common_dir_probe"), 0, "no Git was needed");

        // A checkout moved away has no `.git` entry at the remembered path.
        std::fs::rename(&linked, &moved).unwrap();
        assert!(GitRepo::worktree_common_dir(&linked).is_err());
        assert_eq!(count("worktree_common_dir_remembered"), 1);
        assert_eq!(count("worktree_common_dir_registered"), 1);
        std::fs::rename(&moved, &linked).unwrap();

        // A re-pointed `.git` file (longer, so a different entry) is not
        // answered from memory, and its broken registration goes to Git.
        let pointer = std::fs::read_to_string(linked.join(".git")).unwrap();
        let re_pointed = format!("{}-re-pointed\n", pointer.trim_end());
        std::fs::write(linked.join(".git"), re_pointed).unwrap();
        assert!(GitRepo::worktree_common_dir(&linked).is_err());
        assert_eq!(count("worktree_common_dir_remembered"), 1);
        assert_eq!(count("worktree_common_dir_registered"), 1);
        repository_identity_cache::ENABLED_ON_THIS_THREAD.with(|enabled| enabled.set(false));
    }

    #[test]
    fn combined_common_dir_preserves_expired_deadlines() {
        let fixture = tempfile::tempdir().unwrap();
        let result = with_git_deadline(Duration::ZERO, || {
            GitRepo::worktree_common_dir(fixture.path())
        });
        assert!(
            matches!(result, Err(GitError::TimedOut { ref args, .. })
                if args == "rev-parse --is-inside-work-tree --git-common-dir"),
            "an unavailable ownership inspection must remain unavailable: {result:?}",
        );
    }
}

/// #219: a git that never returns must become an error, not a hang.
#[cfg(test)]
mod timeout_tests {
    use super::*;

    #[test]
    fn checked_ignore_distinguishes_not_ignored_from_git_failure() {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]).unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir(tmp.path().join("target")).unwrap();
        let repo = GitRepo::discover(tmp.path()).unwrap();

        assert!(repo.path_is_ignored_checked("target").unwrap());
        assert!(!repo.path_is_ignored_checked("src").unwrap());

        let _deadline = limit_git_until(Instant::now() - Duration::from_millis(1));
        assert!(matches!(
            repo.path_is_ignored_checked("target"),
            Err(GitError::TimedOut { .. })
        ));
    }

    #[test]
    fn bounded_merge_simulations_preserve_clean_and_conflicting_results() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        run_git(root, &["init", "-q"]).unwrap();
        run_git(root, &["config", "user.name", "fixture"]).unwrap();
        run_git(root, &["config", "user.email", "fixture@example.invalid"]).unwrap();
        std::fs::write(root.join("file.txt"), "base\n").unwrap();
        run_git(root, &["add", "file.txt"]).unwrap();
        run_git(root, &["commit", "-qm", "base"]).unwrap();
        let repo = GitRepo::discover(root).unwrap();
        let base = repo.head_commit().unwrap();
        std::fs::write(root.join("file.txt"), "left\n").unwrap();
        run_git(root, &["commit", "-qam", "left"]).unwrap();
        let left = repo.head_commit().unwrap();
        run_git(root, &["checkout", "--detach", &base]).unwrap();
        std::fs::write(root.join("file.txt"), "right\n").unwrap();
        run_git(root, &["commit", "-qam", "right"]).unwrap();
        let right = repo.head_commit().unwrap();

        assert!(
            repo.merge_tree_simulate(&base, &left)
                .unwrap()
                .conflicts
                .is_empty()
        );
        assert!(
            repo.merge_tree_with_base(&base, &base, &left)
                .unwrap()
                .conflicts
                .is_empty()
        );
        for result in [
            repo.merge_tree_simulate(&left, &right),
            repo.merge_tree_with_base(&base, &left, &right),
        ] {
            let result = result.expect("Git exit 1 is a conflict result, not a command failure");
            assert_eq!(result.conflicts, vec!["file.txt"]);
            assert!(!result.tree.is_empty());
        }
        assert_eq!(
            repo.head_commit().unwrap(),
            right,
            "simulation must not move HEAD"
        );
    }

    #[test]
    fn merge_simulations_refuse_an_expired_inspection_before_starting_git() {
        let fixture = tempfile::tempdir().unwrap();
        run_git(fixture.path(), &["init", "-q"]).unwrap();
        let repo = GitRepo::discover(fixture.path()).unwrap();
        // Invalid revisions distinguish a spawned Git error from the typed
        // deadline refusal without sleeps or a timing-sensitive assertion.
        let _deadline = limit_git_until(Instant::now() - Duration::from_secs(1));
        for result in [
            repo.merge_tree_simulate("missing-base", "missing-head"),
            repo.merge_tree_with_base("missing-base", "missing-current", "missing-incoming"),
        ] {
            assert!(
                matches!(result, Err(GitError::TimedOut { ref args, .. }) if args.starts_with("merge-tree --write-tree")),
                "an expired inspection must not run a merge simulation: {result:?}"
            );
        }
    }

    #[test]
    fn discovery_preserves_an_expired_deadline_instead_of_denying_the_repository() {
        let fixture = tempfile::tempdir().expect("temporary discovery path");
        let error = with_git_deadline(Duration::ZERO, || GitRepo::discover(fixture.path()))
            .err()
            .expect("an expired deadline cannot discover a repository");
        assert!(
            matches!(&error, GitError::TimedOut { args, .. } if args == "rev-parse --show-toplevel"),
            "discovery must preserve the actual timeout: {error:?}"
        );
        assert!(error.to_string().contains("was not started"), "{error}");
    }

    #[test]
    fn discovery_still_identifies_an_actual_non_repository() {
        let fixture = tempfile::tempdir().expect("temporary non-repository");
        let error = GitRepo::discover(fixture.path())
            .err()
            .expect("the fixture has no repository");
        assert!(
            matches!(&error, GitError::NotARepository { path } if path == fixture.path()),
            "{error:?}"
        );
    }

    #[test]
    fn a_git_that_outlives_its_deadline_is_killed_and_reported_as_timed_out() {
        // A stand-in for a wedged git: the runner does not care what the
        // program is, only that it has not exited when the budget runs out.
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let started = std::time::Instant::now();
        let timeout_generation_before = git_timeout_generation();
        let error = run_git_command(command, &["fetch", "origin"], Duration::from_millis(300))
            .expect_err("a sleeping git must time out");
        assert!(
            matches!(&error, GitError::TimedOut { args, .. } if args == "fetch origin"),
            "{error:?}"
        );
        assert_ne!(
            git_timeout_generation(),
            timeout_generation_before,
            "a real subprocess timeout must be observable by bounded callers"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(error.to_string().contains(GIT_TIMEOUT_ENV), "{error}");
        assert!(
            error.to_string().contains("caller caps may be shorter"),
            "{error}"
        );
        let GitError::TimedOut { detail, .. } = &error else {
            unreachable!("matched above")
        };
        assert_eq!(detail.source, GitTimeoutSource::PerCall);
        assert!(detail.started);
        assert_eq!(detail.subcommand, "fetch");
        assert_eq!(detail.limit_ms, 300);
        assert!(detail.elapsed_ms >= 250, "{detail:?}");
    }

    #[test]
    fn the_scoped_deadline_is_named_as_the_source_when_it_cuts_git_short() {
        let error = with_git_deadline(Duration::from_millis(300), || {
            let mut command = Command::new("sh");
            command.args(["-c", "exec sleep 30"]);
            run_git_command(command, &["status"], Duration::from_secs(60))
                .expect_err("the scoped deadline must stop a wedged git")
        });
        let GitError::TimedOut { detail, .. } = &error else {
            panic!("expected a timeout: {error:?}");
        };
        assert_eq!(detail.source, GitTimeoutSource::OuterDeadline);
        assert!(detail.started);
        assert!(
            error.to_string().contains("outer command deadline"),
            "{error}"
        );
        assert!(
            !error.to_string().contains("killed"),
            "the message must not claim a kill: {error}"
        );
    }

    #[test]
    fn a_spent_budget_says_git_was_not_started_and_never_says_killed() {
        let error = with_git_deadline(Duration::ZERO, || {
            let mut command = Command::new("sh");
            command.current_dir(std::env::temp_dir());
            run_git_command(
                command,
                &["-c", "core.fsmonitor=false", "status"],
                git_timeout(),
            )
            .expect_err("a spent budget cannot start git")
        });
        let GitError::TimedOut { detail, .. } = &error else {
            panic!("expected a timeout: {error:?}");
        };
        assert_eq!(detail.source, GitTimeoutSource::NotStarted);
        assert!(!detail.started);
        assert_eq!(detail.subcommand, "status");
        let message = error.to_string();
        assert!(
            message.starts_with("git status was not started"),
            "{message}"
        );
        assert!(!message.contains("killed"), "{message}");
        let json = error.timeout_json().expect("a timeout has a JSON form");
        assert_eq!(json["source"], "not_started");
        assert_eq!(json["started"], false);
        assert_eq!(json["subcommand"], "status");
    }

    #[test]
    fn the_timeout_message_reads_as_one_line_with_a_shortened_directory() {
        let per_call = GitTimeoutDetail {
            source: GitTimeoutSource::PerCall,
            started: true,
            subcommand: git_subcommand(
                "-c core.fsmonitor=false -c core.untrackedCache=false status --porcelain",
            ),
            directory: Some(git_timeout_directory(Path::new(
                "/home/someone/work/repo-a/wt-1",
            ))),
            limit_ms: 15_000,
            elapsed_ms: 16_200,
        };
        let message = per_call.message("status");
        assert!(
            message.starts_with(
                "git status timed out after 16.2 s (limit 15.0 s, per-call) in repo-a/wt-1"
            ),
            "{message}"
        );
        assert!(
            !message.contains('\n') && !message.contains("someone"),
            "{message}"
        );
        let not_started = GitTimeoutDetail {
            source: GitTimeoutSource::NotStarted,
            started: false,
            elapsed_ms: 0,
            limit_ms: 300,
            ..per_call.clone()
        };
        assert_eq!(
            not_started.message("status"),
            "git status was not started: the command's deadline had 0.3 s left in repo-a/wt-1"
        );
        assert_eq!(git_timeout_directory(Path::new("/")), ".");
    }

    #[test]
    fn an_interactive_deadline_caps_a_git_call_with_a_larger_local_budget() {
        let deadline = Instant::now() + Duration::from_millis(300);
        let _limit = limit_git_until(deadline);
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let started = Instant::now();
        let error = run_git_command(command, &["status"], Duration::from_secs(30))
            .expect_err("the enclosing inspection deadline must cap this command");
        assert!(matches!(error, GitError::TimedOut { .. }), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn an_expired_interactive_deadline_does_not_spawn_git() {
        let _limit = limit_git_until(Instant::now() - Duration::from_millis(1));
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);
        let error = run_git_command(command, &["status"], Duration::from_secs(30))
            .expect_err("an expired report must not start another Git command");
        assert!(matches!(error, GitError::TimedOut { .. }), "{error:?}");
    }

    /// A finish scope (#616) and a report scope (#460) share one deadline:
    /// nesting keeps the earlier instant, and either way of lifting it lifts
    /// both, so neither mechanism can cut off a write the other suspended.
    #[test]
    fn finish_and_report_deadlines_nest_and_lift_as_one() {
        let general = git_timeout();
        let report = Instant::now() + Duration::from_secs(5);
        let _report = limit_git_until(report);
        with_git_deadline(Duration::from_secs(60), || {
            assert_eq!(active_git_deadline(), Some(report), "the earlier wins");
            without_git_deadline(|| {
                assert_eq!(active_git_deadline(), None);
                assert_eq!(git_timeout(), general);
            });
            assert_eq!(active_git_deadline(), Some(report));
        });
        without_git_deadline(|| {
            assert_eq!(
                active_git_deadline(),
                None,
                "a finish lift lifts a report deadline"
            );
            assert_eq!(git_timeout(), general);
        });
        assert_eq!(active_git_deadline(), Some(report));
    }

    #[test]
    fn lifting_the_deadline_restores_the_general_budget_then_the_outer_one() {
        let general = git_timeout();
        with_git_deadline(Duration::from_secs(5), || {
            assert!(git_timeout() <= Duration::from_secs(5));
            without_git_deadline(|| assert_eq!(git_timeout(), general));
            assert!(git_timeout() <= Duration::from_secs(5));
        });
        assert_eq!(git_timeout(), general);
    }

    #[test]
    fn a_scoped_deadline_bounds_git_and_restores_the_previous_budget() {
        let previous = git_timeout();
        let started = Instant::now();
        let error = with_git_deadline(Duration::from_millis(300), || {
            let mut command = Command::new("sh");
            command.args(["-c", "exec sleep 30"]);
            run_git_command(command, &["status"], git_timeout())
                .expect_err("the scoped finish budget must stop a wedged git")
        });
        assert!(matches!(error, GitError::TimedOut { seconds: 1, .. }));
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(git_timeout(), previous);
    }

    #[test]
    fn a_git_that_finishes_in_time_returns_its_trimmed_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf ' M path\\n'"]);
        let out = run_git_command(command, &["status"], Duration::from_secs(30)).unwrap();
        assert_eq!(out, " M path");
    }

    #[test]
    fn the_timeout_defaults_to_ten_minutes_and_honours_a_positive_override() {
        assert_eq!(git_timeout_from(None), Duration::from_secs(600));
        assert_eq!(git_timeout_from(Some("")), Duration::from_secs(600));
        assert_eq!(git_timeout_from(Some("0")), Duration::from_secs(600));
        assert_eq!(git_timeout_from(Some("soon")), Duration::from_secs(600));
        assert_eq!(git_timeout_from(Some(" 45 ")), Duration::from_secs(45));
    }
}

/// Hunk-level views of one path, for telling two sessions' edits apart.
impl GitRepo {
    /// A handle on a checkout whose top level is already known, such as a
    /// session worktree the broker created. Skips the `rev-parse` that
    /// [`GitRepo::discover`] spends, which matters on the agent hook's
    /// per-edit path; a wrong root only makes later Git calls fail.
    pub(crate) fn at_known_root(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Whether `path` exists in the working tree as an untracked,
    /// non-ignored file: a new file no diff against a commit reports.
    /// The patch of `paths` from `from` to `to`, or to the working tree when
    /// `to` is `None`, with paths taken literally and taking no index lock in a
    /// worktree another session is using.
    pub(crate) fn overlap_diff(
        &self,
        from: &str,
        to: Option<&str>,
        paths: &[String],
    ) -> Result<String, GitError> {
        let mut args = vec![
            "--no-optional-locks",
            "--literal-pathspecs",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--unified=2",
            from,
        ];
        args.extend(to);
        args.push("--");
        args.extend(paths.iter().map(String::as_str));
        run_git(&self.root, &args)
    }

    /// Untracked, unignored files under `paths`.
    pub(crate) fn untracked_under(&self, paths: &[String]) -> Result<Vec<String>, GitError> {
        let mut args = vec![
            "--no-optional-locks",
            "--literal-pathspecs",
            "ls-files",
            "--others",
            "--exclude-standard",
            "--",
        ];
        args.extend(paths.iter().map(String::as_str));
        Ok(run_git(&self.root, &args)?
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Which of `paths` differ between two commits.
    pub(crate) fn changed_between_paths(
        &self,
        from: &str,
        to: &str,
        paths: &[String],
    ) -> Result<Vec<String>, GitError> {
        let mut args = vec![
            "--literal-pathspecs",
            "diff",
            "--name-only",
            "--no-ext-diff",
            from,
            to,
            "--",
        ];
        args.extend(paths.iter().map(String::as_str));
        Ok(run_git(&self.root, &args)?
            .lines()
            .map(str::to_string)
            .collect())
    }

    pub(crate) fn path_is_untracked(&self, path: &str) -> Result<bool, GitError> {
        let listed = run_git(
            &self.root,
            &["ls-files", "--others", "--exclude-standard", "--", path],
        )?;
        Ok(!listed.trim().is_empty())
    }

    /// Zero-context patch of `path` from `base` to `head`. Callers parse only
    /// the `@@` headers, so the old-side line ranges are the whole answer.
    pub(crate) fn zero_context_diff(
        &self,
        base: &str,
        head: &str,
        path: &str,
    ) -> Result<String, GitError> {
        run_git(
            &self.root,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--unified=0",
                base,
                head,
                "--",
                path,
            ],
        )
    }

    /// Patch from the exact commit to an isolated working copy.
    pub(crate) fn isolated_working_patch(&self, commit: &str) -> Result<Vec<u8>, GitError> {
        run_isolated_graph_git(
            &self.root,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--binary",
                commit,
                "--",
            ],
        )
    }

    /// Write a synthetic commit in a disposable repository and move its
    /// detached HEAD and private index to that commit without rewriting files.
    pub(crate) fn isolated_candidate_commit(
        &self,
        base: &str,
        tree: &str,
    ) -> Result<String, GitError> {
        graph_snapshot_object_format(base)?;
        graph_snapshot_object_format(tree)?;
        let bytes = run_isolated_graph_git(
            &self.root,
            &[
                "-c",
                "user.name=Aethyme",
                "-c",
                "user.email=broker@aethyme.local",
                "commit-tree",
                tree,
                "-p",
                base,
                "-m",
                "Aethyme semantic patch preview",
            ],
        )?;
        let commit = std::str::from_utf8(&bytes)
            .map_err(|_| graph_snapshot_error("invalid candidate commit ID"))?
            .trim();
        graph_snapshot_object_format(commit)?;
        run_isolated_graph_git(&self.root, &["reset", "--quiet", "--mixed", commit])?;
        Ok(commit.to_owned())
    }

    /// Zero-context patch of `path` from `base` to the working tree, so
    /// uncommitted edits count. Read by the agent hook to describe where
    /// another session is changing a file; only the `@@` headers are used.
    pub(crate) fn working_zero_context_diff(
        &self,
        base: &str,
        path: &str,
    ) -> Result<String, GitError> {
        run_git(
            &self.root,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--unified=0",
                base,
                "--",
                path,
            ],
        )
    }

    /// Commit time, in seconds, of the oldest commit in `base..head` that
    /// touches `path`; `None` when no commit in the range does.
    pub(crate) fn first_commit_time_touching(
        &self,
        base: &str,
        head: &str,
        path: &str,
    ) -> Result<Option<i64>, GitError> {
        let range = format!("{base}..{head}");
        let out = run_git(
            &self.root,
            &["log", "--reverse", "--format=%ct", &range, "--", path],
        )?;
        Ok(out.lines().next().and_then(|line| line.trim().parse().ok()))
    }
}

#[cfg(test)]
mod local_branch_tips_tests {
    use super::*;

    fn run(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.test")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.test")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The listing stands in for one `resolve_ref` per session on the cleanup
    /// plan behind `broker status`, so it must give the same answer: every
    /// local branch at the commit `resolve_ref` reports, and nothing for a
    /// branch that does not exist.
    #[test]
    fn lists_every_local_branch_at_the_commit_resolve_ref_reports() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "one\n").expect("write");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "first"]);
        run(root, &["branch", "agent/kept"]);
        std::fs::write(root.join("a.txt"), "two\n").expect("write");
        run(root, &["commit", "-qam", "second"]);
        run(root, &["branch", "agent/moved"]);

        let repo = GitRepo::discover(root).expect("discover");
        let tips = repo.local_branch_tips().expect("listing");
        for name in [
            "refs/heads/main",
            "refs/heads/agent/kept",
            "refs/heads/agent/moved",
        ] {
            assert_eq!(tips.get(name), repo.resolve_ref(name).as_ref(), "{name}");
        }
        assert_ne!(
            tips["refs/heads/agent/kept"],
            tips["refs/heads/agent/moved"]
        );
        assert_eq!(tips.len(), 3, "{tips:?}");
        assert!(!tips.contains_key("refs/heads/agent/gone"));
    }
}

/// The batched commit questions submit asks (#463) must answer exactly what
/// the per-commit ones did, on every kind of commit a session carries.
#[cfg(test)]
mod batched_commit_tests {
    use super::*;

    fn fixture_git_command() -> Command {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
    }

    fn run(root: &Path, args: &[&str]) -> String {
        let out = fixture_git_command()
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.test")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.test")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn patch_ids_for_log_args(
        root: &Path,
        commits: &[String],
        log_args: &[&str],
    ) -> (BTreeMap<String, String>, Vec<u8>) {
        use std::io::Write as _;

        let mut log = fixture_git_command()
            .args(log_args)
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn fixture git log");
        {
            let mut stdin = log.stdin.take().expect("git log stdin");
            for commit in commits {
                writeln!(stdin, "{commit}").expect("write commit to git log");
            }
        }
        let log_output = log.wait_with_output().expect("wait for git log");
        assert!(
            log_output.status.success(),
            "git {log_args:?}: {}",
            String::from_utf8_lossy(&log_output.stderr)
        );
        assert!(
            log_output.stdout.len() <= 64 * 1024,
            "fixture patch stream exceeded its small bound: {} bytes",
            log_output.stdout.len()
        );

        let mut patch_id = fixture_git_command()
            .args(PATCH_ID_ARGS)
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn fixture git patch-id");
        patch_id
            .stdin
            .take()
            .expect("git patch-id stdin")
            .write_all(&log_output.stdout)
            .expect("write patch stream to git patch-id");
        let patch_output = patch_id.wait_with_output().expect("wait for git patch-id");
        assert!(
            patch_output.status.success(),
            "git patch-id --verbatim: {}",
            String::from_utf8_lossy(&patch_output.stderr)
        );

        let mut patch_ids = BTreeMap::new();
        for line in String::from_utf8_lossy(&patch_output.stdout).lines() {
            let mut fields = line.split_whitespace();
            if let (Some(patch), Some(commit)) = (fields.next(), fields.next()) {
                patch_ids.insert(commit.to_string(), patch.to_string());
            }
        }
        (patch_ids, log_output.stdout)
    }

    #[test]
    fn only_commits_sharing_a_changed_path_set_have_their_patches_compared() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let commit = |path: &str, body: &str| {
            std::fs::write(root.join(path), body).unwrap();
            run(root, &["add", "--", path]);
            run(root, &["commit", "-qm", path]);
            run(root, &["rev-parse", "HEAD"])
        };
        run(root, &["init", "-q", "-b", "main"]);
        commit("base", "base\n");
        run(root, &["branch", "session"]);
        // A name Git quotes in a listing, so quoting is exercised on both sides.
        let shared = "caf\u{e9} notes.txt";
        let unrelated = [commit("a", "a\n"), commit("b", "b\n")];
        run(root, &["checkout", "-q", "session"]);
        let session = commit(shared, "session\n");
        run(root, &["checkout", "-q", "main"]);
        run(root, &["cherry-pick", "session"]);
        let carrier = run(root, &["rev-parse", "HEAD"]);
        let same_path_other_patch = commit(shared, "session\nand more\n");

        let repo = GitRepo::discover(root).unwrap();
        let left = repo
            .cherry_side_commits("main...session", "--left-only")
            .unwrap();
        let right = repo
            .cherry_side_commits("main...session", "--right-only")
            .unwrap();
        let mut cache = PatchIdCache::default();
        let comparable = repo
            .commits_sharing_changed_paths(&left, &right, &mut cache)
            .unwrap();
        assert_eq!(
            comparable.iter().collect::<std::collections::BTreeSet<_>>(),
            [&carrier, &same_path_other_patch, &session]
                .into_iter()
                .collect(),
            "commits changing other paths cannot be patch-equivalent"
        );
        assert!(unrelated.iter().all(|commit| !comparable.contains(commit)));

        // Narrowing changes which patches are rendered, never the marks.
        let all = left
            .iter()
            .chain(&right)
            .map(|(commit, _)| commit.clone())
            .collect::<Vec<_>>();
        let patch_ids = repo.first_parent_patch_ids(&all).unwrap();
        let marks = |listed: &[(String, bool)], other: &[(String, bool)]| {
            listed
                .iter()
                .map(|(commit, _)| {
                    let equivalent = other
                        .iter()
                        .any(|(candidate, _)| patch_ids[candidate] == patch_ids[commit]);
                    (commit.clone(), equivalent)
                })
                .collect::<Vec<_>>()
        };
        let paired = repo
            .cherry_marked_both_cached("main", "session", &mut cache)
            .unwrap();
        assert_eq!(paired.left_only, marks(&left, &right));
        assert_eq!(paired.right_only, marks(&right, &left));
        assert_eq!(paired.right_only, vec![(session, true)]);
        assert_eq!(
            repo.cherry_marked("main", "session", CherrySide::Left)
                .unwrap(),
            paired.left_only
        );
        assert_eq!(
            cache.entries.len(),
            3,
            "only the comparable commits were hashed"
        );
    }

    #[test]
    fn patch_id_cache_reuses_complete_batches_without_changing_cherry_marks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("base"), "base\n").unwrap();
        run(root, &["add", "base"]);
        run(root, &["commit", "-qm", "base"]);
        run(root, &["branch", "worker1"]);
        run(root, &["branch", "worker2"]);
        std::fs::write(root.join("target"), "target\n").unwrap();
        run(root, &["add", "target"]);
        run(root, &["commit", "-qm", "target"]);
        run(root, &["commit", "--allow-empty", "-qm", "empty"]);
        let empty = run(root, &["rev-parse", "HEAD"]);
        for branch in ["worker1", "worker2"] {
            run(root, &["checkout", "-q", branch]);
            std::fs::write(root.join(branch), format!("{branch}\n")).unwrap();
            run(root, &["add", branch]);
            run(root, &["commit", "-qm", branch]);
            // An empty commit on each side shares the target's empty path
            // set, so the target's empty commit is hashed and cached.
            run(root, &["commit", "--allow-empty", "-qm", "empty"]);
        }
        let repo = GitRepo::discover(root).unwrap();
        let mut cache = PatchIdCache::default();
        for branch in ["worker1", "worker2"] {
            let expected = repo.cherry_marked_both("main", branch).unwrap();
            let actual = repo
                .cherry_marked_both_cached("main", branch, &mut cache)
                .unwrap();
            assert_eq!(actual, expected);
        }
        assert!(
            cache.hits >= 2,
            "the second comparison must reuse target patches"
        );
        assert_eq!(cache.entries.get(&(repo.root.clone(), empty)), Some(&None));

        let before = cache.entries.clone();
        let bad = "0000000000000000000000000000000000000001".to_string();
        assert!(
            repo.cached_first_parent_patch_ids(&[bad], &mut cache)
                .is_err()
        );
        assert_eq!(
            cache.entries, before,
            "a failed batch must not populate the cache"
        );

        let deadline = limit_git_until(Instant::now() - Duration::from_millis(1));
        let existing = before.keys().next().unwrap().1.clone();
        assert!(matches!(
            repo.cached_first_parent_patch_ids(&[existing], &mut cache),
            Err(GitError::TimedOut { .. })
        ));
        drop(deadline);
        assert_eq!(
            cache.entries, before,
            "an expired query must not change cached evidence"
        );
    }

    #[test]
    fn empty_opposite_cherry_comparison_skips_patch_streams() {
        const ROOT: &str = "AETHYME_EMPTY_OPPOSITE_TEST_ROOT";
        const MODE: &str = "AETHYME_EMPTY_OPPOSITE_TEST_MODE";
        const TEST: &str =
            "git::batched_commit_tests::empty_opposite_cherry_comparison_skips_patch_streams";
        if let Some(root) = std::env::var_os(ROOT) {
            let root = PathBuf::from(root);
            let repo = GitRepo { root: root.clone() };
            let edit = run(&root, &["rev-parse", "main~1"]);
            let empty = run(&root, &["rev-parse", "main"]);
            if std::env::var(MODE).unwrap() == "ancestry" {
                let expected = vec![(empty, false), (edit, false)];
                assert_eq!(
                    repo.cherry_marked("base", "main", CherrySide::Right)
                        .unwrap(),
                    expected
                );
                assert_eq!(
                    repo.cherry_marked("main", "base", CherrySide::Left)
                        .unwrap(),
                    expected
                );
                assert!(
                    repo.cherry_marked("base", "main", CherrySide::Left)
                        .unwrap()
                        .is_empty()
                );
                let paired = repo.cherry_marked_both("base", "main").unwrap();
                assert!(paired.left_only.is_empty());
                assert_eq!(paired.right_only, expected);
            } else {
                let copied = run(&root, &["rev-parse", "copied"]);
                assert_eq!(
                    repo.cherry_marked("main", "copied", CherrySide::Right)
                        .unwrap(),
                    vec![(copied, true)]
                );
            }
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("payload.bin"), [0_u8, 1, 2]).unwrap();
        run(root, &["add", "payload.bin"]);
        run(root, &["commit", "-qm", "base"]);
        run(root, &["branch", "base"]);
        std::fs::write(root.join("payload.bin"), [0_u8, 2, 3]).unwrap();
        run(root, &["commit", "-qam", "binary edit"]);
        run(root, &["commit", "--allow-empty", "-qm", "empty"]);
        run(root, &["switch", "-q", "-c", "copied", "base"]);
        run(root, &["cherry-pick", "-x", "main~1"]);

        for mode in ["ancestry", "divergent"] {
            let trace = root.join(format!("{mode}-trace.json"));
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", TEST, "--nocapture"]);
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("GIT_") {
                    child.env_remove(key);
                }
            }
            let output = child
                .env(ROOT, root)
                .env(MODE, mode)
                .env("GIT_TRACE2_EVENT", &trace)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let events = std::fs::read_to_string(trace).unwrap();
            let mut saw_rev_list = false;
            let mut saw_patch_stream = false;
            for line in events.lines() {
                let event: serde_json::Value = serde_json::from_str(line).unwrap();
                if event["event"] != "start" {
                    continue;
                }
                let Some(argv) = event["argv"].as_array() else {
                    continue;
                };
                saw_rev_list |= argv.iter().any(|arg| arg == "rev-list");
                saw_patch_stream |= argv.iter().any(|arg| arg == "patch-id" || arg == "log");
            }
            assert!(saw_rev_list, "positive trace witness missing for {mode}");
            assert_eq!(
                saw_patch_stream,
                mode == "divergent",
                "patch stream for {mode}"
            );
        }
    }

    #[test]
    fn paired_cherry_comparison_streams_patch_ids_once() {
        const ROOT: &str = "AETHYME_PAIRED_CHERRY_TEST_ROOT";
        const TEST: &str =
            "git::batched_commit_tests::paired_cherry_comparison_streams_patch_ids_once";
        if let Some(root) = std::env::var_os(ROOT) {
            let repo = GitRepo {
                root: PathBuf::from(root),
            };
            let paired = repo.cherry_marked_both("left", "main").unwrap();
            assert_eq!(paired.left_only.len(), 1);
            assert!(
                paired.left_only[0].1,
                "the cherry-picked patch is present on the right"
            );
            assert_eq!(
                paired
                    .right_only
                    .iter()
                    .map(|(_, equivalent)| *equivalent)
                    .collect::<Vec<_>>(),
                vec![false, false, true],
                "the empty and distinct patches do not match; the cherry-pick does"
            );
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "base\n").unwrap();
        run(root, &["add", "a.txt"]);
        run(root, &["commit", "-qm", "base"]);
        run(root, &["branch", "base"]);
        run(root, &["switch", "-q", "-c", "left", "base"]);
        std::fs::write(root.join("a.txt"), "shared patch\n").unwrap();
        run(root, &["commit", "-qam", "left patch"]);
        run(root, &["switch", "-q", "main"]);
        run(root, &["cherry-pick", "-x", "left"]);
        std::fs::write(root.join("b.txt"), "right-only change\n").unwrap();
        run(root, &["add", "b.txt"]);
        run(root, &["commit", "-qm", "right-only patch"]);
        run(root, &["commit", "--allow-empty", "-qm", "empty patch"]);

        let trace = root.join("paired-trace.json");
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", TEST, "--nocapture"]);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                child.env_remove(key);
            }
        }
        let output = child
            .env(ROOT, root)
            .env("GIT_TRACE2_EVENT", &trace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let events = std::fs::read_to_string(trace).unwrap();
        let (mut rev_lists, mut logs, mut patch_ids) = (0, 0, 0);
        for line in events.lines() {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            if event["event"] != "start" {
                continue;
            }
            let Some(argv) = event["argv"].as_array() else {
                continue;
            };
            if argv.iter().any(|arg| arg == "rev-list") {
                rev_lists += 1;
            }
            if argv.iter().any(|arg| arg == "log") {
                logs += 1;
            }
            if argv.iter().any(|arg| arg == "patch-id") {
                patch_ids += 1;
            }
        }
        assert_eq!(rev_lists, 2, "one commit listing for each side");
        assert_eq!(
            logs, 2,
            "one path listing, then one git log stream for the comparable patches"
        );
        assert_eq!(
            patch_ids, 1,
            "all candidate patches share one patch-id stream"
        );
    }

    #[test]
    fn paired_cherry_comparison_matches_one_sided_for_unrelated_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("left.txt"), "left root\n").unwrap();
        run(root, &["add", "left.txt"]);
        run(root, &["commit", "-qm", "left root"]);
        run(root, &["checkout", "--orphan", "unrelated"]);
        std::fs::write(root.join("right.txt"), "right root\n").unwrap();
        run(root, &["add", "right.txt"]);
        run(root, &["commit", "-qm", "unrelated root"]);

        let repo = GitRepo {
            root: root.to_path_buf(),
        };
        let paired = repo.cherry_marked_both("main", "unrelated").unwrap();
        assert_eq!(paired.left_only.len(), 1);
        assert!(
            !paired.left_only[0].1,
            "a parentless commit has no patch id"
        );
        assert_eq!(paired.right_only.len(), 1);
        assert!(
            !paired.right_only[0].1,
            "a parentless commit has no patch id"
        );
        assert_eq!(
            paired.left_only,
            repo.cherry_marked("main", "unrelated", CherrySide::Left)
                .unwrap()
        );
        assert_eq!(
            paired.right_only,
            repo.cherry_marked("main", "unrelated", CherrySide::Right)
                .unwrap()
        );
    }

    #[test]
    fn batched_shapes_and_patch_ids_match_the_per_commit_answers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").expect("write");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "root"]);
        let root_commit = run(root, &["rev-parse", "HEAD"]);
        // An ordinary edit.
        std::fs::write(root.join("a.txt"), "one\n2\nthree\n").expect("write");
        run(root, &["commit", "-qam", "edit"]);
        // A rename with an edit.
        run(root, &["mv", "a.txt", "b.txt"]);
        std::fs::write(root.join("b.txt"), "one\n2\nthree\nfour\n").expect("write");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "rename"]);
        // Pair binary add, modify, and delete patches with text hunks so the
        // patch-id has an independent textual contribution as well as the
        // binary pre/post object IDs.
        std::fs::write(root.join("b.txt"), "one\n2\nthree\nfour\nadd\n").expect("write");
        std::fs::write(root.join("blob.bin"), [0_u8, 159, 146, 150, 0, 7]).expect("write");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "mixed binary add"]);
        let mixed_binary_add = run(root, &["rev-parse", "HEAD"]);

        std::fs::write(root.join("b.txt"), "one\n2\nthree\nfour\nmodify\n").expect("write");
        std::fs::write(root.join("blob.bin"), [0_u8, 158, 147, 149, 0, 8]).expect("write");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "mixed binary modify"]);
        let mixed_binary_modify = run(root, &["rev-parse", "HEAD"]);

        std::fs::write(root.join("b.txt"), "one\n2\nthree\nfour\ndelete\n").expect("write");
        std::fs::remove_file(root.join("blob.bin")).expect("remove binary file");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "mixed binary delete"]);
        let mixed_binary_delete = run(root, &["rev-parse", "HEAD"]);

        // Keep a binary-only change too, but compare its result to Git's
        // legacy producer instead of assuming patch-id emits or omits it.
        std::fs::write(root.join("pure.bin"), [0_u8, 1, 0, 2, 255]).expect("write");
        run(root, &["add", "pure.bin"]);
        run(root, &["commit", "-qm", "pure binary"]);
        let pure_binary = run(root, &["rev-parse", "HEAD"]);

        // A mode change.
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.join("b.txt"), std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        run(root, &["commit", "-qam", "mode"]);
        // An empty commit has no patch id.
        run(root, &["commit", "-q", "--allow-empty", "-m", "empty"]);
        let empty_commit = run(root, &["rev-parse", "HEAD"]);
        // A merge, whose patch id is its diff against the first parent.
        run(root, &["switch", "-q", "-c", "side", "HEAD~2"]);
        std::fs::write(root.join("side.txt"), "side\n").expect("write");
        run(root, &["add", "side.txt"]);
        run(root, &["commit", "-qm", "side"]);
        run(root, &["switch", "-q", "main"]);
        run(
            root,
            &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
        );
        let merge = run(root, &["rev-parse", "HEAD"]);
        // A cherry-pick of the side commit is patch-identical to it.
        run(root, &["switch", "-q", "-c", "picked", "HEAD~1"]);
        run(root, &["cherry-pick", "side"]);
        run(root, &["switch", "-q", "main"]);

        let repo = GitRepo::discover(root).expect("repo");
        let mut commits = run(root, &["rev-list", "--reverse", "--all"])
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        commits.dedup();
        let shapes = repo.commit_shapes(&commits).expect("shapes");
        let with_parents = commits
            .iter()
            .filter(|commit| !shapes[*commit].parents.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        let patch_ids = repo
            .first_parent_patch_ids(&with_parents)
            .expect("patch ids");

        // Keep the independent binary-patch oracle under the same pinned diff
        // semantics; its only producer difference is `--binary` vs
        // `--full-index`.
        let mut legacy_binary_log_args = vec!["log", "--no-walk=unsorted", "--stdin", "-p"];
        legacy_binary_log_args.extend(patch_id_diff_args());
        legacy_binary_log_args.extend([
            "--binary",
            "--diff-merges=first-parent",
            "--format=commit %H",
        ]);
        let (legacy_patch_ids, legacy_log) =
            patch_ids_for_log_args(root, &with_parents, &legacy_binary_log_args);
        let production_log_args = first_parent_patch_id_log_args();
        let (full_index_patch_ids, full_index_log) =
            patch_ids_for_log_args(root, &with_parents, &production_log_args);
        assert_eq!(
            patch_ids, legacy_patch_ids,
            "optimized stream vs old --binary producer"
        );
        assert_eq!(
            patch_ids, full_index_patch_ids,
            "production args vs full-index stream"
        );
        let legacy_log = String::from_utf8_lossy(&legacy_log);
        let full_index_log = String::from_utf8_lossy(&full_index_log);
        assert!(
            legacy_log.contains("GIT binary patch"),
            "legacy binary control"
        );
        assert!(
            full_index_log.contains("Binary files"),
            "binary diff marker"
        );
        assert!(
            !full_index_log.contains("GIT binary patch"),
            "full-index output must not include encoded binary bodies"
        );
        for commit in [
            &mixed_binary_add,
            &mixed_binary_modify,
            &mixed_binary_delete,
        ] {
            assert!(
                patch_ids.contains_key(commit),
                "mixed text/binary patch {commit}"
            );
            assert_eq!(
                patch_ids.get(commit),
                legacy_patch_ids.get(commit),
                "mixed text/binary patch {commit}"
            );
        }
        assert_eq!(
            patch_ids.get(&pure_binary),
            legacy_patch_ids.get(&pure_binary),
            "pure binary patch parity must follow Git's result without assuming an ID"
        );
        let side = run(root, &["rev-parse", "side"]);
        assert_eq!(
            patch_ids.get(&merge),
            patch_ids.get(&side),
            "merge patch ID uses the first-parent diff"
        );

        for commit in &commits {
            let parents = repo.commit_parents(commit).expect("parents");
            assert_eq!(shapes[commit].parents, parents, "{commit}");
            assert_eq!(
                shapes[commit].tree,
                repo.commit_tree_id(commit).expect("tree"),
                "{commit}"
            );
            let expected = match parents.first() {
                Some(parent) => repo.patch_id_between(parent, commit).expect("patch id"),
                None => None,
            };
            assert_eq!(patch_ids.get(commit).cloned(), expected, "{commit}");
        }
        assert!(
            !patch_ids.contains_key(&root_commit),
            "root commit has no patch ID"
        );
        assert!(
            !patch_ids.contains_key(&empty_commit),
            "empty commit has no patch ID"
        );
        let picked = run(root, &["rev-parse", "picked"]);
        assert_ne!(side, picked);
        assert_eq!(
            patch_ids[&side], patch_ids[&picked],
            "a cherry-pick keeps its patch id"
        );

        let missing = vec!["0000000000000000000000000000000000000001".to_string()];
        assert!(repo.commit_shapes(&missing).is_err());
    }

    #[test]
    fn oversized_commit_batch_is_rejected_before_staging() {
        let commits = vec!["x".repeat(MAX_GIT_BATCH_INPUT_BYTES + 1)];
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(matches!(
            git_batch_input(&commits, deadline),
            Err(GitBatchInputError::TooLarge)
        ));
    }

    #[test]
    fn expired_commit_batch_staging_refuses_before_writing_input() {
        let commits = vec!["0123456789abcdef".to_string()];
        let deadline = Instant::now() - Duration::from_millis(1);
        assert!(matches!(
            git_batch_input(&commits, deadline),
            Err(GitBatchInputError::TimedOut)
        ));
    }

    #[test]
    fn streamed_batch_git_helpers_refuse_an_expired_inspection_before_spawning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("tracked.txt"), "fixture\n").expect("write");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "fixture"]);
        let repo = GitRepo::discover(root).expect("repo");
        let head = repo.head_commit().expect("head");
        let deadline = limit_git_until(Instant::now() - Duration::from_millis(1));
        let before = git_timeout_generation();

        let shapes = repo.commit_shapes(std::slice::from_ref(&head));
        assert!(
            matches!(shapes, Err(GitError::TimedOut { ref args, .. }) if args.starts_with("log --no-walk=unsorted --stdin")),
            "commit shape batch must respect the active report deadline: {shapes:?}"
        );
        let patch_ids = repo.first_parent_patch_ids(&[head]);
        assert!(
            matches!(patch_ids, Err(GitError::TimedOut { ref args, .. }) if args.contains("patch-id --verbatim")),
            "streamed patch-id batch must respect the active report deadline: {patch_ids:?}"
        );
        assert_eq!(git_timeout_generation(), before + 2);
        drop(deadline);
    }

    /// A port that differs only in whitespace is different work: re-indented
    /// Python or tabs turned into spaces change what the code does. Every
    /// patch comparison -- batched, per-commit and `cherry_marked` -- must
    /// tell it apart from the original, while an exact cherry-pick still
    /// matches.
    #[test]
    fn a_whitespace_only_difference_is_never_the_same_patch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("job.py"), "def job():\n    return 1\n").expect("write");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "root"]);
        let base = run(root, &["rev-parse", "HEAD"]);
        // Upstream: the change, indented with spaces.
        std::fs::write(
            root.join("job.py"),
            "def job():\n    if ready():\n        return 1\n",
        )
        .expect("write");
        run(root, &["commit", "-qam", "guard"]);
        // A session that made the same change with a tab: not the same work.
        run(root, &["switch", "-q", "-c", "tabbed", &base]);
        std::fs::write(
            root.join("job.py"),
            "def job():\n    if ready():\n\treturn 1\n",
        )
        .expect("write");
        run(root, &["commit", "-qam", "guard"]);
        // A session that cherry-picked the exact change: the same work.
        run(root, &["switch", "-q", "-c", "exact", &base]);
        // `-x` changes the message, so the pick is a distinct commit.
        run(root, &["cherry-pick", "-x", "main"]);

        let repo = GitRepo::discover(root).expect("repo");
        let upstream = run(root, &["rev-parse", "main"]);
        let tabbed = run(root, &["rev-parse", "tabbed"]);
        let exact = run(root, &["rev-parse", "exact"]);

        let batched = repo
            .first_parent_patch_ids(&[upstream.clone(), tabbed.clone(), exact.clone()])
            .expect("patch ids");
        assert_ne!(batched[&upstream], batched[&tabbed], "batched");
        assert_eq!(batched[&upstream], batched[&exact], "batched");
        let single = |commit: &str| {
            repo.patch_id_between(&base, commit)
                .expect("patch id")
                .expect("non-empty")
        };
        assert_ne!(single(&upstream), single(&tabbed), "per-commit");
        assert_eq!(single(&upstream), single(&exact), "per-commit");

        let marked = |branch: &str| {
            repo.cherry_marked("main", branch, CherrySide::Right)
                .expect("cherry marked")
        };
        assert_eq!(marked("tabbed"), vec![(tabbed, false)], "not landed");
        assert_eq!(marked("exact"), vec![(exact, true)], "landed");

        let tabbed_paired = repo
            .cherry_marked_both("main", "tabbed")
            .expect("paired whitespace comparison");
        assert_eq!(
            tabbed_paired.left_only,
            repo.cherry_marked("main", "tabbed", CherrySide::Left)
                .expect("left-side whitespace comparison")
        );
        assert_eq!(tabbed_paired.right_only, marked("tabbed"));
        let exact_paired = repo
            .cherry_marked_both("main", "exact")
            .expect("paired exact comparison");
        assert_eq!(
            exact_paired.left_only,
            repo.cherry_marked("main", "exact", CherrySide::Left)
                .expect("left-side exact comparison")
        );
        assert_eq!(exact_paired.right_only, marked("exact"));
    }

    #[cfg(unix)]
    #[test]
    fn patch_id_producers_ignore_textconv_and_external_diff_configuration() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitattributes"), "*.txt diff=normalize\n")
            .expect("write attributes");
        std::fs::write(root.join("sample.txt"), "base raw\n").expect("write base");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "base"]);
        let base = run(root, &["rev-parse", "HEAD"]);

        run(root, &["switch", "-q", "-c", "session"]);
        std::fs::write(root.join("sample.txt"), "session raw\n").expect("write session");
        run(root, &["commit", "-qam", "session"]);
        let session = run(root, &["rev-parse", "HEAD"]);

        run(root, &["switch", "-q", "-c", "candidate-match", &base]);
        std::fs::write(root.join("sample.txt"), "session raw\n").expect("write match");
        run(root, &["commit", "-qam", "matching candidate"]);
        let matching = run(root, &["rev-parse", "HEAD"]);

        run(root, &["switch", "-q", "-c", "candidate-different", &base]);
        std::fs::write(root.join("sample.txt"), "candidate raw\n").expect("write mismatch");
        run(root, &["commit", "-qam", "different candidate"]);
        let different = run(root, &["rev-parse", "HEAD"]);

        let textconv = root.join("normalize.sh");
        std::fs::write(
            &textconv,
            "#!/bin/sh\nif grep -q 'base raw' \"$1\"; then printf 'old normalized\\n'; else printf 'new normalized\\n'; fi\n",
        )
        .expect("write textconv");
        std::fs::set_permissions(&textconv, std::fs::Permissions::from_mode(0o755))
            .expect("chmod textconv");
        let external_diff = root.join("external-diff.sh");
        std::fs::write(
            &external_diff,
            "#!/bin/sh\nprintf '%s\\n' 'diff --git a/sample.txt b/sample.txt' '--- a/sample.txt' '+++ b/sample.txt' '@@ -1 +1 @@' '-old normalized' '+new normalized'\n",
        )
        .expect("write external diff");
        std::fs::set_permissions(&external_diff, std::fs::Permissions::from_mode(0o755))
            .expect("chmod external diff");
        run(
            root,
            &[
                "config",
                "diff.normalize.textconv",
                textconv.to_str().expect("utf-8 path"),
            ],
        );
        run(
            root,
            &[
                "config",
                "diff.external",
                external_diff.to_str().expect("utf-8 path"),
            ],
        );
        run(root, &["config", "diff.noprefix", "true"]);
        run(root, &["config", "color.ui", "always"]);

        let repo = GitRepo::discover(root).expect("repo");
        let session_patch = repo
            .patch_id_between(&base, &session)
            .expect("session patch")
            .expect("non-empty patch");
        let candidate_patches = repo
            .first_parent_patch_ids(&[matching.clone(), different.clone()])
            .expect("candidate patches");

        assert_eq!(candidate_patches.get(&matching), Some(&session_patch));
        assert_ne!(candidate_patches.get(&different), Some(&session_patch));
    }

    #[test]
    fn patch_ids_and_path_census_include_gitlink_changes_despite_ignore_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("note.txt"), "base note\n").expect("write base");
        run(root, &["add", "note.txt"]);
        run(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,1111111111111111111111111111111111111111,submodule",
            ],
        );
        run(root, &["commit", "-qm", "base"]);
        let base = run(root, &["rev-parse", "HEAD"]);

        run(root, &["switch", "-q", "-c", "session"]);
        std::fs::write(root.join("note.txt"), "shared note change\n").expect("write session");
        run(root, &["add", "note.txt"]);
        run(
            root,
            &[
                "update-index",
                "--cacheinfo",
                "160000,2222222222222222222222222222222222222222,submodule",
            ],
        );
        run(root, &["commit", "-qm", "session changes gitlink"]);
        let session = run(root, &["rev-parse", "HEAD"]);

        run(root, &["switch", "-q", "-c", "candidate", &base]);
        std::fs::write(root.join("note.txt"), "shared note change\n").expect("write candidate");
        run(root, &["commit", "-qam", "candidate changes text only"]);
        let candidate = run(root, &["rev-parse", "HEAD"]);
        run(root, &["config", "diff.ignoreSubmodules", "all"]);

        let repo = GitRepo::discover(root).expect("repo");
        let changed = repo
            .changed_between(&base, &session)
            .expect("changed paths");
        assert!(
            changed.iter().any(|path| path == "submodule"),
            "{changed:?}"
        );

        let session_patch = repo
            .patch_id_between(&base, &session)
            .expect("session patch")
            .expect("non-empty patch");
        let candidate_patch = repo
            .first_parent_patch_ids(std::slice::from_ref(&candidate))
            .expect("candidate patches");
        assert_ne!(candidate_patch.get(&candidate), Some(&session_patch));
        assert!(
            !repo
                .paths_equal(&session, &candidate, &changed)
                .expect("compare gitlink paths")
        );
    }

    #[cfg(unix)]
    #[test]
    fn paths_equal_is_literal_and_bounded_across_mode_only_changes() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("mode.sh"), "#!/bin/sh\nexit 0\n").expect("write mode file");
        std::fs::write(root.join(":(exclude)literal.txt"), "before\n").expect("write literal path");
        std::fs::write(root.join("stable.txt"), "stable\n").expect("write stable");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "base"]);
        let base = run(root, &["rev-parse", "HEAD"]);

        run(root, &["switch", "-q", "-c", "session"]);
        std::fs::set_permissions(root.join("mode.sh"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod mode file");
        std::fs::write(root.join(":(exclude)literal.txt"), "after\n").expect("modify literal path");
        run(root, &["commit", "-qam", "mode and literal path change"]);
        let head = run(root, &["rev-parse", "HEAD"]);

        let repo = GitRepo::discover(root).expect("repo");
        let mut paths = vec!["stable.txt".to_string(); MAX_LITERAL_PATHSPEC_BATCH_COUNT + 1];
        paths.push("mode.sh".to_string());
        assert!(
            !repo
                .paths_equal(&base, &head, &paths)
                .expect("mode compare")
        );
        assert!(
            !repo
                .paths_equal(&base, &head, &[":(exclude)literal.txt".to_string()])
                .expect("literal path compare")
        );
    }

    #[test]
    fn changed_between_preserves_rename_projection_and_full_census_reports_both_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("before.txt"), "same content\n").expect("write before");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "base"]);
        let base = run(root, &["rev-parse", "HEAD"]);

        std::fs::rename(root.join("before.txt"), root.join("after.txt")).expect("rename");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "rename"]);
        run(root, &["config", "diff.renames", "true"]);

        let head = run(root, &["rev-parse", "HEAD"]);
        let repo = GitRepo::discover(root).expect("repo");
        let changed = repo.changed_between(&base, &head).expect("changed paths");
        assert_eq!(changed, vec!["after.txt"]);

        let complete = repo
            .changed_between_all_paths(&base, &head)
            .expect("complete changed-path census");
        assert_eq!(complete.len(), 2, "{complete:?}");
        assert!(
            complete.iter().any(|path| path == "before.txt"),
            "{complete:?}"
        );
        assert!(
            complete.iter().any(|path| path == "after.txt"),
            "{complete:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn strict_path_evidence_preserves_newline_names_from_real_git() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        let path = "tracked\nname.txt";
        std::fs::write(root.join(path), "before\n").expect("write base");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "base"]);
        let base = run(root, &["rev-parse", "HEAD"]);

        run(root, &["switch", "-q", "-c", "session"]);
        std::fs::write(root.join(path), "after\n").expect("modify newline path");
        run(root, &["commit", "-qam", "change newline path"]);
        let head = run(root, &["rev-parse", "HEAD"]);

        let repo = GitRepo::discover(root).expect("repo");
        let changed = repo.changed_between(&base, &head).expect("changed paths");
        assert_eq!(changed, vec![path]);
        assert!(
            !repo
                .paths_equal(&base, &head, &changed)
                .expect("path comparison")
        );
    }

    #[cfg(unix)]
    #[test]
    fn strict_path_evidence_refuses_non_utf8_names_from_real_git() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("base.txt"), "base\n").expect("write base");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "base"]);
        let base = run(root, &["rev-parse", "HEAD"]);

        run(root, &["switch", "-q", "-c", "session"]);
        let Some(_path) = aethyme_testkit::filesystem::write_non_utf8_file(
            root,
            b"invalid-\xff-name.txt",
            b"tracked\n",
        )
        .expect("write invalid UTF-8 path") else {
            return;
        };
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "add non-UTF-8 path"]);
        let head = run(root, &["rev-parse", "HEAD"]);

        let repo = GitRepo::discover(root).expect("repo");
        assert!(matches!(
            repo.changed_between(&base, &head),
            Err(GitError::UntrustedOutput { .. })
        ));
    }
}

#[cfg(test)]
mod graph_snapshot_tests {
    use super::*;

    #[test]
    fn graph_environment_preflight_refuses_routing_history_but_allows_scoped_settings() {
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_OBJECT_DIRECTORY_RELATIVE",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
            "GIT_REPLACE_REF_BASE",
            "GIT_GRAFT_FILE",
            "GIT_SHALLOW_FILE",
            "GIT_CEILING_DIRECTORIES",
            "GIT_DISCOVERY_ACROSS_FILESYSTEM",
            "GIT_PREFIX",
        ] {
            assert!(
                unsafe_graph_git_environment_key(std::ffi::OsStr::new(key)),
                "{key}"
            );
        }
        for key in [
            "GIT_CONFIG",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_KEY_17",
            "GIT_CONFIG_VALUE_17",
            "GIT_EXEC_PATH",
            "GIT_TEMPLATE_DIR",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_ATTR_NOSYSTEM",
            "GIT_TRACE2_EVENT",
            "GIT_TRACE_SETUP",
            "GIT_CURL_VERBOSE",
            "GIT_PAGER",
            "GIT_EXTERNAL_DIFF",
            "GIT_NO_REPLACE_OBJECTS",
            "HOME",
            "NOT_GIT_DIR",
            "PATH",
        ] {
            assert!(
                !unsafe_graph_git_environment_key(std::ffi::OsStr::new(key)),
                "{key}"
            );
        }
        assert!(is_git_environment_key(std::ffi::OsStr::new(
            "GIT_CONFIG_COUNT"
        )));
        assert!(!is_git_environment_key(std::ffi::OsStr::new("PATH")));
    }

    #[test]
    fn session_commit_environment_preflight_refuses_routing_and_config_overrides() {
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
            "GIT_PREFIX",
            "GIT_CONFIG",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_EXEC_PATH",
        ] {
            assert!(
                unsafe_session_commit_git_environment_key(std::ffi::OsStr::new(key)),
                "{key}"
            );
        }
        for key in [
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "GIT_EDITOR",
            "GIT_PAGER",
            "GIT_TRACE2_EVENT",
            "HOME",
            "PATH",
        ] {
            assert!(
                !unsafe_session_commit_git_environment_key(std::ffi::OsStr::new(key)),
                "{key}"
            );
        }
    }

    #[test]
    fn graph_inventory_counts_directories_and_unusual_paths() {
        let oid = "a".repeat(40);
        let input = format!(
            "040000 tree {oid}       -\tnested\0\
            100644 blob {oid}       1\tnested/a\tnewline\n.txt\0\
            120000 blob {oid}       7\tlink\0"
        );
        assert_eq!(
            parse_graph_snapshot_inventory(input.as_bytes()).unwrap(),
            (3 * 4096, 4)
        );
        assert_eq!(parse_graph_snapshot_inventory(b"").unwrap(), (0, 1));
        let sha256_oid = "b".repeat(64);
        assert_eq!(graph_snapshot_object_format(&oid).unwrap(), "sha1");
        assert_eq!(graph_snapshot_object_format(&sha256_oid).unwrap(), "sha256");
        let sha256_row = format!("100644 blob {sha256_oid} 1\tfile\0");
        assert_eq!(
            parse_graph_snapshot_inventory(sha256_row.as_bytes()).unwrap(),
            (4096, 2)
        );
        assert!(graph_snapshot_object_format("HEAD").is_err());
        assert!(graph_snapshot_object_format(&"a".repeat(41)).is_err());
    }

    #[test]
    fn graph_inventory_refuses_unknown_incomplete_and_oversized_inputs() {
        let oid = "a".repeat(40);
        for input in [
            format!("100644 blob {oid} 1\tfile"),
            format!("100644 blob {oid} -\tfile\0"),
            format!("160000 commit {oid} -\tsubmodule\0"),
            format!("100644 blob {oid} 2147483649\tfile\0"),
            format!("100644 blob {oid} 18446744073709551615\tfile\0"),
            format!("100644 blob {oid} 1\t\0"),
        ] {
            assert!(
                parse_graph_snapshot_inventory(input.as_bytes()).is_err(),
                "{input:?}"
            );
        }
        let row = format!("100644 blob {oid} 0\tfile\0");
        assert!(parse_graph_snapshot_inventory(row.repeat(100_000).as_bytes()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn isolated_graph_checkout_scrubs_foreign_metadata_and_filters() {
        const CHILD: &str = "AETHYME_GRAPH_SNAPSHOT_TEST_CHILD";
        const ROOT: &str = "AETHYME_GRAPH_SNAPSHOT_TEST_ROOT";
        const TEST: &str = "git::graph_snapshot_tests::isolated_graph_checkout_scrubs_foreign_metadata_and_filters";
        if let Some(mode) = std::env::var_os(CHILD) {
            let mode = mode.to_string_lossy();
            let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
            let source = GitRepo {
                root: root.join("source"),
            };
            let commit = std::env::var("AETHYME_GRAPH_SNAPSHOT_COMMIT").unwrap();
            let disabled_commit = std::env::var("AETHYME_GRAPH_SNAPSHOT_DISABLED_COMMIT").unwrap();
            let global_filter_hit = root.join("global-filter-invoked");
            let counted_filter_hit = root.join("counted-filter-invoked");
            let trace_file = root.join("trace2.json");
            if mode == "filter" {
                // Benign configuration, clean-filter, pager, and trace
                // variables are accepted at the graph boundary. Git commands
                // inside that boundary must still ignore all of them.
                assert!(!graph_verification_git_enabled());
                assert_eq!(
                    crate::graph_integrity::load_graph_policy_at_commit(&source, &disabled_commit)
                        .unwrap(),
                    aethyme_graph_storage::GraphIntegrityPolicy::default()
                );
                assert_eq!(
                    crate::graph_integrity::verify_disposable_checkout(
                        &source,
                        &aethyme_graph_storage::GraphIntegrityPolicy::default()
                    )
                    .status,
                    crate::graph_integrity::GraphIntegrityStatus::Disabled,
                    "safe Git configuration must not refuse the disabled fast path"
                );
                assert!(
                    !graph_verification_git_enabled(),
                    "graph-policy read leaked its scoped Git guard"
                );
                assert!(
                    graph_verification_environment_refusal().is_none(),
                    "safe Git configuration was treated as repository redirection"
                );
                assert!(
                    !global_filter_hit.exists(),
                    "graph-policy read or disabled fast path ran the HOME clean filter"
                );
                assert!(
                    !counted_filter_hit.exists(),
                    "graph-policy read or disabled fast path ran the GIT_CONFIG_COUNT clean filter"
                );
                assert!(
                    !trace_file.exists(),
                    "initial Git program resolution honored inherited GIT_TRACE2_EVENT"
                );

                // The ordinary path sees both HOME's global clean filter and
                // the GIT_CONFIG_COUNT filter, and emits trace output. The
                // graph scope suppresses them for the exact snapshot, then
                // restores ordinary behavior.
                source.working_tree_hash().unwrap();
                assert!(
                    global_filter_hit.is_file(),
                    "global clean filter did not execute"
                );
                assert!(
                    counted_filter_hit.is_file(),
                    "GIT_CONFIG_COUNT filter did not execute"
                );
                assert!(trace_file.is_file(), "GIT_TRACE2_EVENT did not execute");
                std::fs::remove_file(&global_filter_hit).unwrap();
                std::fs::remove_file(&counted_filter_hit).unwrap();
                std::fs::remove_file(&trace_file).unwrap();
                let expected_tree = source.commit_tree_id(&commit).unwrap();
                if trace_file.exists() {
                    std::fs::remove_file(&trace_file).unwrap();
                }
                {
                    let _graph_environment = graph_verification_git_guard();
                    let destination = root.join("isolated");
                    let isolated = source.isolated_checkout_at(&destination, &commit).unwrap();
                    assert_eq!(isolated.root(), destination.canonicalize().unwrap());
                    assert_eq!(isolated.working_tree_hash().unwrap(), expected_tree);
                    std::fs::write(destination.join("payload.txt"), "isolated dirty bytes\n")
                        .unwrap();
                    isolated.working_tree_hash().unwrap();
                    assert!(
                        !global_filter_hit.exists() && !counted_filter_hit.exists(),
                        "a clean filter executed inside graph scope"
                    );
                    assert!(
                        !trace_file.exists(),
                        "Git trace was written inside graph scope"
                    );
                }
                std::fs::write(source.root.join("payload.txt"), "after graph scope\n").unwrap();
                std::fs::write(source.root.join("counted.bin"), "after graph scope\n").unwrap();
                source.working_tree_hash().unwrap();
                assert!(
                    global_filter_hit.is_file() && counted_filter_hit.is_file(),
                    "ordinary Git filter configuration was not restored"
                );
                assert!(
                    trace_file.is_file(),
                    "ordinary Git tracing was not restored"
                );
                return;
            }
            assert_eq!(mode, "hostile");
            let foreign = root.join("foreign/.git");
            let source_before = metadata_bytes(&source.root.join(".git"));
            let foreign_before = metadata_bytes(&foreign);
            assert!(
                graph_verification_environment_refusal().is_some(),
                "an unguarded graph entrypoint must refuse inherited repository selectors"
            );
            {
                let _guard = graph_verification_git_guard();
                assert!(graph_verification_git_enabled());
                assert!(
                    graph_verification_environment_refusal().is_none(),
                    "the active isolating guard must permit its own Git commands"
                );
                let other_thread_refusal =
                    std::thread::spawn(graph_verification_environment_refusal)
                        .join()
                        .unwrap();
                assert!(
                    other_thread_refusal.is_some(),
                    "the isolating guard must not change refusal in another thread"
                );
            }
            assert!(
                !graph_verification_git_enabled(),
                "dropping the isolating guard must restore its prior scope"
            );
            assert!(
                graph_verification_environment_refusal().is_some(),
                "dropping the isolating guard must restore refusal of inherited selectors"
            );
            // This source commit has no graph policy, and would otherwise
            // resolve to the disabled default. An inherited redirect must be
            // refused before reading that authoritative config from another
            // repository.
            let policy_error =
                crate::graph_integrity::load_graph_policy_at_commit(&source, &commit)
                    .expect_err("inherited Git redirection must block policy reads");
            let policy_error = policy_error.to_string();
            assert!(
                policy_error.contains("refuses inherited Git routing/history overrides"),
                "unexpected policy refusal: {policy_error}"
            );
            assert!(
                !policy_error.contains(&foreign.to_string_lossy().to_string()),
                "policy refusal exposed an inherited Git environment value"
            );
            assert!(
                !graph_verification_git_enabled(),
                "refused graph-policy read changed the scoped Git guard"
            );
            let policy = aethyme_graph_storage::GraphIntegrityPolicy::parse(
                "[graph]\nauthority = 'committed_fragments'\nrepository = 'fixture'\n",
            )
            .unwrap();
            let root_plan = crate::broker::lifecycle::plan_worktree_root_for(
                &source.root,
                Some(&source.root.join(".git")),
                Some(&root.join("worktrees")),
            );
            let outcomes = [
                crate::graph_integrity::verify_disposable_checkout(&source, &policy),
                crate::graph_integrity::verify_checkout_without_mutation(
                    &source.root,
                    &source,
                    &policy,
                )
                .unwrap(),
                crate::graph_integrity::verify_checkout_without_mutation_with_worktree_root_plan(
                    &source.root,
                    &source,
                    &policy,
                    &root_plan,
                )
                .unwrap(),
                crate::graph_integrity::verify_commit_for_advice_with_worktree_root_plan(
                    &source.root,
                    &source,
                    &commit,
                    &root_plan,
                )
                .unwrap(),
            ];
            for outcome in outcomes {
                assert_eq!(
                    outcome.status,
                    crate::graph_integrity::GraphIntegrityStatus::Error
                );
                assert!(
                    outcome.tree_hash.is_empty(),
                    "Git work ran before context refusal: {outcome:?}"
                );
                assert!(
                    outcome
                        .reason
                        .contains("refuses inherited Git routing/history overrides")
                );
            }
            assert_eq!(metadata_bytes(&foreign), foreign_before);
            assert_eq!(metadata_bytes(&source.root.join(".git")), source_before);
            assert!(!root.join("worktrees").exists());
            return;
        }
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        for name in ["source", "foreign"] {
            let path = root.join(name);
            std::fs::create_dir(&path).unwrap();
            run_isolated_graph_git(&path, &["init", "--quiet", "--template=", "-b", "main"])
                .unwrap();
            let content = if name == "source" {
                "original content\n"
            } else {
                "foreign content\n"
            };
            std::fs::write(path.join("payload.txt"), content).unwrap();
            if name == "source" {
                std::fs::write(
                    path.join(".gitattributes"),
                    "*.txt filter=aethyme_snapshot_global\n*.bin filter=aethyme_snapshot_count\n",
                )
                .unwrap();
                std::fs::write(path.join("counted.bin"), "counted original content\n").unwrap();
            }
            run_isolated_graph_git(&path, &["add", "--all"]).unwrap();
            run_isolated_graph_git(
                &path,
                &[
                    "-c",
                    "user.name=Snapshot Test",
                    "-c",
                    "user.email=snapshot@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "fixture",
                ],
            )
            .unwrap();
        }
        let disabled_commit = String::from_utf8(
            run_isolated_graph_git(&root.join("source"), &["rev-parse", "HEAD"]).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("source/.aethyme")).unwrap();
        std::fs::write(
            root.join("source/.aethyme/config.toml"),
            "[graph]\nauthority = 'committed_fragments'\nrepository = 'fixture'\n",
        )
        .unwrap();
        run_isolated_graph_git(&root.join("source"), &["add", "--all"]).unwrap();
        run_isolated_graph_git(
            &root.join("source"),
            &[
                "-c",
                "user.name=Snapshot Test",
                "-c",
                "user.email=snapshot@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "enable graph policy",
            ],
        )
        .unwrap();
        let commit = String::from_utf8(
            run_isolated_graph_git(&root.join("source"), &["rev-parse", "HEAD"]).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("source/payload.txt"), "working tree content\n").unwrap();
        std::fs::write(root.join("source/counted.bin"), "working tree content\n").unwrap();
        let config = root.join(".gitconfig");
        run_isolated_graph_git(
            &root,
            &[
                "config",
                "--file",
                config.to_str().unwrap(),
                "filter.aethyme_snapshot_global.clean",
                "touch \"$AETHYME_GRAPH_FILTER_HIT\"; cat",
            ],
        )
        .unwrap();

        let run_child = |mode: &str, hostile_git_env: bool| {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", TEST, "--nocapture"])
                .env(CHILD, mode)
                .env(ROOT, &root)
                .env("HOME", &root)
                .env("AETHYME_GRAPH_SNAPSHOT_COMMIT", commit.trim())
                .env(
                    "AETHYME_GRAPH_SNAPSHOT_DISABLED_COMMIT",
                    disabled_commit.trim(),
                )
                .env(
                    "AETHYME_GRAPH_FILTER_HIT",
                    root.join("global-filter-invoked"),
                )
                .env(
                    "AETHYME_GRAPH_COUNT_FILTER_HIT",
                    root.join("counted-filter-invoked"),
                );
            for (key, _) in std::env::vars_os() {
                if is_git_environment_key(&key) {
                    command.env_remove(key);
                }
            }
            if !hostile_git_env {
                command
                    .env("GIT_CONFIG_GLOBAL", &config)
                    .env("GIT_CONFIG_COUNT", "1")
                    .env("GIT_CONFIG_KEY_0", "filter.aethyme_snapshot_count.clean")
                    .env(
                        "GIT_CONFIG_VALUE_0",
                        "touch \"$AETHYME_GRAPH_COUNT_FILTER_HIT\"; cat",
                    )
                    .env("GIT_TRACE2_EVENT", root.join("trace2.json"))
                    .env("GIT_PAGER", "cat");
            }
            if hostile_git_env {
                command
                    .env("GIT_DIR", root.join("foreign/.git"))
                    .env("GIT_COMMON_DIR", root.join("foreign/.git"))
                    .env("GIT_WORK_TREE", root.join("foreign"))
                    .env("GIT_INDEX_FILE", root.join("foreign/.git/index"))
                    .env("GIT_OBJECT_DIRECTORY", root.join("foreign/.git/objects"))
                    .env(
                        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                        root.join("foreign/.git/objects"),
                    )
                    .env("GIT_SHALLOW_FILE", root.join("foreign/.git/shallow"))
                    .env("GIT_GRAFT_FILE", root.join("foreign/.git/info/grafts"))
                    .env("GIT_CONFIG_COUNT", "1")
                    .env("GIT_CONFIG_KEY_0", "filter.aethyme_snapshot_global.clean")
                    .env(
                        "GIT_CONFIG_VALUE_0",
                        "touch \"$AETHYME_GRAPH_FILTER_HIT\"; cat",
                    );
            }
            match crate::bounded_output::output_limited(
                &mut command,
                Duration::from_secs(30),
                1024 * 1024,
            )
            .unwrap()
            {
                crate::bounded_output::LimitedOutput::Completed(output) => assert!(
                    output.status.success(),
                    "stdout: {}\nstderr: {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
                other => panic!("isolated graph child did not complete: {other:?}"),
            }
        };
        // Each child starts with an unused Git-program OnceLock, so the
        // injected trace variable reaches the candidate probe itself before
        // graph-command isolation is configured.
        run_child("filter", false);
        run_child("hostile", true);
    }

    #[cfg(unix)]
    fn metadata_bytes(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(root: &Path, path: &Path, rows: &mut Vec<(PathBuf, Vec<u8>)>) {
            let metadata = std::fs::symlink_metadata(path).unwrap();
            if metadata.is_dir() {
                for entry in std::fs::read_dir(path).unwrap() {
                    walk(root, &entry.unwrap().path(), rows);
                }
            } else {
                assert!(metadata.is_file() && metadata.len() <= 1024 * 1024);
                assert!(rows.len() < 500);
                rows.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                ));
            }
        }
        let mut rows = Vec::new();
        walk(root, root, &mut rows);
        rows.sort();
        rows
    }
}

#[cfg(test)]
mod object_batch_bound_tests {
    use super::*;

    #[test]
    fn object_query_byte_limit_includes_path_and_delimiters_before_staging() {
        let queries = [("rev", "path")];
        let deadline = Instant::now() + Duration::from_secs(5);

        assert_eq!(
            checked_git_batch_check_input_len(&queries, 9, deadline).unwrap(),
            9
        );
        assert!(matches!(
            checked_git_batch_check_input_len(&queries, 8, deadline),
            Err(GitBatchInputError::TooLarge)
        ));
    }

    #[test]
    fn object_query_count_is_refused_before_repo_access() {
        let root = tempfile::tempdir().unwrap();
        let repo = GitRepo {
            root: root.path().to_path_buf(),
        };
        let queries = vec![("HEAD", "file.txt"); MAX_GIT_BATCH_QUERY_COUNT + 1];

        let error = repo.objects_at_many(&queries).unwrap_err();

        assert!(matches!(
            error,
            GitError::Git { ref stderr, .. }
                if stderr.contains("exceeded 100000 queries")
        ));
    }

    #[test]
    fn oversized_newline_object_path_is_refused_before_git() {
        let root = tempfile::tempdir().unwrap();
        let repo = GitRepo {
            root: root.path().to_path_buf(),
        };
        let path = format!("line\nbreak{}", "x".repeat(MAX_GIT_BATCH_INPUT_BYTES));
        let queries = [("HEAD", path.as_str())];

        let error = repo.objects_at_many(&queries).unwrap_err();

        assert!(matches!(
            error,
            GitError::Git { ref stderr, .. }
                if stderr.contains("input exceeded 16777216 bytes")
        ));
    }

    #[test]
    fn object_query_observes_an_expired_deadline_as_unknown() {
        let root = tempfile::tempdir().unwrap();
        let repo = GitRepo {
            root: root.path().to_path_buf(),
        };
        let deadline = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let _git_deadline = limit_git_until(deadline);

        let error = repo.objects_at_many(&[("HEAD", "file.txt")]).unwrap_err();

        assert!(matches!(
            error,
            GitError::TimedOut { ref args, .. } if args.contains("cat-file")
        ));
    }
}

#[cfg(test)]
mod seeded_tree_hash_tests {
    use super::{GitRepo, SeedFallback};
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    fn git(root: &Path, args: &[&str]) {
        super::run_isolated_graph_git(root, args)
            .unwrap_or_else(|error| panic!("git {}: {error}", args.join(" ")));
    }

    /// A repository and a slot checked out from its HEAD, as submit does.
    struct Slot {
        _holder: tempfile::TempDir,
        repo: GitRepo,
        slot: GitRepo,
    }

    impl Slot {
        fn new(attributes: Option<&str>) -> Self {
            let holder = tempfile::tempdir().unwrap();
            let root = holder.path().join("repository");
            std::fs::create_dir_all(root.join("dir")).unwrap();
            git(&root, &["init", "-q", "-b", "main"]);
            git(&root, &["config", "user.name", "Seeded hash test"]);
            git(&root, &["config", "user.email", "seeded@example.invalid"]);
            std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
            std::fs::write(root.join("dir/b.txt"), "bravo\n").unwrap();
            std::fs::write(root.join("crlf.txt"), "one\r\ntwo\r\n").unwrap();
            if let Some(attributes) = attributes {
                std::fs::write(root.join(".gitattributes"), attributes).unwrap();
            }
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "-qm", "initial"]);
            let repo = GitRepo::discover(&root).unwrap();
            let head = repo.head_commit().unwrap();
            let slot = repo
                .worktree_add_detached(&holder.path().join("slot"), &head)
                .unwrap();
            Self {
                _holder: holder,
                repo,
                slot,
            }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.slot.root().join(relative)
        }

        fn seeded(&self) -> Result<String, SeedFallback> {
            let mut quiescent = true;
            self.slot.seeded_working_tree_hash(&mut quiescent)
        }

        fn full(&self) -> String {
            self.slot.working_tree_hash().unwrap()
        }

        /// The seeded shortcut answered, and with the full hash's tree.
        fn assert_seeded_matches(&self, case: &str) -> String {
            let full = self.full();
            assert_eq!(self.seeded(), Ok(full.clone()), "{case}");
            let public = self
                .slot
                .working_tree_hash_observation_for_fresh_checkout()
                .result
                .unwrap();
            assert_eq!(public, full, "{case}");
            full
        }

        /// The shortcut declined for `reason`, and the public call still
        /// returns the full hash.
        fn assert_falls_back(&self, reason: SeedFallback) {
            assert_eq!(self.seeded(), Err(reason));
            let public = self
                .slot
                .working_tree_hash_observation_for_fresh_checkout()
                .result
                .unwrap();
            assert_eq!(public, self.full());
        }

        fn index_modified(&self) -> SystemTime {
            let index =
                super::run_git(self.slot.root(), &["rev-parse", "--git-path", "index"]).unwrap();
            let index = PathBuf::from(index.trim());
            let index = if index.is_absolute() {
                index
            } else {
                self.slot.root().join(index)
            };
            std::fs::metadata(index).unwrap().modified().unwrap()
        }
    }

    fn set_modified(path: &Path, time: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn a_fresh_slot_hashes_to_the_commit_tree() {
        let slot = Slot::new(None);
        let tree = slot.assert_seeded_matches("fresh slot");
        let head = slot.repo.head_commit().unwrap();
        assert_eq!(tree, slot.repo.commit_tree_id(&head).unwrap());
    }

    #[test]
    fn a_tracked_file_modified_after_checkout_is_hashed() {
        let slot = Slot::new(None);
        let fresh = slot.full();
        std::fs::write(slot.path("a.txt"), "alpha, longer now\n").unwrap();
        let changed = slot.assert_seeded_matches("modified file");
        assert_ne!(changed, fresh);
    }

    #[test]
    fn an_untracked_file_added_after_checkout_is_hashed() {
        let slot = Slot::new(None);
        let fresh = slot.full();
        std::fs::write(slot.path("dir/new.txt"), "new\n").unwrap();
        assert_ne!(slot.assert_seeded_matches("untracked file"), fresh);
    }

    #[test]
    fn a_deleted_tracked_file_is_hashed() {
        let slot = Slot::new(None);
        let fresh = slot.full();
        std::fs::remove_file(slot.path("dir/b.txt")).unwrap();
        assert_ne!(slot.assert_seeded_matches("deleted file"), fresh);
    }

    #[test]
    fn a_same_size_change_in_the_same_second_is_hashed() {
        let slot = Slot::new(None);
        let fresh = slot.full();
        let original = std::fs::metadata(slot.path("a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        // Same size, and the modification time put back: only the inode
        // change time and the racy-clean rule can tell.
        std::fs::write(slot.path("a.txt"), "ALPHA\n").unwrap();
        set_modified(&slot.path("a.txt"), original);
        assert_ne!(slot.assert_seeded_matches("same size, same mtime"), fresh);

        // Same size, with the modification time equal to the index's: a
        // racily clean entry, which Git must re-read.
        std::fs::write(slot.path("dir/b.txt"), "BRAVO\n").unwrap();
        set_modified(&slot.path("dir/b.txt"), slot.index_modified());
        slot.assert_seeded_matches("same size, mtime of the index");
    }

    #[test]
    fn the_index_copy_keeps_the_original_timestamp() {
        let holder = tempfile::tempdir().unwrap();
        let original = holder.path().join("index");
        let copy = holder.path().join("copy");
        std::fs::write(&original, b"DIRC index bytes").unwrap();
        // An old timestamp with a sub-second part, unlike the copy's own.
        let stamp = SystemTime::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 123_456_789);
        set_modified(&original, stamp);
        let original_time = std::fs::metadata(&original).unwrap().modified().unwrap();
        super::copy_index_with_its_timestamp(&original, &copy).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"DIRC index bytes");
        let copy_time = std::fs::metadata(&copy).unwrap().modified().unwrap();
        // Equal to the precision the file system keeps, and never later.
        assert!(
            copy_time <= original_time,
            "{copy_time:?} > {original_time:?}"
        );
        assert_eq!(copy_time, original_time);
    }

    #[test]
    fn an_edit_in_the_index_write_second_is_hashed() {
        // On a file system with sub-second timestamps the changed inode time
        // also reveals this edit, so the timestamp guard is pinned by the
        // copy test above; this one checks the end-to-end outcome.
        let slot = Slot::new(None);
        let fresh = slot.full();
        let path = slot.path("a.txt");
        let original = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "ALPHA\n").unwrap();
        set_modified(&path, original);
        let written = slot.index_modified();
        while SystemTime::now()
            .duration_since(written)
            .is_ok_and(|elapsed| elapsed < std::time::Duration::from_millis(1100))
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_ne!(
            slot.assert_seeded_matches("edit in the index-write second"),
            fresh
        );
    }

    #[test]
    fn staged_content_falls_back_to_the_full_hash() {
        let slot = Slot::new(None);
        std::fs::write(slot.path("a.txt"), "staged\n").unwrap();
        git(slot.slot.root(), &["add", "a.txt"]);
        std::fs::write(slot.path("a.txt"), "alpha\n").unwrap();
        slot.assert_falls_back(SeedFallback::IndexNotHead);
    }

    #[test]
    fn an_assume_unchanged_entry_falls_back_to_the_full_hash() {
        let slot = Slot::new(None);
        git(
            slot.slot.root(),
            &["update-index", "--assume-unchanged", "a.txt"],
        );
        std::fs::write(slot.path("a.txt"), "hidden from status\n").unwrap();
        slot.assert_falls_back(SeedFallback::EntryFlags);
    }

    #[test]
    fn content_converting_attributes_fall_back_to_the_full_hash() {
        // The committed CRLF blob re-hashes to an LF blob through `text`.
        let slot = Slot::new(Some("*.txt text=auto\n"));
        slot.assert_falls_back(SeedFallback::ContentConversion);
        let slot = Slot::new(Some("*.txt filter=anything\n"));
        slot.assert_falls_back(SeedFallback::ContentConversion);
    }

    #[test]
    fn attributes_that_convert_nothing_keep_the_shortcut() {
        let slot = Slot::new(Some("*.json merge=union\n*.png binary\n*.txt -text\n"));
        slot.assert_seeded_matches("merge and unset attributes only");
    }
}

#[cfg(test)]
mod working_tree_hash_runtime_path_tests {
    use super::GitRepo;
    use std::path::Path;

    fn git(root: &Path, args: &[&str]) {
        super::run_isolated_graph_git(root, args)
            .unwrap_or_else(|error| panic!("git {}: {error}", args.join(" ")));
    }

    fn repository() -> (tempfile::TempDir, GitRepo) {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(
            root.path(),
            &["config", "user.name", "Working tree hash test"],
        );
        git(
            root.path(),
            &["config", "user.email", "working-tree-hash@example.invalid"],
        );
        std::fs::write(root.path().join("tracked.txt"), "baseline\n").unwrap();
        git(root.path(), &["add", "tracked.txt"]);
        git(root.path(), &["commit", "-qm", "initial"]);
        let repo = GitRepo::discover(root.path()).unwrap();
        (root, repo)
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn working_tree_hash_omits_untracked_runtime_but_keeps_other_untracked_files() {
        let (root, repo) = repository();
        let baseline = repo.working_tree_hash().unwrap();

        // A directory rule does not classify a same-named standalone file.
        write(root.path(), ".aethyme/run", "ordinary file\n");
        assert_ne!(repo.working_tree_hash().unwrap(), baseline);
        std::fs::remove_file(root.path().join(".aethyme/run")).unwrap();
        assert_eq!(repo.working_tree_hash().unwrap(), baseline);

        write(root.path(), ".aethyme/run/worker.json", "marker\n");
        write(root.path(), ".aethyme/logs/worker.log", "log\n");
        write(root.path(), ".aethyme/gc.lock", "lock\n");
        write(root.path(), ".aethyme/broker.db-wal", "wal\n");
        write(
            root.path(),
            ".aethyme/broker-action-required.md/child",
            "runtime child\n",
        );
        assert_eq!(repo.working_tree_hash().unwrap(), baseline);

        #[cfg(unix)]
        let pathspec_name = ":(exclude)literal [glob]*.txt";
        #[cfg(not(unix))]
        let pathspec_name = "new-source [literal].txt";
        write(root.path(), pathspec_name, "ordinary untracked content\n");
        assert_ne!(repo.working_tree_hash().unwrap(), baseline);
    }

    #[test]
    fn working_tree_hash_handles_an_ignored_runtime_parent() {
        let (root, repo) = repository();
        write(root.path(), ".gitignore", ".aethyme/\n");
        git(root.path(), &["add", ".gitignore"]);
        git(root.path(), &["commit", "-qm", "ignore runtime root"]);

        let baseline = repo.working_tree_hash().unwrap();
        write(root.path(), ".aethyme/broker.db", "runtime database\n");
        write(root.path(), ".aethyme/run/worker.json", "runtime marker\n");
        write(root.path(), ".aethyme/logs/worker.log", "runtime log\n");

        assert_eq!(repo.working_tree_hash().unwrap(), baseline);
    }

    #[test]
    fn working_tree_hash_keeps_tracked_runtime_edits_in_the_key() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(
            root.path(),
            &["config", "user.name", "Working tree hash test"],
        );
        git(
            root.path(),
            &["config", "user.email", "working-tree-hash@example.invalid"],
        );
        write(root.path(), ".gitignore", ".aethyme/\n");
        git(root.path(), &["add", ".gitignore"]);
        write(root.path(), ".aethyme/run/tracked-policy.json", "before\n");
        git(
            root.path(),
            &["add", "-f", ".aethyme/run/tracked-policy.json"],
        );
        git(root.path(), &["commit", "-qm", "track runtime policy"]);

        let repo = GitRepo::discover(root.path()).unwrap();
        let baseline = repo.working_tree_hash().unwrap();
        let tracked_policy = root.path().join(".aethyme/run/tracked-policy.json");
        write(root.path(), ".aethyme/run/tracked-policy.json", "after\n");
        assert_ne!(repo.working_tree_hash().unwrap(), baseline);
        std::fs::remove_file(tracked_policy).unwrap();
        assert_ne!(repo.working_tree_hash().unwrap(), baseline);
    }
}

#[cfg(all(test, unix))]
mod working_tree_hash_process_group_tests {
    use super::{GitError, GitRepo, run_isolated_graph_git};
    use std::path::Path;
    use std::time::Duration;

    fn git(root: &Path, args: &[&str]) {
        run_isolated_graph_git(root, args)
            .unwrap_or_else(|error| panic!("git {}: {error}", args.join(" ")));
    }

    fn repository() -> (tempfile::TempDir, GitRepo) {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(
            root.path(),
            &["config", "user.name", "Tree hash group test"],
        );
        git(
            root.path(),
            &["config", "user.email", "tree-hash-group@example.invalid"],
        );
        std::fs::write(root.path().join("tracked.txt"), "baseline\n").unwrap();
        git(root.path(), &["add", "tracked.txt"]);
        git(root.path(), &["commit", "-qm", "initial"]);
        let repo = GitRepo::discover(root.path()).unwrap();
        (root, repo)
    }

    fn configure_clean_filter(root: &Path, script: &str) {
        std::fs::write(root.join(".git/clean-filter.sh"), script).unwrap();
        git(
            root,
            &[
                "config",
                "filter.aethyme-test.clean",
                "sh .git/clean-filter.sh",
            ],
        );
        git(root, &["config", "filter.aethyme-test.required", "true"]);
        std::fs::write(
            root.join(".gitattributes"),
            "tracked.txt filter=aethyme-test\n",
        )
        .unwrap();
        std::fs::write(root.join("tracked.txt"), "changed content\n").unwrap();
    }

    #[test]
    fn working_tree_hash_waits_out_a_clean_filter_background_writer() {
        let (root, repo) = repository();
        configure_clean_filter(
            root.path(),
            "#!/bin/sh\ncat >/dev/null\n(sleep 0.5; printf leaked > .git/clean-filter-descendant-ran) </dev/null >/dev/null 2>&1 &\nprintf normalized\n",
        );

        assert!(!repo.working_tree_hash().unwrap().is_empty());
        std::thread::sleep(Duration::from_millis(650));
        assert!(
            !root
                .path()
                .join(".git/clean-filter-descendant-ran")
                .exists(),
            "tree hashing must prove its owned Git process group is quiescent"
        );
    }

    #[test]
    fn working_tree_hash_preserves_a_completed_nonzero_git_error() {
        let (root, repo) = repository();
        configure_clean_filter(
            root.path(),
            "#!/bin/sh\ncat >/dev/null\nprintf filter-failed >&2\nexit 23\n",
        );

        let error = repo.working_tree_hash().unwrap_err();

        assert!(
            matches!(error, GitError::Git { ref args, .. } if args.contains("add")),
            "a completed nonzero Git child keeps the ordinary Git error: {error:?}"
        );
    }
}

#[cfg(test)]
mod config_tree_record_tests {
    use super::*;

    #[test]
    fn checked_tree_records_accept_only_empty_output_or_nonempty_nul_records() {
        let args = ["ls-tree", "-z"];
        assert!(
            GitRepo::strict_config_tree_records(b"", &args)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            GitRepo::strict_config_tree_records(b"entry\0", &args).unwrap(),
            vec![b"entry".as_slice()]
        );

        for malformed in [b"\0".as_slice(), b"entry\0\0", b"\0entry\0"] {
            assert!(
                matches!(
                    GitRepo::strict_config_tree_records(malformed, &args),
                    Err(GitError::UntrustedOutput { .. })
                ),
                "malformed output {malformed:?} must not mean absence"
            );
        }
        assert!(matches!(
            GitRepo::strict_config_tree_records(b"unterminated", &args),
            Err(GitError::UntrustedOutput { .. })
        ));
    }
}

#[cfg(test)]
mod sparse_worktree_tests {
    use super::{GitError, GitRepo};
    use std::path::{Path, PathBuf};

    fn git(root: &Path, args: &[&str]) {
        super::run_isolated_graph_git(root, args)
            .unwrap_or_else(|error| panic!("git {}: {error}", args.join(" ")));
    }

    fn repository() -> (tempfile::TempDir, std::path::PathBuf) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "--template=", "-b", "main"]);
        git(&root, &["config", "user.name", "Sparse checkout test"]);
        git(
            &root,
            &["config", "user.email", "sparse-checkout@example.invalid"],
        );
        std::fs::write(root.join("README.md"), "root file").unwrap();
        for (relative, contents) in [
            ("included/source.rs", "selected source"),
            ("omitted/deep/data.txt", "omitted source proof"),
        ] {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "initial"]);
        (fixture, root)
    }

    fn add_git_exclude(root: &Path, pattern: &str) {
        let exclude = PathBuf::from(
            super::run_git(
                root,
                &[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "info/exclude",
                ],
            )
            .unwrap(),
        );
        std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        let mut ignore_rules = std::fs::read(&exclude).unwrap_or_default();
        ignore_rules.extend_from_slice(b"\n");
        ignore_rules.extend_from_slice(pattern.as_bytes());
        ignore_rules.extend_from_slice(b"\n");
        std::fs::write(exclude, ignore_rules).unwrap();
    }

    #[test]
    fn checkout_worker_default_respects_config_and_cpu_limits() {
        assert_eq!(
            super::checkout_worker_default_from_config(
                Err(GitError::Git {
                    args: "config".into(),
                    stderr: String::new(),
                }),
                8
            ),
            Some("checkout.workers=8")
        );
        assert_eq!(
            super::checkout_worker_default_from_config(
                Err(GitError::Git {
                    args: "config".into(),
                    stderr: String::new(),
                }),
                4
            ),
            Some("checkout.workers=4")
        );
        assert_eq!(
            super::checkout_worker_default_from_config(
                Err(GitError::Git {
                    args: "config".into(),
                    stderr: String::new(),
                }),
                2
            ),
            None
        );
        assert_eq!(
            super::checkout_worker_default_from_config(
                Err(GitError::Git {
                    args: "config".into(),
                    stderr: "invalid configuration".into(),
                }),
                8
            ),
            None
        );
        let (_fixture, root) = repository();
        let repo = GitRepo::discover(&root).unwrap();
        // Explicit local settings win regardless of user-global config.
        git(&root, &["config", "checkout.workers", "1"]);
        assert_eq!(repo.checkout_worker_default(8), None);
        assert_eq!(repo.checkout_worker_default(2), None);
        git(&root, &["config", "checkout.workers", "invalid"]);
        assert_eq!(repo.checkout_worker_default(8), None);
    }

    #[test]
    fn full_worktree_checkout_preserves_contents_and_config() {
        let (fixture, root) = repository();
        git(&root, &["config", "checkout.workers", "1"]);
        let repo = GitRepo::discover(&root).unwrap();
        let base = repo.head_commit().unwrap();
        let path = fixture.path().join("linked/full");
        let checkout = repo.worktree_add(&path, "agent/full", &base).unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join("included/source.rs")).unwrap(),
            "selected source"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("omitted/deep/data.txt")).unwrap(),
            "omitted source proof"
        );
        assert!(checkout.dirty_paths().unwrap().is_empty());
        assert_eq!(
            super::run_git(&root, &["config", "--get", "checkout.workers"]).unwrap(),
            "1"
        );
        repo.worktree_remove(&path, false).unwrap();
    }

    #[test]
    fn sparse_checkout_keeps_root_files_and_omissions_clean_for_git_status() {
        let (fixture, root) = repository();
        let repo = GitRepo::discover(&root).unwrap();
        let base = repo.head_commit().unwrap();
        let worktree_path = fixture.path().join("linked/sparse");
        let (worktree, fallback) = repo
            .worktree_add_with_sparse_paths(
                &worktree_path,
                "agent/sparse",
                &base,
                &["included".to_string()],
            )
            .unwrap();

        assert!(fallback.is_none(), "sparse mode unexpectedly fell back");
        assert!(worktree_path.join("README.md").is_file());
        assert!(worktree_path.join("included/source.rs").is_file());
        assert!(!worktree_path.join("omitted/deep/data.txt").exists());
        assert!(repo.require_complete_gate_checkout().is_ok());
        let error = worktree
            .require_complete_gate_checkout()
            .unwrap_err()
            .to_string();
        assert!(error.contains("omitted/deep/data.txt"), "{error}");
        assert!(error.contains("complete checkout"), "{error}");
        assert!(worktree.dirty_paths().unwrap().is_empty());
        assert!(
            root.join("omitted/deep/data.txt").is_file(),
            "sparse checkout must not omit files from the primary checkout"
        );
        assert!(repo.dirty_paths().unwrap().is_empty());
        assert_eq!(
            super::run_git(
                &worktree_path,
                &["config", "--bool", "--worktree", "core.sparseCheckout"]
            )
            .unwrap()
            .trim(),
            "true",
            "the sparse setting belongs to the linked worktree"
        );
        assert!(
            matches!(
                super::run_git(
                    &root,
                    &["config", "--bool", "--worktree", "core.sparseCheckout"]
                ),
                Err(GitError::Git { .. })
            ),
            "the primary checkout must not inherit the linked worktree's sparse setting"
        );
        assert_eq!(
            worktree
                .file_at_commit(&base, "omitted/deep/data.txt")
                .unwrap()
                .as_deref(),
            Some("omitted source proof"),
            "commit-backed source reads must still see sparse-omitted files"
        );

        std::fs::write(worktree_path.join("included/source.rs"), "edited").unwrap();
        assert_eq!(
            worktree.dirty_paths().unwrap(),
            vec!["included/source.rs"],
            "sparse omissions must not become deleted paths"
        );
        git(&worktree_path, &["checkout", "--", "included/source.rs"]);
        assert!(worktree.dirty_paths().unwrap().is_empty());
        repo.worktree_remove(&worktree_path, false).unwrap();
    }

    #[test]
    fn sparse_setup_failure_restores_a_complete_full_checkout() {
        let (fixture, root) = repository();
        let repo = GitRepo::discover(&root).unwrap();
        let base = repo.head_commit().unwrap();
        let worktree_path = fixture.path().join("linked/fallback");
        let (worktree, fallback) = repo
            .worktree_add_with_sparse_paths_using(
                &worktree_path,
                "agent/fallback",
                &base,
                &["included".to_string()],
                |worktree, _| {
                    git(
                        worktree,
                        &[
                            "sparse-checkout",
                            "set",
                            "--cone",
                            "--no-sparse-index",
                            "--",
                            "included",
                        ],
                    );
                    Err(GitError::Git {
                        args: "sparse-checkout set".into(),
                        stderr: "injected failure".into(),
                    })
                },
            )
            .unwrap();

        assert!(
            fallback
                .as_deref()
                .is_some_and(|reason| reason.contains("injected failure"))
        );
        assert!(worktree_path.join("README.md").is_file());
        assert!(worktree_path.join("included/source.rs").is_file());
        assert!(
            worktree_path.join("omitted/deep/data.txt").is_file(),
            "full-checkout recovery left a tracked file omitted"
        );
        assert!(worktree.dirty_paths().unwrap().is_empty());
        assert_eq!(
            super::run_git(
                &worktree_path,
                &["config", "--bool", "--worktree", "core.sparseCheckout"]
            )
            .unwrap()
            .trim(),
            "false",
            "full fallback must disable sparse mode only in the linked worktree"
        );
        assert!(
            matches!(
                super::run_git(&root, &["config", "--bool", "--get", "core.sparseCheckout"]),
                Err(GitError::Git { .. })
            ),
            "full fallback must not write sparse settings into shared config"
        );
        repo.worktree_remove(&worktree_path, false).unwrap();
    }

    #[test]
    fn sparse_failure_after_materialization_restores_a_complete_full_checkout() {
        let (fixture, root) = repository();
        let repo = GitRepo::discover(&root).unwrap();
        let base = repo.head_commit().unwrap();
        let worktree_path = fixture.path().join("linked/materialized-fallback");
        let (worktree, fallback) = repo
            .worktree_add_with_sparse_paths_using(
                &worktree_path,
                "agent/materialized-fallback",
                &base,
                &["included".to_string()],
                |worktree, _| {
                    git(
                        worktree,
                        &[
                            "sparse-checkout",
                            "set",
                            "--cone",
                            "--no-sparse-index",
                            "--",
                            "included",
                        ],
                    );
                    repo.materialize_fresh_sparse_worktree(
                        worktree,
                        "agent/materialized-fallback",
                        &base,
                    )?;
                    Err(GitError::Git {
                        args: "sparse-checkout set".into(),
                        stderr: "injected failure after sparse materialization".into(),
                    })
                },
            )
            .unwrap();

        assert!(
            fallback
                .as_deref()
                .is_some_and(|reason| reason.contains("injected failure"))
        );
        assert!(worktree_path.join("README.md").is_file());
        assert!(worktree_path.join("included/source.rs").is_file());
        assert!(worktree_path.join("omitted/deep/data.txt").is_file());
        assert!(worktree.dirty_paths().unwrap().is_empty());
        assert_eq!(
            super::run_git(
                &worktree_path,
                &["config", "--bool", "--worktree", "core.sparseCheckout"]
            )
            .unwrap()
            .trim(),
            "false"
        );
        repo.worktree_remove(&worktree_path, false).unwrap();
    }

    #[test]
    fn sparse_failure_before_set_preserves_shared_config_and_recovers_full_checkout() {
        let (fixture, root) = repository();
        git(&root, &["config", "extensions.worktreeConfig", "false"]);
        git(&root, &["config", "core.sparseCheckout", "false"]);
        let repo = GitRepo::discover(&root).unwrap();
        let base = repo.head_commit().unwrap();
        let worktree_path = fixture.path().join("linked/before-set-fallback");
        let (worktree, fallback) = repo
            .worktree_add_with_sparse_paths_using(
                &worktree_path,
                "agent/before-set-fallback",
                &base,
                &["included".to_string()],
                |_, _| {
                    Err(GitError::Git {
                        args: "sparse-checkout set".into(),
                        stderr: "injected failure before sparse configuration".into(),
                    })
                },
            )
            .unwrap();

        assert!(
            fallback
                .as_deref()
                .is_some_and(|reason| reason.contains("injected failure"))
        );
        assert!(worktree_path.join("README.md").is_file());
        assert!(worktree_path.join("omitted/deep/data.txt").is_file());
        assert!(worktree.dirty_paths().unwrap().is_empty());
        assert_eq!(
            worktree.current_branch().unwrap(),
            "agent/before-set-fallback"
        );
        assert_eq!(worktree.head_commit().unwrap(), base);
        assert_eq!(
            super::run_git(
                &root,
                &["config", "--bool", "--get", "extensions.worktreeConfig"]
            )
            .unwrap()
            .trim(),
            "false",
            "failure before sparse setup must not enable worktree-specific config"
        );
        assert_eq!(
            super::run_git(&root, &["config", "--bool", "--get", "core.sparseCheckout"])
                .unwrap()
                .trim(),
            "false",
            "failure before sparse setup must preserve the common checkout setting"
        );
        assert!(matches!(
            super::run_git(
                &worktree_path,
                &["config", "--bool", "--worktree", "core.sparseCheckout"]
            ),
            Err(GitError::Git { .. })
        ));
        repo.worktree_remove(&worktree_path, false).unwrap();
    }

    #[test]
    fn sparse_recovery_retains_tracked_and_untracked_edits() {
        let (fixture, root) = repository();
        let repo = GitRepo::discover(&root).unwrap();
        let base = repo.head_commit().unwrap();
        let worktree_path = fixture.path().join("linked/dirty-fallback");
        let error = match repo.worktree_add_with_sparse_paths_using(
            &worktree_path,
            "agent/dirty-fallback",
            &base,
            &["included".to_string()],
            |worktree, _| {
                git(
                    worktree,
                    &[
                        "sparse-checkout",
                        "set",
                        "--cone",
                        "--no-sparse-index",
                        "--",
                        "included",
                    ],
                );
                repo.materialize_fresh_sparse_worktree(worktree, "agent/dirty-fallback", &base)?;
                std::fs::write(
                    worktree.join("included/source.rs"),
                    "tracked recovery sentinel",
                )
                .unwrap();
                std::fs::write(
                    worktree.join("included/untracked-recovery-sentinel.txt"),
                    "untracked recovery sentinel",
                )
                .unwrap();
                add_git_exclude(worktree, "included/ignored-recovery-sentinel.txt");
                std::fs::write(
                    worktree.join("included/ignored-recovery-sentinel.txt"),
                    "ignored recovery sentinel",
                )
                .unwrap();
                Err(GitError::Git {
                    args: "sparse-checkout set".into(),
                    stderr: "injected failure after edits".into(),
                })
            },
        ) {
            Ok(_) => panic!("dirty sparse recovery unexpectedly succeeded"),
            Err(error) => error,
        };

        let error_text = error.to_string();
        assert!(
            error_text.contains("partial worktree retained"),
            "{error_text}"
        );
        let tracked = worktree_path.join("included/source.rs");
        assert_eq!(
            std::fs::read_to_string(tracked).unwrap(),
            "tracked recovery sentinel"
        );
        let untracked = worktree_path.join("included/untracked-recovery-sentinel.txt");
        assert_eq!(
            std::fs::read_to_string(untracked).unwrap(),
            "untracked recovery sentinel"
        );
        let ignored = worktree_path.join("included/ignored-recovery-sentinel.txt");
        assert_eq!(
            std::fs::read_to_string(ignored).unwrap(),
            "ignored recovery sentinel"
        );
        let retained = GitRepo::discover(&worktree_path).unwrap();
        assert_eq!(retained.current_branch().unwrap(), "agent/dirty-fallback");
        assert_eq!(retained.head_commit().unwrap(), base);
    }

    #[test]
    fn sparse_recovery_retains_an_ignored_file_created_after_the_empty_index_proof() {
        let (fixture, root) = repository();
        let repo = GitRepo::discover(&root).unwrap();
        let base = repo.head_commit().unwrap();
        let worktree_path = fixture.path().join("linked/ignored-fallback");
        let sentinel = worktree_path.join("omitted/deep/ignored-recovery-sentinel.txt");
        let error = match repo.worktree_add_with_sparse_paths_using(
            &worktree_path,
            "agent/ignored-fallback",
            &base,
            &["included".to_string()],
            |worktree, _| {
                add_git_exclude(worktree, "omitted/deep/ignored-recovery-sentinel.txt");
                std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
                std::fs::write(&sentinel, "ignored recovery sentinel").unwrap();
                git(
                    worktree,
                    &[
                        "check-ignore",
                        "--quiet",
                        "--no-index",
                        "--",
                        "omitted/deep/ignored-recovery-sentinel.txt",
                    ],
                );
                git(
                    worktree,
                    &[
                        "sparse-checkout",
                        "set",
                        "--cone",
                        "--no-sparse-index",
                        "--",
                        "included",
                    ],
                );
                Err(GitError::Git {
                    args: "sparse-checkout set".into(),
                    stderr: "injected failure after ignored file raced sparse setup".into(),
                })
            },
        ) {
            Ok(_) => panic!("ignored-file sparse recovery unexpectedly succeeded"),
            Err(error) => error,
        };

        let error_text = error.to_string();
        assert!(
            error_text.contains("partial worktree retained"),
            "{error_text}"
        );
        assert_eq!(
            std::fs::read_to_string(sentinel).unwrap(),
            "ignored recovery sentinel"
        );
        let retained = GitRepo::discover(&worktree_path).unwrap();
        assert_eq!(retained.current_branch().unwrap(), "agent/ignored-fallback");
        assert_eq!(retained.head_commit().unwrap(), base);
    }

    #[test]
    fn sparse_recovery_retains_a_worktree_whose_head_moved() {
        let (fixture, root) = repository();
        let repo = GitRepo::discover(&root).unwrap();
        let base = repo.head_commit().unwrap();
        let worktree_path = fixture.path().join("linked/moved-head");
        let error = match repo.worktree_add_with_sparse_paths_using(
            &worktree_path,
            "agent/moved-head",
            &base,
            &["included".to_string()],
            |worktree, _| {
                git(
                    worktree,
                    &[
                        "sparse-checkout",
                        "set",
                        "--cone",
                        "--no-sparse-index",
                        "--",
                        "included",
                    ],
                );
                repo.materialize_fresh_sparse_worktree(worktree, "agent/moved-head", &base)?;
                std::fs::write(
                    worktree.join("included/source.rs"),
                    "moved head recovery sentinel",
                )
                .unwrap();
                git(worktree, &["add", "included/source.rs"]);
                git(worktree, &["commit", "-qm", "move sparse recovery head"]);
                Err(GitError::Git {
                    args: "sparse-checkout set".into(),
                    stderr: "injected failure after head move".into(),
                })
            },
        ) {
            Ok(_) => panic!("moved-head sparse recovery unexpectedly succeeded"),
            Err(error) => error,
        };

        let error_text = error.to_string();
        assert!(
            error_text.contains("partial worktree retained"),
            "{error_text}"
        );
        let retained = GitRepo::discover(&worktree_path).unwrap();
        assert_eq!(retained.current_branch().unwrap(), "agent/moved-head");
        assert_ne!(retained.head_commit().unwrap(), base);
        assert_eq!(
            std::fs::read_to_string(worktree_path.join("included/source.rs")).unwrap(),
            "moved head recovery sentinel"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sparse_config_directory_validation_uses_tree_objects_without_following_symlinks() {
        let (_fixture, root) = repository();
        std::os::unix::fs::symlink("included", root.join("alias")).unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "add directory symlink"]);
        let repo = GitRepo::discover(&root).unwrap();
        let base = repo.head_commit().unwrap();
        assert!(repo.commit_has_directory(&base, "included").unwrap());
        assert!(!repo.commit_has_directory(&base, "alias").unwrap());
    }
}

/// The per-user probe cache as the resolver uses it: what a hit skips, what
/// forces a probe, and what is never recorded. Every test names its own cache
/// file; the process default is disabled under test.
#[cfg(all(test, unix))]
mod git_probe_cache_resolution_tests {
    use super::*;
    use crate::git_probe_cache::{
        self as probe_cache, CacheLocation, CachedVerdict, Lookup, MissReason,
    };
    use std::os::unix::fs::PermissionsExt as _;

    const PATH_ENV: &str = "/usr/bin:/bin";

    fn trustworthy_git() -> Option<PathBuf> {
        match git_output_trust() {
            GitOutputTrust::Undecorated { resolved, .. } => Some(resolved.clone()),
            _ => None,
        }
    }

    /// Write `body` as an executable `git` in a fresh directory under `root`,
    /// through a rename so the final path never has an open write handle.
    fn install(root: &Path, dir: &str, body: &str) -> PathBuf {
        let dir = root.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let staged = dir.join(".git-staged");
        std::fs::write(&staged, body).unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.join("git");
        std::fs::rename(&staged, &path).unwrap();
        path
    }

    /// A faithful wrapper that appends a line to `counter` per Git spawn.
    fn counting(root: &Path, dir: &str, real: &Path, counter: &Path) -> PathBuf {
        install(
            root,
            dir,
            &format!(
                "#!/bin/sh\necho spawn >> \"{}\"\nexec \"{}\" \"$@\"\n",
                counter.display(),
                real.display()
            ),
        )
    }

    /// Counts spawns, and lies about `diff --quiet` while `mode` says `lie`:
    /// the residual a key cannot see, because the wrapper file never changes.
    fn moody(root: &Path, real: &Path, counter: &Path, mode: &Path) -> PathBuf {
        install(
            root,
            "moody",
            &format!(
                r#"#!/bin/sh
echo spawn >> "{counter}"
"{real}" "$@"
status=$?
if [ "$(cat "{mode}" 2>/dev/null)" = "lie" ]; then
    for arg in "$@"; do
        if [ "$arg" = "--quiet" ]; then exit 0; fi
    done
fi
exit $status
"#,
                counter = counter.display(),
                real = real.display(),
                mode = mode.display()
            ),
        )
    }

    fn spawns(counter: &Path) -> usize {
        std::fs::read_to_string(counter)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    /// `porcelain_probe`, retrying only Linux's ETXTBSY from a sibling test's
    /// fork holding a write handle (see `git_program_tests::probe`).
    fn probe(path: &Path) -> PorcelainProbe {
        for _ in 0..50 {
            let result = porcelain_probe(path);
            if !matches!(&result, PorcelainProbe::Unusable { reason } if reason.contains("Text file busy"))
            {
                return result;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        porcelain_probe(path)
    }

    fn resolve(candidates: &[PathBuf], cache: &CacheLocation, fresh: bool) -> GitProgram {
        resolve_git_program_with(
            candidates,
            Some(std::ffi::OsStr::new(PATH_ENV)),
            cache,
            fresh,
            probe,
        )
    }

    fn resolved(program: &GitProgram) -> Option<&Path> {
        match &program.trust {
            GitOutputTrust::Undecorated { resolved, .. } => Some(resolved),
            _ => None,
        }
    }

    fn key(path: &Path) -> probe_cache::ProgramKey {
        probe_cache::program_key(path, Some(std::ffi::OsStr::new(PATH_ENV))).unwrap()
    }

    struct Fixture {
        _root: tempfile::TempDir,
        root: PathBuf,
        real: PathBuf,
        cache: CacheLocation,
    }

    fn fixture() -> Option<Fixture> {
        let Some(real) = trustworthy_git() else {
            eprintln!("every git on PATH is wrapped; nothing honest to wrap");
            return None;
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().to_path_buf();
        Some(Fixture {
            cache: CacheLocation::File(path.join("cache/git-probe/verdicts.json")),
            _root: root,
            root: path,
            real,
        })
    }

    #[test]
    fn a_cache_hit_spawns_no_git_and_fresh_mode_probes_anyway() {
        let Some(f) = fixture() else { return };
        let counter = f.root.join("count");
        let git = counting(&f.root, "a", &f.real, &counter);

        let first = resolve(std::slice::from_ref(&git), &f.cache, false);
        assert_eq!(resolved(&first), Some(git.as_path()));
        assert!(!first.verdict_from_cache);
        let probed = spawns(&counter);
        assert!(
            probed >= 8,
            "the probe runs about ten git commands, saw {probed}"
        );

        let second = resolve(std::slice::from_ref(&git), &f.cache, false);
        assert_eq!(resolved(&second), Some(git.as_path()));
        assert!(second.verdict_from_cache);
        assert_eq!(spawns(&counter), probed, "a hit must not spawn git");

        // Destructive commands: never answered from the cache.
        let forced = resolve(std::slice::from_ref(&git), &f.cache, true);
        assert!(!forced.verdict_from_cache);
        assert_eq!(spawns(&counter), probed * 2);
        assert_eq!(confirm_cached_verdict(&second, &forced), Ok(()));
    }

    #[test]
    fn replacing_the_binary_forces_a_probe() {
        let Some(f) = fixture() else { return };
        let counter = f.root.join("count");
        let git = counting(&f.root, "a", &f.real, &counter);
        resolve(std::slice::from_ref(&git), &f.cache, false);
        let probed = spawns(&counter);

        // Same path, new file (as a package upgrade does).
        let replaced = counting(&f.root, "a", &f.real, &counter);
        assert_eq!(replaced, git);
        assert_eq!(
            probe_cache::lookup(&f.cache, &key(&git), std::time::SystemTime::now()),
            Lookup::Miss(MissReason::KeyMismatch)
        );
        let again = resolve(std::slice::from_ref(&git), &f.cache, false);
        assert!(!again.verdict_from_cache);
        assert!(spawns(&counter) > probed);
    }

    #[test]
    fn retargeting_a_symlink_hop_forces_a_probe() {
        let Some(f) = fixture() else { return };
        let counter = f.root.join("count");
        let one = counting(&f.root, "one", &f.real, &counter);
        let two = counting(&f.root, "two", &f.real, &counter);
        let link_dir = f.root.join("link");
        std::fs::create_dir_all(&link_dir).unwrap();
        let link = link_dir.join("git");
        std::os::unix::fs::symlink(&one, &link).unwrap();
        resolve(std::slice::from_ref(&link), &f.cache, false);
        assert!(resolve(std::slice::from_ref(&link), &f.cache, false).verdict_from_cache);

        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&two, &link).unwrap();
        let before = spawns(&counter);
        let retargeted = resolve(std::slice::from_ref(&link), &f.cache, false);
        assert!(!retargeted.verdict_from_cache);
        assert!(spawns(&counter) > before);
    }

    #[test]
    fn a_different_path_winner_is_probed_not_borrowed() {
        let Some(f) = fixture() else { return };
        let counter_a = f.root.join("count-a");
        let counter_b = f.root.join("count-b");
        let a = counting(&f.root, "a", &f.real, &counter_a);
        let b = counting(&f.root, "b", &f.real, &counter_b);
        resolve(std::slice::from_ref(&a), &f.cache, false);

        let program = resolve(&[b.clone(), a.clone()], &f.cache, false);
        assert_eq!(resolved(&program), Some(b.as_path()));
        assert!(!program.verdict_from_cache);
        assert!(spawns(&counter_b) > 0, "the new winner is probed");
    }

    fn is_cached_rejection(lookup: Lookup) -> bool {
        matches!(lookup, Lookup::Hit(CachedVerdict::Rejected { .. }))
    }

    /// The saving where it was measured: a rejected shim directory sits first
    /// on PATH. Its rejection is cached, so a later process skips it without
    /// spawning anything -- fail safe, exactly as a fresh rejection skips it --
    /// and replacing the shim with an honest binary re-probes it.
    #[test]
    fn a_cached_rejection_skips_the_shim_without_spawning_and_a_new_shim_is_probed() {
        let Some(f) = fixture() else { return };
        let shim_counter = f.root.join("count-shim");
        let shim = install(
            &f.root,
            "shim",
            &format!(
                "#!/bin/sh\necho spawn >> \"{}\"\n\"{}\" \"$@\"\nstatus=$?\nprintf 'ok'\nexit $status\n",
                shim_counter.display(),
                f.real.display()
            ),
        );
        let honest_counter = f.root.join("count-honest");
        let honest = counting(&f.root, "honest", &f.real, &honest_counter);
        let candidates = [shim.clone(), honest.clone()];

        let first = resolve(&candidates, &f.cache, false);
        assert_eq!(resolved(&first), Some(honest.as_path()));
        assert!(is_cached_rejection(probe_cache::lookup(
            &f.cache,
            &key(&shim),
            std::time::SystemTime::now()
        )));
        let (shim_spawns, honest_spawns) = (spawns(&shim_counter), spawns(&honest_counter));
        assert!(shim_spawns > 0);

        let second = resolve(&candidates, &f.cache, false);
        assert_eq!(resolved(&second), Some(honest.as_path()));
        assert!(second.verdict_from_cache);
        assert_eq!(
            spawns(&shim_counter),
            shim_spawns,
            "a cached rejection spawns nothing"
        );
        assert_eq!(spawns(&honest_counter), honest_spawns);
        match &second.trust {
            GitOutputTrust::Undecorated { bypassed, .. } => assert!(
                bypassed
                    .iter()
                    .any(|entry| entry.path == shim && entry.dishonest),
                "the skipped shim stays on record for the gate PATH: {bypassed:?}"
            ),
            other => panic!("{other:?}"),
        }

        // The shim is fixed (new file, new identity): probed again, selected.
        let fixed = counting(&f.root, "shim", &f.real, &shim_counter);
        assert_eq!(fixed, shim);
        let third = resolve(&candidates, &f.cache, false);
        assert_eq!(resolved(&third), Some(shim.as_path()));
        assert!(!third.verdict_from_cache);
        assert!(spawns(&shim_counter) > shim_spawns);
    }

    /// A fresh-forced process re-probes a cached rejection and replaces the
    /// entry either way; an `Unusable` result is never recorded.
    #[test]
    fn fresh_mode_reprobes_rejections_and_unusable_is_not_recorded() {
        let Some(f) = fixture() else { return };
        let counter = f.root.join("count");
        let mode = f.root.join("mode");
        std::fs::write(&mode, "lie").unwrap();
        let git = moody(&f.root, &f.real, &counter, &mode);
        resolve(std::slice::from_ref(&git), &f.cache, false);
        assert!(is_cached_rejection(probe_cache::lookup(
            &f.cache,
            &key(&git),
            std::time::SystemTime::now()
        )));
        std::fs::write(&mode, "honest").unwrap();
        let fresh = resolve(std::slice::from_ref(&git), &f.cache, true);
        assert_eq!(resolved(&fresh), Some(git.as_path()));
        assert_eq!(
            probe_cache::lookup(&f.cache, &key(&git), std::time::SystemTime::now()),
            Lookup::Hit(CachedVerdict::Accepted)
        );

        let broken = install(&f.root, "broken", "#!/bin/sh\nexit 7\n");
        resolve(&[broken.clone(), git.clone()], &f.cache, false);
        assert_eq!(
            probe_cache::lookup(&f.cache, &key(&broken), std::time::SystemTime::now()),
            Lookup::Miss(MissReason::Absent),
            "a probe that could not run proves nothing and is not remembered"
        );
    }

    /// A binary replaced while it was being probed: the verdict describes
    /// neither file for certain, so nothing is recorded.
    #[test]
    fn a_key_that_changes_during_the_probe_is_not_recorded() {
        let Some(f) = fixture() else { return };
        let counter = f.root.join("count");
        let git = counting(&f.root, "a", &f.real, &counter);
        let root = f.root.clone();
        let real = f.real.clone();
        let program = resolve_git_program_with(
            std::slice::from_ref(&git),
            Some(std::ffi::OsStr::new(PATH_ENV)),
            &f.cache,
            false,
            |path: &Path| {
                let verdict = probe(path);
                counting(&root, "a", &real, &root.join("count-2"));
                verdict
            },
        );
        assert_eq!(resolved(&program), Some(git.as_path()));
        assert_eq!(
            probe_cache::lookup(&f.cache, &key(&git), std::time::SystemTime::now()),
            Lookup::Miss(MissReason::Absent)
        );
    }

    /// The adversarial case the cache must not weaken: the real git was
    /// accepted and cached, then a lying wrapper appears earlier on PATH. It
    /// has its own path and metadata, so it is probed and refused, and the
    /// broker keeps the cached honest git with the wrapper on record.
    #[test]
    fn a_wrapper_placed_ahead_of_a_cached_git_is_probed_and_rejected() {
        let Some(f) = fixture() else { return };
        let honest_counter = f.root.join("count-honest");
        let honest = counting(&f.root, "honest", &f.real, &honest_counter);
        resolve(std::slice::from_ref(&honest), &f.cache, false);

        let liar_counter = f.root.join("count-liar");
        let liar = install(
            &f.root,
            "liar",
            &format!(
                "#!/bin/sh\necho spawn >> \"{}\"\n\"{}\" \"$@\"\nstatus=$?\nfor arg in \"$@\"; do\n    if [ \"$arg\" = \"--quiet\" ]; then exit 0; fi\ndone\nexit $status\n",
                liar_counter.display(),
                f.real.display()
            ),
        );
        let program = resolve(&[liar.clone(), honest.clone()], &f.cache, false);
        assert!(spawns(&liar_counter) > 0, "the wrapper is probed");
        match &program.trust {
            GitOutputTrust::Undecorated { resolved, bypassed } => {
                assert_eq!(resolved, &honest);
                assert!(
                    bypassed
                        .iter()
                        .any(|entry| entry.path == liar && entry.dishonest),
                    "{bypassed:?}"
                );
            }
            other => panic!("expected the cached honest git, got {other:?}"),
        }
        assert!(is_cached_rejection(probe_cache::lookup(
            &f.cache,
            &key(&liar),
            std::time::SystemTime::now()
        )));
    }

    /// The residual, stated as a test: a wrapper that starts lying without its
    /// file changing is still a cache hit for non-destructive work, and the
    /// fresh confirmation every deletion runs refuses it and drops the entry.
    #[test]
    fn a_wrapper_that_changes_behaviour_in_place_is_refused_by_the_fresh_check() {
        let Some(f) = fixture() else { return };
        let counter = f.root.join("count");
        let mode = f.root.join("mode");
        std::fs::write(&mode, "honest").unwrap();
        let git = moody(&f.root, &f.real, &counter, &mode);
        let accepted = resolve(std::slice::from_ref(&git), &f.cache, false);
        assert!(resolved(&accepted).is_some());

        std::fs::write(&mode, "lie").unwrap();
        let cached = resolve(std::slice::from_ref(&git), &f.cache, false);
        assert!(
            cached.verdict_from_cache,
            "the key cannot see a config change"
        );

        let fresh = resolve(std::slice::from_ref(&git), &f.cache, true);
        assert!(matches!(fresh.trust, GitOutputTrust::Decorated { .. }));
        let refusal = confirm_cached_verdict(&cached, &fresh).unwrap_err();
        assert!(refusal.contains("refusing to discard work"), "{refusal}");
        assert!(
            is_cached_rejection(probe_cache::lookup(
                &f.cache,
                &key(&git),
                std::time::SystemTime::now()
            )),
            "the fresh probe replaces the stale acceptance with its rejection"
        );
    }

    #[test]
    fn a_corrupt_cache_means_probe_never_accept() {
        let Some(f) = fixture() else { return };
        let counter = f.root.join("count");
        let git = counting(&f.root, "a", &f.real, &counter);
        resolve(std::slice::from_ref(&git), &f.cache, false);
        let path = f.cache.path().unwrap().to_path_buf();

        std::fs::write(&path, b"[]").unwrap();
        let before = spawns(&counter);
        let program = resolve(std::slice::from_ref(&git), &f.cache, false);
        assert!(!program.verdict_from_cache);
        assert!(spawns(&counter) > before);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let before = spawns(&counter);
        let program = resolve(std::slice::from_ref(&git), &f.cache, false);
        assert!(
            !program.verdict_from_cache,
            "a world-writable file is ignored"
        );
        assert!(spawns(&counter) > before);
    }

    #[test]
    fn a_disabled_cache_always_probes() {
        let Some(f) = fixture() else { return };
        let counter = f.root.join("count");
        let git = counting(&f.root, "a", &f.real, &counter);
        resolve(std::slice::from_ref(&git), &CacheLocation::Disabled, false);
        let once = spawns(&counter);
        let program = resolve(std::slice::from_ref(&git), &CacheLocation::Disabled, false);
        assert!(!program.verdict_from_cache);
        assert_eq!(spawns(&counter), once * 2);
    }
}

/// Every deletion primitive forces the fresh verdict before it deletes. The
/// process cache is disabled under test, so each test installs the
/// `test_cached_verdict` seam: the thread behaves as if Git had been resolved
/// from a cache that accepted a wrapper which has since started lying in place
/// (the one change a key cannot see). Removing any primitive's fresh check
/// makes its test delete, and fail.
#[cfg(all(test, unix))]
mod fresh_verdict_before_deletion_tests {
    use super::*;
    use crate::git_probe_cache::CacheLocation;
    use std::os::unix::fs::PermissionsExt as _;

    const PATH_ENV: &str = "/usr/bin:/bin";

    struct Fixture {
        _root: tempfile::TempDir,
        root: PathBuf,
        real: PathBuf,
        git: PathBuf,
        counter: PathBuf,
        mode: PathBuf,
        cache: CacheLocation,
    }

    /// A counting wrapper the cache accepted while honest; `lie` makes it
    /// rewrite `diff --quiet` from now on without its file changing.
    fn fixture(lie: bool) -> Option<Fixture> {
        let GitOutputTrust::Undecorated { resolved: real, .. } = git_output_trust() else {
            eprintln!("every git on PATH is wrapped; nothing honest to wrap");
            return None;
        };
        let real = real.clone();
        let root_dir = tempfile::tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        let counter = root.join("count");
        let mode = root.join("mode");
        std::fs::write(&mode, "honest").unwrap();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let staged = bin.join(".staged");
        std::fs::write(
            &staged,
            format!(
                r#"#!/bin/sh
echo spawn >> "{counter}"
"{real}" "$@"
status=$?
if [ "$(cat "{mode}" 2>/dev/null)" = "lie" ]; then
    for arg in "$@"; do
        if [ "$arg" = "--quiet" ]; then exit 0; fi
    done
fi
exit $status
"#,
                counter = counter.display(),
                real = real.display(),
                mode = mode.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).unwrap();
        let git = bin.join("git");
        std::fs::rename(&staged, &git).unwrap();
        let cache = CacheLocation::File(root.join("cache/git-probe/verdicts.json"));
        let accepted = resolve_git_program_with(
            std::slice::from_ref(&git),
            Some(std::ffi::OsStr::new(PATH_ENV)),
            &cache,
            false,
            porcelain_probe,
        );
        assert!(matches!(accepted.trust, GitOutputTrust::Undecorated { .. }));
        if lie {
            std::fs::write(&mode, "lie").unwrap();
        }
        Some(Fixture {
            _root: root_dir,
            root,
            real,
            git,
            counter,
            mode,
            cache,
        })
    }

    fn under_seam<T>(f: &Fixture, body: impl FnOnce() -> T) -> (T, usize) {
        test_cached_verdict::with(vec![f.git.clone()], PATH_ENV, f.cache.clone(), body)
    }

    fn victim(f: &Fixture) -> PathBuf {
        let dir = f.root.join("victim");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested/work.txt"), b"keep").unwrap();
        dir
    }

    fn spawns(f: &Fixture) -> usize {
        std::fs::read_to_string(&f.counter)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    /// A repository with a `side` branch, built with the honest git.
    fn repository(f: &Fixture) -> (GitRepo, String) {
        let repo = f.root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new(&f.real)
                .args(args)
                .current_dir(&repo)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("a"), b"a").unwrap();
        git(&["add", "a"]);
        git(&["commit", "-q", "-m", "a"]);
        git(&["branch", "side"]);
        let head = git(&["rev-parse", "HEAD"]);
        (GitRepo { root: repo }, head)
    }

    #[test]
    fn remove_tree_refuses_a_wrapper_that_started_lying_and_removes_when_honest() {
        let Some(f) = fixture(true) else { return };
        let dir = victim(&f);
        let (outcome, confirmations) = under_seam(&f, || crate::removal::remove_tree(&dir));
        assert_eq!(confirmations, 1, "the fresh probe ran");
        assert!(!outcome.removed);
        assert!(
            outcome.error.as_deref().unwrap_or("").contains("refusing"),
            "{outcome:?}"
        );
        assert!(dir.join("nested/work.txt").exists(), "nothing was deleted");

        // The refusal replaced the acceptance with a rejection. Honest again,
        // re-accepted by a probe, then confirmed fresh at deletion time.
        std::fs::write(&f.mode, "honest").unwrap();
        resolve_git_program_with(
            std::slice::from_ref(&f.git),
            Some(std::ffi::OsStr::new(PATH_ENV)),
            &f.cache,
            true,
            porcelain_probe,
        );
        let before = spawns(&f);
        let (outcome, confirmations) = under_seam(&f, || crate::removal::remove_tree(&dir));
        assert_eq!(confirmations, 1);
        assert!(spawns(&f) > before, "the confirmation spawned the probe");
        assert!(outcome.removed, "{outcome:?}");
    }

    /// Choke point for every pinned removal: the ignored-directory sweep at
    /// broker open (B), GC journal replay of artifacts and recovery archives
    /// (C), gate caches, storage, the worktree registry and the probe sweep.
    #[test]
    fn pinned_tree_removal_refuses_on_an_unconfirmed_verdict() {
        let Some(f) = fixture(true) else { return };
        let dir = victim(&f);
        let ((condemned, observed, pinned), confirmations) = under_seam(&f, || {
            (
                crate::gc::remove_condemned_tree(&dir, None, None),
                crate::gc::remove_condemned_tree_with_progress_and_observed_file_bytes_and_guard(
                    &dir,
                    None,
                    None,
                    || Ok(()),
                ),
                crate::gc::PinnedTreeRemoval::open(&dir),
            )
        });
        assert_eq!(confirmations, 1);
        assert!(condemned.is_err());
        assert!(observed.is_err());
        assert!(pinned.is_err());
        assert!(dir.join("nested/work.txt").exists());
    }

    #[test]
    fn git_deletion_primitives_and_paths_equal_refuse_on_an_unconfirmed_verdict() {
        let Some(f) = fixture(true) else { return };
        let (repo, head) = repository(&f);
        let dir = victim(&f);
        let (results, confirmations) = under_seam(&f, || {
            (
                repo.delete_branch_ref_checked("side", &head),
                repo.worktree_remove(&dir, true),
                repo.worktree_remove_registered(&dir, true),
                repo.paths_equal("HEAD", "HEAD", &["a".to_string()]),
            )
        });
        assert_eq!(confirmations, 1);
        assert!(
            matches!(results.0, Err(GitError::UntrustedOutput { .. })),
            "{:?}",
            results.0
        );
        assert!(
            matches!(results.1, Err(GitError::UntrustedOutput { .. })),
            "{:?}",
            results.1
        );
        assert!(
            matches!(results.2, Err(GitError::UntrustedOutput { .. })),
            "{:?}",
            results.2
        );
        assert!(
            matches!(results.3, Err(GitError::UntrustedOutput { .. })),
            "{:?}",
            results.3
        );
        assert!(
            repo.resolve_ref("refs/heads/side").is_some(),
            "the branch survives"
        );
        assert!(
            dir.join("nested/work.txt").exists(),
            "the orphan fallback never ran"
        );
    }

    /// A: `reclaim apply` deletes on the plan's ignored/untracked evidence.
    #[test]
    fn reclaim_apply_refuses_before_removing_anything() {
        let Some(f) = fixture(true) else { return };
        let dir = victim(&f);
        let plan = crate::ReclaimPlan {
            digest: "test".into(),
            root: f.root.clone(),
            scope: None,
            candidates: vec![crate::reclaim::ReclaimCandidate {
                path: dir.clone(),
                bytes: Some(4),
                worktree: f.root.clone(),
                reclaimable: true,
                reason: "test".into(),
            }],
            reclaimable_bytes: 4,
            total_bytes: 4,
        };
        let (outcome, confirmations) = under_seam(&f, || crate::reclaim::apply(&plan));
        assert_eq!(confirmations, 1);
        assert!(outcome.removed.is_empty(), "{outcome:?}");
        assert!(
            outcome
                .skipped
                .iter()
                .any(|reason| reason.contains("nothing removed"))
        );
        assert!(dir.join("nested/work.txt").exists());
    }

    /// Without the seam (a fresh verdict), the primitives work as before.
    #[test]
    fn a_fresh_verdict_needs_no_confirmation() {
        let Some(f) = fixture(false) else { return };
        let dir = victim(&f);
        assert!(crate::removal::remove_tree(&dir).removed);
        assert!(ensure_fresh_git_verdict().is_ok());
    }
}
