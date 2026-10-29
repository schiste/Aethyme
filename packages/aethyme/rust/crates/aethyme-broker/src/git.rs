//! Git service layer: every git operation the broker performs, in one
//! module, shelling out to the `git` binary (no libgit2/gix until a
//! measured need appears — see docs/aethyme-local-agent-broker.md).
//!
//! No other broker code may run git directly. Keeping the surface here
//! makes the broker's git contract auditable and testable against real
//! throwaway repositories.

use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::io::Read;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_PRIVATE_INDEX_ID: AtomicU64 = AtomicU64::new(1);

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
/// scope (`limit_git_until`) nest by taking the earlier instant, and either
/// way of lifting it (`without_git_deadline`, `suspend_git_deadline`) lifts
/// both, so a mutation is never cut off by whichever scope it did not lift.
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

/// Lift the deadline for a mutation that runs inside a report. The deadline
/// exists to bound reads; killing a ref or index write midway would leave
/// the half-applied state it was never meant to produce. Each command keeps
/// its ordinary timeout, and the caller's deadline returns when the guard is
/// dropped.
pub(crate) fn suspend_git_deadline() -> GitDeadlineGuard {
    let previous = GIT_DEADLINE.with(|slot| slot.replace(None));
    GitDeadlineGuard {
        _restore: RestoreGitDeadline(previous),
    }
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

    /// git ran past its deadline and was killed (#219). Nothing about the
    /// repository is known from it; the host, a remote or a lock is wedged.
    #[error(
        "git {args} did not finish within {seconds}s and was killed \
         (AETHYME_GIT_TIMEOUT_SECS changes the general limit; caller caps may be shorter)"
    )]
    TimedOut { args: String, seconds: u64 },

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

fn git_program() -> &'static GitProgram {
    GIT_PROGRAM.get_or_init(resolve_git_program)
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
    }
    command
}

fn resolve_git_program() -> GitProgram {
    let mut dishonest: Vec<BypassedGit> = Vec::new();
    let mut unusable: Vec<BypassedGit> = Vec::new();
    for candidate in path_candidates("git") {
        match porcelain_probe(&candidate) {
            PorcelainProbe::Undecorated => {
                let mut bypassed = dishonest;
                bypassed.append(&mut unusable);
                return GitProgram {
                    program: candidate.clone(),
                    trust: GitOutputTrust::Undecorated {
                        resolved: candidate,
                        bypassed,
                    },
                };
            }
            PorcelainProbe::Decorated { bytes } => dishonest.push(BypassedGit {
                reason: format!(
                    "{} emitted {bytes} bytes of porcelain on a clean temporary repository",
                    candidate.display()
                ),
                path: candidate,
                dishonest: true,
            }),
            PorcelainProbe::StatusRewritten { detail } => dishonest.push(BypassedGit {
                reason: format!("{}: {detail}", candidate.display()),
                path: candidate,
                dishonest: true,
            }),
            // PATH routinely holds stale entries, and one that cannot run is
            // not evidence of a wrapper. Keep looking.
            PorcelainProbe::Unusable { reason } => unusable.push(BypassedGit {
                reason: format!("{}: {reason}", candidate.display()),
                path: candidate,
                dishonest: false,
            }),
        }
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
    let probe = match tempfile::tempdir() {
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
        crate::bounded_output::LimitedOutput::Deadline => Err(GitError::TimedOut {
            args: args.join(" "),
            seconds: budget.as_secs().max(1),
        }),
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

fn run_git_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let mut command = git_command();
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0");
    let output = run_git_command_output(command, args, git_timeout())?;
    Ok(output.stdout)
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
            return Err(GitError::TimedOut {
                args: DISPLAY_ARGS.into(),
                seconds: git_timeout().as_secs().max(1),
            });
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
    let budget = match active_git_deadline() {
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                note_git_timeout();
                return Err(GitError::TimedOut {
                    args: args.join(" "),
                    seconds: 1,
                });
            }
            budget.min(remaining)
        }
        None => budget,
    };
    let output = crate::bounded_output::output_within(&mut command, budget).map_err(|source| {
        GitError::Spawn {
            args: args.join(" "),
            source,
        }
    })?;
    let Some(output) = output else {
        note_git_timeout();
        return Err(GitError::TimedOut {
            args: args.join(" "),
            seconds: budget.as_secs().max(1),
        });
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
    let budget = match active_git_deadline() {
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                note_git_timeout();
                return Err(GitError::TimedOut {
                    args: args.join(" "),
                    seconds: 1,
                });
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
            Err(GitError::TimedOut {
                args: args.join(" "),
                seconds: budget.as_secs().max(1),
            })
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
        return Err(GitError::TimedOut {
            args: args.to_string(),
            seconds: deadline.saturating_duration_since(started).as_secs().max(1),
        });
    }
    Ok(deadline)
}

fn remaining_git_budget(args: &str, deadline: Instant) -> Result<Duration, GitError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        note_git_timeout();
        return Err(GitError::TimedOut {
            args: args.to_string(),
            seconds: 1,
        });
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
    started: Instant,
    deadline: Instant,
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
            GitError::TimedOut {
                args: args.to_string(),
                seconds: deadline.saturating_duration_since(started).as_secs().max(1),
            }
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

impl GitRepo {
    /// Discover the repository containing `path`.
    pub fn discover(path: &Path) -> Result<Self, GitError> {
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
        Ok(Self {
            root: PathBuf::from(root),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path of the repository's **common** git dir — the one
    /// directory every linked worktree shares. Installed git hooks live
    /// in `<common>/hooks`, which is what makes them worktree-wide.
    pub fn git_common_dir(&self) -> Result<PathBuf, GitError> {
        let common = run_git(&self.root, &["rev-parse", "--git-common-dir"])?;
        Ok(Self::absolute_common_dir(&self.root, &common))
    }

    /// Resolve a checkout's common directory without a separate root lookup.
    /// The worktree bit is required: a bare repository also has a common
    /// directory, but cannot supply the checkout evidence ownership needs.
    pub(crate) fn worktree_common_dir(path: &Path) -> Result<PathBuf, GitError> {
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
        Ok(Self::absolute_common_dir(path, common))
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
        let root_metadata = fs::symlink_metadata(&root).ok()?;
        if root_metadata.file_type().is_symlink()
            || !root_metadata.is_dir()
            || (root_metadata.dev(), root_metadata.ino()) != (worktree_device, worktree_inode)
        {
            return None;
        }

        Some(crate::events::SpawnedWorktreeInstanceWitness {
            schema_version: 2,
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
        let default_ref = self.symbolic_ref_checked("refs/remotes/origin/HEAD")?;
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
        let unexpected_direct_ref = run_git(
            &self.root,
            &[
                "for-each-ref",
                "--count=1",
                "--format=%(refname)",
                "refs/remotes/origin/HEAD",
            ],
        )?;
        if !unexpected_direct_ref.is_empty() {
            return Err(GitError::UntrustedOutput {
                detail: "refs/remotes/origin/HEAD exists but is not a symbolic default ref".into(),
            });
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
        let display_args = format!("ls-remote --refs <endpoint> {}", destinations.join(" "));
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
            exact_push_endpoint_output(&self.root, remote, &push_url, destinations, &display_args)?
        } else {
            let mut command = git_command();
            command.args(["ls-remote", "--refs", remote]);
            command.args(destinations).current_dir(&self.root);
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
        Ok(observed)
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

    /// Every commit reachable from `head`, `head` included: one `rev-list`
    /// that answers `is_ancestor(commit, head)` for any number of commits by
    /// membership.
    pub fn reachable_commits(&self, head: &str) -> Result<Vec<String>, GitError> {
        Ok(run_git(&self.root, &["rev-list", head])?
            .lines()
            .map(str::to_string)
            .collect())
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
                    return Err(GitError::TimedOut {
                        args: args.into(),
                        seconds: budget.as_secs().max(1),
                    });
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
            return Err(GitError::TimedOut {
                args: args.into(),
                seconds: deadline.saturating_duration_since(started).as_secs().max(1),
            });
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

    /// Point `name` at `commit`, creating or moving it. Used for preservation
    /// refs, where the name already encodes the commit it preserves.
    pub fn create_branch_at(&self, name: &str, commit: &str) -> Result<(), GitError> {
        run_git(&self.root, &["branch", "--force", name, commit]).map(|_| ())
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
            return Err(GitError::TimedOut {
                args: args.into(),
                seconds: deadline.saturating_duration_since(started).as_secs().max(1),
            });
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
                return Err(GitError::TimedOut {
                    args: args.into(),
                    seconds: budget.as_secs().max(1),
                });
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
            return Err(GitError::TimedOut {
                args: args.into(),
                seconds: deadline.saturating_duration_since(started).as_secs().max(1),
            });
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
        let with_parent = listed
            .iter()
            .chain(other.iter())
            .filter(|(_, has_parent)| *has_parent)
            .map(|(commit, _)| commit.clone())
            .collect::<Vec<_>>();
        let patch_ids = self.first_parent_patch_ids(&with_parent)?;
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
    pub(crate) fn cherry_marked_both(
        &self,
        left: &str,
        right: &str,
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

        let with_parent = left
            .iter()
            .chain(right.iter())
            .filter(|(_, has_parent)| *has_parent)
            .map(|(commit, _)| commit.clone())
            .collect::<Vec<_>>();
        let patch_ids = self.first_parent_patch_ids(&with_parent)?;
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
                return Err(GitError::TimedOut {
                    args: pipeline_args,
                    seconds: budget.as_secs().max(1),
                });
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
                return Err(GitError::TimedOut {
                    args: args_display,
                    seconds: budget.as_secs().max(1),
                });
            }
            crate::bounded_output::LimitedOutput::OutputLimit => {
                return Err(GitError::Git {
                    args: args_display,
                    stderr: format!(
                        "Git batch output exceeded {MAX_GIT_BATCH_OUTPUT_BYTES} bytes per stream"
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
        let mut args: Vec<String> = vec!["commit-tree".into(), tree.into()];
        for parent in parents {
            args.push("-p".into());
            args.push((*parent).into());
        }
        args.push("-m".into());
        args.push(message.into());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = git_command()
            .args(&arg_refs)
            .current_dir(&self.root)
            .env("GIT_AUTHOR_NAME", &attribution.author.name)
            .env("GIT_AUTHOR_EMAIL", &attribution.author.email)
            .env("GIT_COMMITTER_NAME", &attribution.committer.name)
            .env("GIT_COMMITTER_EMAIL", &attribution.committer.email)
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
            run_git_with_index_for_tree_hash(
                &self.root,
                &tmp_str,
                &["read-tree", "HEAD"],
                writers_quiescent,
            )?;
            // Capture tracked edits and deletions first, including anything
            // intentionally tracked beneath a runtime path. Enumerate only
            // Git-visible untracked files for the second pass, then filter
            // broker runtime paths in memory. Negative pathspecs after `.`
            // can make Git report an ignored parent directory such as
            // `.aethyme` itself, even when every ignored runtime child is
            // excluded.
            run_git_with_index_for_tree_hash(
                &self.root,
                &tmp_str,
                &["add", "-u", "--", "."],
                writers_quiescent,
            )?;
            let untracked = run_git_bytes_with_index_for_tree_hash(
                &self.root,
                &tmp_str,
                &["ls-files", "--others", "--exclude-standard", "-z"],
                writers_quiescent,
            )?;
            let pathspecs = untracked_hash_pathspecs(&untracked)?;
            if !pathspecs.is_empty() {
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
                "-b",
                branch,
                dest.to_str().unwrap_or_default(),
                base,
            ],
        )?;
        GitRepo::discover(dest)
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
    fn worktree_lock_reason(&self, worktree: &Path) -> Option<String> {
        let listing = run_git(&self.root, &["worktree", "list", "--porcelain"]).ok()?;
        let wanted = worktree
            .canonicalize()
            .unwrap_or_else(|_| worktree.to_path_buf());
        let mut current: Option<PathBuf> = None;
        for line in listing.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                let path = PathBuf::from(path);
                current = Some(path.canonicalize().unwrap_or(path));
            } else if current.as_deref() == Some(wanted.as_path()) {
                if line == "locked" {
                    return Some(String::new());
                }
                if let Some(reason) = line.strip_prefix("locked ") {
                    return Some(reason.to_string());
                }
            }
        }
        None
    }

    pub fn worktree_remove(&self, worktree: &Path, force: bool) -> Result<(), GitError> {
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
        // Environments are destroyed before their worktree goes: their
        // destroy hooks usually live in it (#1211). A worktree whose
        // environments could not be destroyed is kept, with the reason.
        if let Err(reason) = crate::environments::lifecycle::destroy_for_worktree(worktree) {
            return Err(GitError::Git {
                args: format!("worktree remove {}", worktree.display()),
                stderr: reason,
            });
        }
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
        run_git(&self.root, &args)?;
        Ok(())
    }

    /// Delete an exact local branch only when it still names the expected
    /// object. Used to roll back a just-created session worktree if its
    /// atomic database registration loses a planned-lease race.
    pub fn delete_branch_ref_checked(&self, branch: &str, expected: &str) -> Result<(), GitError> {
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
        let listing = run_git_bytes(&self.root, &["worktree", "list", "--porcelain", "-z"])?;
        parse_worktree_porcelain_z(&listing)
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

fn ensure_bounded_git_listing_before(deadline: Instant, args: &[&str]) -> Result<(), GitError> {
    if Instant::now() >= deadline {
        note_git_timeout();
        Err(GitError::TimedOut {
            args: args.join(" "),
            seconds: 1,
        })
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
        return Err(GitError::TimedOut {
            args: args.join(" "),
            seconds: 1,
        });
    }
    let budget = remaining.min(git_timeout());
    match crate::bounded_output::output_limited(command, budget, max_output_bytes).map_err(
        |source| GitError::Spawn {
            args: args.join(" "),
            source,
        },
    )? {
        crate::bounded_output::LimitedOutput::Completed(output) => {
            if Instant::now() >= effective_deadline {
                note_git_timeout();
                Err(GitError::TimedOut {
                    args: args.join(" "),
                    seconds: 1,
                })
            } else {
                Ok(output)
            }
        }
        crate::bounded_output::LimitedOutput::Deadline => {
            note_git_timeout();
            Err(GitError::TimedOut {
                args: args.join(" "),
                seconds: budget.as_secs().max(1),
            })
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
        use std::os::unix::ffi::OsStrExt;

        let root = repository();
        std::fs::write(root.path().join(".gitignore"), "bad-*\n").unwrap();
        git(root.path(), &["add", ".gitignore"]);
        git(root.path(), &["commit", "-qm", "ignore malformed filename"]);
        let invalid_name = std::ffi::OsStr::from_bytes(b"bad-\xff");
        std::fs::write(root.path().join(invalid_name), b"ignored").unwrap();

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

#[cfg(test)]
mod commit_count_tests {
    use super::{GitError, GitRepo, parse_commit_count, run_git};

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
        assert_eq!(repo.patch_unique_commit_count(&base, "HEAD").unwrap(), 1);
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
        let policy = crate::AutoCleanupPolicy::default();
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
    use std::os::unix::ffi::OsStringExt;
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
        let name = std::ffi::OsString::from_vec(b"ignored-\xff".to_vec());
        std::fs::write(root.path().join(name), "data").unwrap();

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
        assert!(error.to_string().contains("did not finish within"));
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
            {
                let _write = suspend_git_deadline();
                assert_eq!(active_git_deadline(), None);
                assert_eq!(git_timeout(), general);
            }
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
        assert_eq!(logs, 1, "all candidate patches share one git log stream");
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
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("base.txt"), "base\n").expect("write base");
        run(root, &["add", "-A"]);
        run(root, &["commit", "-qm", "base"]);
        let base = run(root, &["rev-parse", "HEAD"]);

        run(root, &["switch", "-q", "-c", "session"]);
        let path = OsString::from_vec(b"invalid-\xff-name.txt".to_vec());
        std::fs::write(root.join(path), "tracked\n").expect("write invalid UTF-8 path");
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
